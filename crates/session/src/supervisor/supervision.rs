//! Per-session tasks: supervision (gauges, bounded-run continuation) and live input.
//!
//! Every session owns two long-lived tasks, started when its creation commits and ended by
//! [`SessionHandle::close`]. They touch only their own session, so a worker whose pipe stalls
//! (up to the exchange deadline in `controlled_worker.rs`) delays nothing but itself.
//! Exchanges on one worker stay serialized by that session's `native` mutex; Tokio's mutex is
//! FIFO, so a queued pause or debugger command waits for at most the one exchange in flight.
//!
//! A timed-out exchange is never abandoned mid-reply: dropping it would leave the reply on the
//! pipe for the next request. The worker's own deadline ends the connection instead.
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{actor_client, check_lease, lock, observe, SessionHandle};
use crate::native_session::InputState;
use crate::telemetry::{Actor, InputMetrics};
use crate::SessionState;

/// Status cadence of an active session.
pub(super) const POLL_PERIOD: Duration = Duration::from_millis(100);
/// Cadence once the worker has stopped or crashed: no IPC, gauges only.
const INACTIVE_PERIOD: Duration = Duration::from_secs(1);
/// Queued live states per session before a newer state must overwrite the tail.
pub(super) const MAILBOX: usize = 8;

/// Supervise one session until it closes. Never issues `run` after a debugger stop.
pub(super) async fn supervise(handle: Arc<SessionHandle>) {
    let mut due = Instant::now() + POLL_PERIOD;
    loop {
        tokio::select! {
            () = tokio::time::sleep_until(due.into()) => {}
            () = handle.wake.notified() => {}
        }
        if handle.is_closed() {
            return;
        }
        let started = Instant::now();
        let active = poll(&handle, due, started).await;
        // Scheduled from completion: a slow exchange delays this session's next poll and
        // never causes a burst of catch-up polls.
        due = Instant::now() + if active { POLL_PERIOD } else { INACTIVE_PERIOD };
    }
}

/// One supervision pass; false when the worker is no longer active.
async fn poll(handle: &SessionHandle, due: Instant, started: Instant) -> bool {
    let Ok(mut native) = handle.native.try_lock() else {
        let mut telemetry = handle.telemetry();
        telemetry.busy = true;
        telemetry.supervision.busy_polls += 1;
        return true;
    };
    if !native.state().is_active() {
        handle.set_playing(false);
        let mut telemetry = handle.telemetry();
        telemetry.state = native.state();
        telemetry.fps = 0.0;
        telemetry.speed = 0.0;
        telemetry.memory_bytes = None;
        return false;
    }
    let status = native.status().await;
    let elapsed = started.elapsed();
    let status = match status {
        Ok(status) => status,
        Err(error) => {
            handle.set_playing(false);
            let mut telemetry = handle.telemetry();
            telemetry.state = native.state();
            telemetry.last_error = error.to_string();
            telemetry.supervision.status_failures += 1;
            return native.state().is_active();
        }
    };
    observe(handle, &native, &status);
    handle
        .telemetry()
        .supervision
        .poll(due, started, elapsed, POLL_PERIOD);
    if !handle.is_playing() || native.state() != SessionState::Paused {
        return true;
    }
    // A bounded run ended. Resume unless the guest stopped for a reason a retry
    // cannot fix; a host deadline only means the batch ran slowly.
    let stop = status["stop"]["reason"].as_str().unwrap_or("");
    if !matches!(stop, "" | "frame") {
        handle.set_playing(false);
        return true;
    }
    let reason = status["last_error"].as_str().unwrap_or("");
    if !matches!(reason, "" | "host_deadline") {
        handle.set_playing(false);
        return true;
    }
    match native.run().await {
        Ok(_) => handle.telemetry().supervision.continuations += 1,
        Err(error) => {
            handle.set_playing(false);
            handle.telemetry().last_error = error.to_string();
        }
    }
    true
}

/// A complete input state waiting for delivery.
#[derive(Debug, Clone)]
pub(super) struct Queued {
    pub state: InputState,
    pub owner: String,
    pub actor: Actor,
    /// When the oldest state folded into this entry was submitted.
    pub submitted: Instant,
}

