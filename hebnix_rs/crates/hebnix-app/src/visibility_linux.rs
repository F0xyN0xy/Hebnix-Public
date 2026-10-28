//! Admit one visibility request at a time. Windows also waits for native,
//! render and taskbar barriers before taking the next one; Linux has none of
//! those, so a change is complete as soon as it has been applied.

use std::sync::Mutex;

use eframe::egui::Context;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Queued,
    Changing,
}

static PHASE: Mutex<Phase> = Mutex::new(Phase::Idle);

fn phase() -> std::sync::MutexGuard<'static, Phase> {
    PHASE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// reserve the one pending toggle; false while another is still in flight
pub fn queue_toggle() -> bool {
    let mut phase = phase();
    if *phase != Phase::Idle {
        return false;
    }
    *phase = Phase::Queued;
    true
}

pub fn cancel_queued_toggle() {
    let mut phase = phase();
    if *phase == Phase::Queued {
        *phase = Phase::Idle;
    }
}

pub fn begin_change(_ctx: &Context, _hidden: bool) -> bool {
    let mut phase = phase();
    if *phase == Phase::Changing {
        return false;
    }
    *phase = Phase::Changing;
    true
}

pub fn applied(_ctx: &Context) {
    *phase() = Phase::Idle;
}

pub fn note_ui_frame(_ctx: &Context) {}

pub fn poll(_ctx: &Context) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_toggle_at_a_time_until_applied() {
        let ctx = Context::default();
        cancel_queued_toggle();
        assert!(queue_toggle());
        assert!(!queue_toggle());
        assert!(begin_change(&ctx, true));
        assert!(!begin_change(&ctx, false));
        applied(&ctx);
        assert!(queue_toggle());
        cancel_queued_toggle();
    }
}
