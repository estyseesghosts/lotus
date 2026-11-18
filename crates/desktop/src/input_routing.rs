//! Desktop input routing (lotus#61): keyboard, pointer and controllers to sessions.
//!
//! [`lotus_input::router::Router`] composes one guest state per session from every source
//! targeting it; this module submits those states to the supervisor and keeps the router in
//! step with the device service and the session list. The window reports keyboard and pointer
//! changes with the session they target; a background thread applies controller events.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use lotus_input::router::{GuestState, PadStatus, Remembered, Router};
use lotus_input::{ControllerService, DeviceId, Event};
use lotus_session::native_session::InputState;
use lotus_session::supervisor::Supervisor;
use lotus_session::telemetry::Actor;
use serde::Serialize;

/// Owner of desktop input on the worker (`ctr_worker/control.h` accepts `live` and
/// `scenario`; a `scenario` owner refuses `live` input).
const OWNER: &str = "live";
/// Longest the router thread waits for a controller event before checking the device list,
/// the drop counter, the session list and shutdown.
const RECONCILE: Duration = Duration::from_millis(250);

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct RoutingMetrics {
    /// Submissions refused by the supervisor (session gone or stopping, control disabled).
    pub refused: u64,
    /// States whose sticks were dropped because the worker does not list `analog_input`.
    pub analog_dropped: u64,
}

/// What the window's controller list shows.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Controllers {
    pub pads: Vec<PadStatus>,
    pub remembered: Vec<Remembered>,
    pub identifying: bool,
    pub identified: Option<DeviceId>,
    pub identifications: u64,
}

/// Shared by the window (keyboard, pointer) and the controller thread.
pub struct InputRouting {
    router: Mutex<Router>,
    host: Arc<Supervisor>,
    metrics: Mutex<RoutingMetrics>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn input_state(state: GuestState) -> InputState {
    let (x, y) = state.touch.unwrap_or_default();
    InputState {
        buttons: state.buttons,
        x,
        y,
        touch: state.touch.is_some(),
        circle_pad: state.circle_pad,
        c_stick: state.c_stick,
    }
}

impl InputRouting {
    #[must_use]
    pub fn new(host: Arc<Supervisor>) -> Arc<Self> {
        Arc::new(Self {
            router: Mutex::new(Router::default()),
            host,
            metrics: Mutex::default(),
        })
    }

    /// Apply a router change and submit what it returned, under the router lock so states
    /// for one session reach its mailbox in the order they were composed.
    /// `Supervisor::submit_input` only queues, so a stalled worker blocks nobody here.
    fn route(&self, change: impl FnOnce(&mut Router) -> Vec<(String, GuestState)>) {
        let mut router = lock(&self.router);
        let changed = change(&mut router);
        self.submit(&mut router, changed);
    }

    fn submit(&self, router: &mut Router, changed: Vec<(String, GuestState)>) {
        for (session, state) in changed {
            let mut input = input_state(state);
            let mut result = self
                .host
                .submit_input(&session, input, OWNER, Actor::Desktop);
            if result
                .as_ref()
                .is_err_and(|error| error.kind() == std::io::ErrorKind::Unsupported)
            {
                // A worker without `analog_input` still takes buttons and touch.
                lock(&self.metrics).analog_dropped += 1;
                input.circle_pad = [0.0; 2];
                input.c_stick = [0.0; 2];
                result = self
                    .host
                    .submit_input(&session, input, OWNER, Actor::Desktop);
            }
            if let Err(error) = result {
                tracing::debug!(%session, %error, "input refused");
                lock(&self.metrics).refused += 1;
                // `Supervisor::handle` reports an unknown session as PermissionDenied, the
                // same kind as disabled control, so ask the registry.
                if self.host.sessions().contains(&session) {
                    router.invalidate(&session);
                } else {
                    router.remove_session(&session);
                }
            }
        }
    }

    /// Keyboard buttons (3DS bits) for the session keyboard input targets, if any.
    pub fn keyboard(&self, session: Option<&str>, buttons: u32) {
        self.route(|router| router.keyboard(session, buttons));
    }