/// Latest-value input queue of one session.
///
/// A newer state replaces the queued tail unless that would hide a transition: the tail
/// differs from the state before it in a button or touch bit and the newer state reverts that
/// bit. Then the newer state is appended, so a press and release shorter than one exchange
/// both reach the guest. Touch coordinates are not transitions and always take the newest
/// value. With [`MAILBOX`] entries queued the tail is overwritten and counted as dropped.
#[derive(Debug, Default)]
pub(super) struct Mailbox {
    queue: VecDeque<Queued>,
    /// State being delivered, taken from the queue after the session lock was acquired.
    in_flight: Option<InputState>,
    /// Last state this mailbox delivered; `None` once anything else may have set input.
    sent: Option<(InputState, String)>,
}

fn same(a: &InputState, b: &InputState) -> bool {
    a.buttons == b.buttons && a.touch == b.touch && a.x == b.x && a.y == b.y
}

impl Mailbox {
    pub(super) fn len(&self) -> usize {
        self.queue.len()
    }

    pub(super) fn submit(&mut self, item: Queued, metrics: &mut InputMetrics) {
        metrics.submitted += 1;
        if self.queue.iter().any(|queued| queued.owner != item.owner) {
            metrics.cancelled += self.queue.len() as u64;
            self.queue.clear();
        }
        let len = self.queue.len();
        let Some(tail) = self.queue.back() else {
            let held = match (&self.in_flight, &self.sent) {
                (Some(state), _) => same(state, &item.state),
                (None, Some((state, owner))) => same(state, &item.state) && *owner == item.owner,
                (None, None) => false,
            };
            if held {
                metrics.coalesced += 1;
            } else {
                self.queue.push_back(item);
            }
            return;
        };
        if same(&tail.state, &item.state) {
            metrics.coalesced += 1;
            return;
        }
        // Unknown worker input counts as released, which keeps more transitions, not fewer.
        let before = if len >= 2 {
            self.queue[len - 2].state
        } else {
            self.in_flight
                .or(self.sent.as_ref().map(|(state, _)| *state))
                .unwrap_or_default()
        };
        let changed_buttons = tail.state.buttons ^ before.buttons;
        let changed_touch = tail.state.touch != before.touch;
        let reverts = (item.state.buttons ^ tail.state.buttons) & changed_buttons != 0
            || (changed_touch && item.state.touch != tail.state.touch);
        if !reverts {
            let submitted = tail.submitted;
            *self.queue.back_mut().expect("tail exists") = Queued { submitted, ..item };
            metrics.coalesced += 1;
        } else if len < MAILBOX {
            self.queue.push_back(item);
            metrics.edges_kept += 1;
        } else {
            *self.queue.back_mut().expect("tail exists") = item;
            metrics.edges_dropped += 1;
        }
    }

    /// Discard pending states. `forget` also drops the record of what the worker holds.
    pub(super) fn cancel(&mut self, metrics: &mut InputMetrics, forget: bool) {
        metrics.cancelled += self.queue.len() as u64;
        self.queue.clear();
        if forget {
            self.sent = None;
        }
    }

    /// Something other than this mailbox may have changed the worker's input.
    pub(super) fn forget(&mut self) {
        self.sent = None;
    }
}

