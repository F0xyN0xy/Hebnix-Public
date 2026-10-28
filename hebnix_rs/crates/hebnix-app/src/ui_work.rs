//! Limit background event handling so bursts leave time for input and painting.
use crossbeam_channel::{Receiver, Sender};
use eframe::egui;
use std::time::{Duration, Instant};

/// Publish the command before waking its UI-thread consumer. A native wake is
/// needed even if an earlier egui repaint is still pending/coalesced.
pub fn send_control<T>(tx: &Sender<T>, ctx: &egui::Context, message: T) -> bool {
    if tx.send(message).is_err() {
        return false;
    }
    crate::window_state::wake_ui(ctx);
    true
}

/// Ignore toggles during an unfinished transition, without queuing a replay.
/// Busy is a successful no-op; false means the consumer has disconnected.
pub fn send_toggle_control<T>(tx: &Sender<T>, ctx: &egui::Context, message: T) -> bool {
    if !crate::visibility::queue_toggle() {
        return true;
    }
    if !send_control(tx, ctx, message) {
        crate::visibility::cancel_queued_toggle();
        return false;
    }
    true
}

#[cfg(test)]
fn messages<T>(rx: &Receiver<T>, ctx: &egui::Context) -> impl Iterator<Item = T> + use<T> {
    message_batch(rx, None, ctx)
}

pub fn priority_messages<T>(
    priority: &Receiver<T>,
    rx: &Receiver<T>,
    ctx: &egui::Context,
) -> impl Iterator<Item = T> + use<T> {
    message_batch(rx, Some(priority), ctx)
}

fn message_batch<T>(
    rx: &Receiver<T>,
    priority: Option<&Receiver<T>>,
    ctx: &egui::Context,
) -> impl Iterator<Item = T> + use<T> {
    let rx = rx.clone();
    let priority = priority.cloned();
    let ctx = ctx.clone();
    let start = Instant::now();
    let mut remaining = 128;
    std::iter::from_fn(move || {
        // Check before receiving: stopping a frame must not discard the next event.
        if remaining == 0 || start.elapsed() >= Duration::from_millis(2) {
            if !rx.is_empty() || priority.as_ref().is_some_and(|rx| !rx.is_empty()) {
                ctx.request_repaint();
            }
            return None;
        }
        remaining -= 1;
        priority
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
            .or_else(|| rx.try_recv().ok())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_bursts_are_not_queued_and_quit_bypasses_the_gate() {
        let ctx = egui::Context::default();
        let (tx, rx) = crossbeam_channel::unbounded();
        for _ in 0..1000 {
            assert!(send_toggle_control(&tx, &ctx, "toggle"));
        }
        assert!(send_control(&tx, &ctx, "quit"));
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), ["toggle", "quit"]);
        crate::visibility::cancel_queued_toggle();

        drop(rx);
        assert!(!send_toggle_control(&tx, &ctx, "disconnected"));
        assert!(
            crate::visibility::queue_toggle(),
            "failed sends must release their reservation"
        );
        crate::visibility::cancel_queued_toggle();
    }

    #[test]
    fn controls_bypass_background_backlog_without_reordering_either_queue() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (control_tx, control_rx) = crossbeam_channel::unbounded();
        for event in 0..1000 {
            tx.send(event).unwrap();
        }
        control_tx.send(1000).unwrap();
        control_tx.send(1001).unwrap();
        let ctx = egui::Context::default();
        let mut received = Vec::new();
        while !rx.is_empty() || !control_rx.is_empty() {
            received.extend(priority_messages(&control_rx, &rx, &ctx));
        }
        assert_eq!(&received[..2], &[1000, 1001]);
        assert_eq!(&received[2..], &(0..1000).collect::<Vec<_>>());
    }

    #[test]
    fn event_bursts_are_deferred_without_loss_or_reordering() {
        let (tx, rx) = crossbeam_channel::unbounded();
        for event in 0..1000 {
            tx.send(event).unwrap();
        }
        let ctx = egui::Context::default();
        let first: Vec<_> = messages(&rx, &ctx).collect();
        assert!(first.len() <= 128);
        assert!(!rx.is_empty());
        let mut received = first;
        while !rx.is_empty() {
            received.extend(messages(&rx, &ctx));
        }
        assert_eq!(received, (0..1000).collect::<Vec<_>>());
    }

    #[test]
    fn slow_handler_yields_before_receiving_the_next_event() {
        let (tx, rx) = crossbeam_channel::unbounded();
        tx.send(1).unwrap();
        tx.send(2).unwrap();
        let ctx = egui::Context::default();
        let mut batch = messages(&rx, &ctx);
        assert_eq!(batch.next(), Some(1));
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(batch.next(), None);
        assert_eq!(rx.try_recv(), Ok(2));
    }
}