    /// Pointer touch in bottom-screen pixels for the session the pointer targets, if any.
    pub fn pointer(&self, session: Option<&str>, touch: Option<(u16, u16)>) {
        self.route(|router| router.pointer(session, touch));
    }

    /// Assign a controller to a running session, or unassign it with `None` (lotus#62).
    /// See [`Router::assign`] for the policy; `replace` takes the session from the controller
    /// it has. The error is shown to the person as is.
    pub fn assign(
        &self,
        device: DeviceId,
        session: Option<&str>,
        replace: bool,
    ) -> Result<(), String> {
        if let Some(session) = session {
            if !self.host.sessions().iter().any(|s| s == session) {
                return Err(format!("session {session} is not running"));
            }
        }
        let mut router = lock(&self.router);
        let changed = router
            .assign(device, session, replace)
            .map_err(|error| error.to_string())?;
        self.submit(&mut router, changed);
        Ok(())
    }

    /// Report the next controller to press a button ([`InputRouting::controllers`]).
    pub fn identify(&self) {
        lock(&self.router).identify();
    }

    /// Connected controllers, remembered assignments and identification state.
    #[must_use]
    pub fn controllers(&self) -> Controllers {
        let router = lock(&self.router);
        let (identifying, identified, identifications) = router.identification();
        Controllers {
            pads: router.pads(),
            remembered: router.remembered().to_vec(),
            identifying,
            identified,
            identifications,
        }
    }

    pub fn event(&self, event: &Event) {
        self.route(|router| router.event(event));
    }

    /// Repair lost device events and drop sessions that ended.
    pub fn reconcile(&self, service: &ControllerService, lost_events: bool) {
        let devices = service.devices();
        self.route(|router| router.reconcile(&devices, lost_events));
        self.forget_ended_sessions();
    }

    fn forget_ended_sessions(&self) {
        let live: BTreeSet<String> = self.host.sessions().into_iter().collect();
        let mut router = lock(&self.router);
        let targeted: BTreeSet<String> = router
            .assignments()
            .into_values()
            .flatten()
            .chain(router.targets())
            .chain(
                router
                    .remembered()
                    .iter()
                    .map(|entry| entry.session.clone()),
            )
            .collect();
        for session in targeted.difference(&live) {
            router.remove_session(session);
        }
    }

    /// For `frontend_metrics.controllers.routing`.
    #[must_use]
    pub fn status(&self) -> serde_json::Value {
        let controllers = self.controllers();
        let router = lock(&self.router);
        let assignments: serde_json::Map<String, serde_json::Value> = router
            .assignments()
            .into_iter()
            .map(|(device, session)| (device.0.to_string(), serde_json::json!(session)))
            .collect();
        serde_json::json!({
            "assignments": assignments,
            "max_assigned": lotus_input::router::MAX_ASSIGNED,
            "remembered": controllers.remembered,
            "identify": {
                "armed": controllers.identifying,
                "last": controllers.identified,
                "count": controllers.identifications,
            },
            "router": router.metrics(),
            "submission": *lock(&self.metrics),
        })
    }

    /// Apply controller events until `shutdown`; reconcile on every [`RECONCILE`] period
    /// without events and whenever the service reports dropped events.
    pub fn run(
        self: &Arc<Self>,
        service: &ControllerService,
        events: &Receiver<Event>,
        shutdown: &AtomicBool,
    ) {
        let mut dropped = service.diagnostics().dropped;
        while !shutdown.load(Ordering::Acquire) {
            match events.recv_timeout(RECONCILE) {
                Ok(event) => self.event(&event),
                Err(RecvTimeoutError::Timeout) => self.reconcile(service, false),
                Err(RecvTimeoutError::Disconnected) => return,
            }
            let now = service.diagnostics().dropped;
            if now != dropped {
                dropped = now;
                // Drain what is queued first so the snapshot is not older than the queue.
                for event in events.try_iter() {
                    self.event(&event);
                }
                self.reconcile(service, true);
            }
        }
    }
}
