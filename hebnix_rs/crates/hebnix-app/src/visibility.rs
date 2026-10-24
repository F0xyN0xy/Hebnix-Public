//! Admit one visibility request, then wait for the native/render/shell barriers.

use std::sync::Mutex;
use std::time::Duration;

use eframe::egui::{Context, ViewportId};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetLayeredWindowAttributes, GetWindowLongW, IsIconic, IsWindowVisible,
    LAYERED_WINDOW_ATTRIBUTES_FLAGS, LWA_ALPHA, SetWindowLongW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pending {
    id: u64,
    hidden: bool,
    started_frame: u64,
    drawn_frame: Option<u64>,
    applied: bool,
    taskbar: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Queued,
    Changing(Pending),
}

struct Gate {
    phase: Phase,
    generation: u64,
}

impl Gate {
    const fn new() -> Self {
        Self {
            phase: Phase::Idle,
            generation: 0,
        }
    }

    fn queue(&mut self) -> bool {
        if self.phase != Phase::Idle {
            return false;
        }
        self.phase = Phase::Queued;
        true
    }

    fn begin(&mut self, hidden: bool, frame: u64) -> bool {
        if matches!(self.phase, Phase::Changing(_)) {
            return false;
        }
        self.generation += 1;
        self.phase = Phase::Changing(Pending {
            id: self.generation,
            hidden,
            started_frame: frame,
            drawn_frame: None,
            applied: false,
            taskbar: None,
        });
        true
    }

    fn cancel_queued(&mut self) {
        if self.phase == Phase::Queued {
            self.phase = Phase::Idle;
        }
    }

    fn candidate(&self, frame: u64) -> Option<Pending> {
        let Phase::Changing(pending) = self.phase else {
            return None;
        };
        (pending.applied
            && frame > pending.started_frame
            && (pending.hidden || pending.drawn_frame.is_some_and(|drawn| frame > drawn)))
        .then_some(pending)
    }

    fn complete(
        &mut self,
        pending: Pending,
        taskbar_done: u64,
        native: bool,
        composed: bool,
    ) -> bool {
        if self.phase != Phase::Changing(pending)
            || !native
            || !composed
            || !pending
                .taskbar
                .is_some_and(|request| taskbar_done >= request)
        {
            return false;
        }
        self.phase = Phase::Idle;
        true
    }
}

static GATE: Mutex<Gate> = Mutex::new(Gate::new());

pub fn queue_toggle() -> bool {
    GATE.try_lock().is_ok_and(|mut gate| gate.queue())
}

pub fn cancel_queued_toggle() {
    GATE.lock().unwrap().cancel_queued();
}

pub fn begin_change(ctx: &Context, hidden: bool) -> bool {
    GATE.lock()
        .unwrap()
        .begin(hidden, ctx.cumulative_frame_nr_for(ViewportId::ROOT))
}

pub fn applied(ctx: &Context) {
    let id = {
        let mut gate = GATE.lock().unwrap();
        let Phase::Changing(pending) = &mut gate.phase else {
            return;
        };
        pending.applied = true;
        pending.id
    };
    crate::window_state::wake_ui(ctx);
    let ctx = ctx.clone();
    // One small checker per accepted transition; a burst cannot spawn more.
    // This is a retry cadence, not a time-based unlock/debounce.
    std::thread::Builder::new()
        .name("visibility-completion".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(16));
                let current = GATE.lock().unwrap().phase;
                if !matches!(current, Phase::Changing(pending) if pending.id == id) {
                    break;
                }
                crate::window_state::wake_ui(&ctx);
            }
        })
        .expect("could not start visibility completion checker");
}

pub fn note_ui_frame(ctx: &Context) {
    let mut gate = GATE.lock().unwrap();
    if let Phase::Changing(pending) = &mut gate.phase {
        if !pending.hidden && pending.drawn_frame.is_none() {
            pending.drawn_frame = Some(ctx.cumulative_frame_nr_for(ViewportId::ROOT));
        }
    }
}