/// Deliver this session's mailbox, one exchange at a time, until the session closes.
pub(super) async fn deliver_input(handle: Arc<SessionHandle>) {
    loop {
        handle.input_wake.notified().await;
        loop {
            if handle.is_closed() {
                return;
            }
            if lock(&handle.mailbox).len() == 0 {
                break;
            }
            // Wait for the session before choosing the state, so the newest one is sent.
            let mut native = handle.native.lock().await;
            if handle.is_closed() {
                return;
            }
            let item = {
                let mut mailbox = lock(&handle.mailbox);
                let Some(item) = mailbox.queue.pop_front() else {
                    break;
                };
                mailbox.in_flight = Some(item.state);
                handle.telemetry().input.pending = mailbox.len();
                item
            };
            let age = item.submitted.elapsed();
            let result = match check_lease(&handle, actor_client(item.actor)) {
                Ok(()) => native.input(item.state, &item.owner).await,
                Err(error) => Err(error),
            };
            drop(native);
            let mut mailbox = lock(&handle.mailbox);
            mailbox.in_flight = None;
            let mut telemetry = handle.telemetry();
            match result {
                Ok(_) => {
                    mailbox.sent = Some((item.state, item.owner));
                    telemetry.input.delivered(age, item.submitted.elapsed());
                }
                Err(error) => {
                    mailbox.sent = None;
                    telemetry.input.failed += 1;
                    telemetry.last_error = format!("input: {error}");
                }
            }
            telemetry.input.pending = mailbox.len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(buttons: u32, touch: bool, x: u16) -> InputState {
        InputState {
            buttons,
            x,
            y: 0,
            touch,
        }
    }

    fn submit(mailbox: &mut Mailbox, metrics: &mut InputMetrics, value: InputState) {
        mailbox.submit(
            Queued {
                state: value,
                owner: "live".into(),
                actor: Actor::Desktop,
                submitted: Instant::now(),
            },
            metrics,
        );
    }

    fn queued(mailbox: &Mailbox) -> Vec<(u32, bool, u16)> {
        mailbox
            .queue
            .iter()
            .map(|q| (q.state.buttons, q.state.touch, q.state.x))
            .collect()
    }

    #[test]
    fn short_taps_keep_both_transitions() {
        let (mut mailbox, mut metrics) = (Mailbox::default(), InputMetrics::default());
        submit(&mut mailbox, &mut metrics, state(1, false, 0));
        submit(&mut mailbox, &mut metrics, state(0, false, 0));
        submit(&mut mailbox, &mut metrics, state(1, false, 0));
        submit(&mut mailbox, &mut metrics, state(0, false, 0));
        assert_eq!(
            queued(&mailbox),
            vec![(1, false, 0), (0, false, 0), (1, false, 0), (0, false, 0)]
        );
        assert_eq!(metrics.edges_kept, 3);
    }

    #[test]
    fn holds_and_drags_coalesce_to_the_newest_state() {
        let (mut mailbox, mut metrics) = (Mailbox::default(), InputMetrics::default());
        submit(&mut mailbox, &mut metrics, state(1, true, 10));
        submit(&mut mailbox, &mut metrics, state(1, true, 20));
        submit(&mut mailbox, &mut metrics, state(3, true, 30));
        // 3 adds a press on top of 1 without reverting it: still one entry, newest values.
        assert_eq!(queued(&mailbox), vec![(3, true, 30)]);
        assert_eq!(metrics.coalesced, 2);
        // Opposing buttons: release 1, press 4 in the same state keeps the earlier entry.
        submit(&mut mailbox, &mut metrics, state(6, true, 30));
        assert_eq!(queued(&mailbox), vec![(3, true, 30), (6, true, 30)]);
    }

    #[test]
    fn touch_down_up_survives_and_full_mailbox_counts_drops() {
        let (mut mailbox, mut metrics) = (Mailbox::default(), InputMetrics::default());
        submit(&mut mailbox, &mut metrics, state(0, true, 5));
        submit(&mut mailbox, &mut metrics, state(0, false, 5));
        assert_eq!(queued(&mailbox), vec![(0, true, 5), (0, false, 5)]);
        for i in 0..20 {
            submit(&mut mailbox, &mut metrics, state(i % 2, false, 5));
        }
        assert_eq!(mailbox.len(), MAILBOX);
        assert!(metrics.edges_dropped > 0);
        // The newest state is always last, so nothing stays pressed after a release.
        submit(&mut mailbox, &mut metrics, state(0, false, 5));
        assert_eq!(mailbox.queue.back().map(|q| q.state.buttons), Some(0));
    }

    #[test]
    fn owner_change_and_cancel_discard_pending_states() {
        let (mut mailbox, mut metrics) = (Mailbox::default(), InputMetrics::default());
        submit(&mut mailbox, &mut metrics, state(1, false, 0));
        submit(&mut mailbox, &mut metrics, state(0, false, 0));
        mailbox.submit(
            Queued {
                state: state(2, false, 0),
                owner: "scenario".into(),
                actor: Actor::Mcp,
                submitted: Instant::now(),
            },
            &mut metrics,
        );
        assert_eq!(queued(&mailbox), vec![(2, false, 0)]);
        assert_eq!(metrics.cancelled, 2);
        mailbox.cancel(&mut metrics, true);
        assert_eq!(mailbox.len(), 0);
        assert_eq!(metrics.cancelled, 3);
    }

    #[test]
    fn repeating_the_delivered_state_sends_nothing() {
        let (mut mailbox, mut metrics) = (Mailbox::default(), InputMetrics::default());
        mailbox.sent = Some((state(1, false, 0), "live".into()));
        submit(&mut mailbox, &mut metrics, state(1, false, 0));
        assert_eq!(mailbox.len(), 0);
        mailbox.forget();
        submit(&mut mailbox, &mut metrics, state(1, false, 0));
        assert_eq!(mailbox.len(), 1, "after forget the worker state is unknown");
    }
}