fn native_matches(hidden: bool) -> bool {
    let Some(hwnd) = crate::window_state::main_window() else {
        return false;
    };
    unsafe {
        let style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let away = WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0;
        if style & away != if hidden { away } else { 0 } {
            return false;
        }
        if (style & WS_EX_TRANSPARENT.0 != 0) != hidden {
            return false;
        }
        if !hidden && (!IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool()) {
            return false;
        }
        let alpha = if style & WS_EX_LAYERED.0 == 0 {
            255
        } else {
            let mut alpha = 255;
            let mut flags = LAYERED_WINDOW_ATTRIBUTES_FLAGS::default();
            if GetLayeredWindowAttributes(hwnd, None, Some(&mut alpha), Some(&mut flags)).is_err() {
                return false;
            }
            if flags.contains(LWA_ALPHA) {
                alpha
            } else {
                255
            }
        };
        alpha == if hidden { 0 } else { 255 }
    }
}

/// Called before consuming controls in each root logic frame. A newer frame
/// guarantees the previous viewport commands and render submission have run.
pub fn poll(ctx: &Context) {
    let frame = ctx.cumulative_frame_nr_for(ViewportId::ROOT);
    let Some(mut pending) = GATE.lock().unwrap().candidate(frame) else {
        return;
    };
    if let Some(hwnd) = crate::window_state::main_window() {
        unsafe {
            // Winit's deferred hit-test command rewrites extended styles.
            // Restore our no-activate/Alt-Tab policy after that command ran.
            let style = GetWindowLongW(hwnd, GWL_EXSTYLE);
            let away = (WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0) as i32;
            let settled = if pending.hidden {
                style | away
            } else {
                style & !away
            };
            if settled != style {
                SetWindowLongW(hwnd, GWL_EXSTYLE, settled);
            }
        }
    }
    if !native_matches(pending.hidden) {
        return;
    }
    if pending.taskbar.is_none() {
        let Some(hwnd) = crate::window_state::main_window() else {
            return;
        };
        let request = crate::window_state::set_taskbar_button(hwnd, !pending.hidden);
        pending.taskbar = Some(request);
        let mut gate = GATE.lock().unwrap();
        if let Phase::Changing(current) = &mut gate.phase {
            if current.id == pending.id {
                current.taskbar = Some(request);
            }
        }
    }
    let taskbar_done = crate::window_state::completed_taskbar_request();
    if !pending
        .taskbar
        .is_some_and(|request| taskbar_done >= request)
    {
        return;
    }
    let composed = unsafe { DwmFlush().is_ok() };
    if GATE.lock().unwrap().complete(
        pending,
        taskbar_done,
        native_matches(pending.hidden),
        composed,
    ) {
        tracing::debug!(
            hidden = pending.hidden,
            "window visibility transition complete"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejected_queued_requests_release_the_gate_but_cannot_abort_a_transition() {
        let mut gate = Gate::new();
        assert!(gate.queue());
        gate.cancel_queued();
        assert!(gate.queue());
        assert!(gate.begin(true, 1));
        gate.cancel_queued();
        assert!(!gate.queue());
    }

    #[test]
    fn spam_is_discarded_until_every_completion_barrier_is_met() {
        for hidden in [true, false] {
            let mut gate = Gate::new();
            assert!(gate.queue());
            for _ in 0..1000 {
                assert!(!gate.queue());
            }
            assert!(gate.begin(hidden, 10));
            for _ in 0..1000 {
                assert!(!gate.queue());
                assert!(!gate.begin(!hidden, 10));
            }
            assert!(gate.candidate(11).is_none());
            let Phase::Changing(pending) = &mut gate.phase else {
                unreachable!()
            };
            pending.applied = true;
            pending.drawn_frame = Some(11);
            pending.taskbar = Some(7);
            assert!(gate.candidate(10).is_none());
            if !hidden {
                assert!(gate.candidate(11).is_none());
            }
            let pending = gate.candidate(12).unwrap();
            assert!(!gate.complete(pending, 6, true, true));
            assert!(!gate.complete(pending, 7, false, true));
            assert!(!gate.complete(pending, 7, true, false));
            assert!(!gate.queue());
            assert!(gate.complete(pending, 7, true, true));
            assert!(gate.queue());
            assert!(
                !gate.queue(),
                "ignored spam must not be replayed after completion"
            );
        }
    }
}
