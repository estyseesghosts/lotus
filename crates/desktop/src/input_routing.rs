//! Desktop input routing (lotus#61): keyboard, pointer and controllers to sessions.
//!
//! [`lotus_input::router::Router`] composes one guest state per session from every source
//! targeting it; this module submits those states to the supervisor and keeps the router in
//! step with the device service and the session list. The window reports keyboard and pointer
//! changes with the session they target; a background thread applies controller events.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use lotus_input::layout::Mapping;
use lotus_input::router::{GuestState, PadStatus, Remembered, Router};
use lotus_input::{ControllerService, DeviceId, Event, EventKind};
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

/// Latency samples kept per controller for the percentiles.
const LATENCY_WINDOW: usize = 256;
/// Disconnected controllers whose metrics stay listed; connection IDs are never reused, so
/// keeping all of them would grow without bound across hotplugs.
const RETIRED: usize = 8;

/// Per-controller routing counters (lotus#66); see [`DeviceMetrics`].
#[derive(Debug, Clone, Default)]
struct DeviceCounters {
    session: Option<String>,
    connected: bool,
    received: u64,
    submitted: u64,
    unchanged: u64,
    unrouted: u64,
    refused: u64,
    last: Option<Instant>,
    latencies: VecDeque<f32>,
    latency_max_ms: f32,
    busy: Duration,
}

/// One controller in `frontend_metrics.controllers.routing.devices` (lotus#66). Latency is
/// from the backend's event timestamp to the supervisor accepting the composed state into
/// the session's mailbox; mailbox-to-worker delivery is the session's own `input` telemetry.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeviceMetrics {
    pub device: DeviceId,
    /// Assigned session when the last event arrived.
    pub session: Option<String>,
    pub connected: bool,
    /// Events from the device service.
    pub received: u64,
    /// Events that changed their session's composed state and were submitted.
    pub submitted: u64,
    /// Button and axis events that left the composed state as it was (a duplicate edge, a stick inside the
    /// dead zone, a button another source already holds), so nothing was sent.
    pub unchanged: u64,
    /// Button and axis events while the controller had no session; they reach no guest.
    pub unrouted: u64,
    /// Submissions for this controller's session that the supervisor refused.
    pub refused: u64,
    pub last_event_age_ms: Option<f64>,
    pub latency_p50_ms: f64,
    pub latency_p95_ms: f64,
    pub latency_max_ms: f64,
    /// Router-thread time spent handling this device's events: routing, composition and
    /// the mailbox handoff. Wall time inside `event`, so it includes waits on the router
    /// and supervisor locks; device polling on the `lotus-input` thread is not included.
    pub busy_ms: f64,
}

fn percentile(sorted: &[f32], percent: usize) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    // Nearest rank, as `InputMetrics` in lotus-session.
    f64::from(sorted[(sorted.len() * percent).div_ceil(100).max(1) - 1])
}

impl DeviceCounters {
    fn report(&self, device: DeviceId, now: Instant) -> DeviceMetrics {
        let mut sorted: Vec<f32> = self.latencies.iter().copied().collect();
        sorted.sort_by(f32::total_cmp);
        DeviceMetrics {
            device,
            session: self.session.clone(),
            connected: self.connected,
            received: self.received,
            submitted: self.submitted,
            unchanged: self.unchanged,
            unrouted: self.unrouted,
            refused: self.refused,
            last_event_age_ms: self
                .last
                .map(|last| now.duration_since(last).as_secs_f64() * 1e3),
            latency_p50_ms: percentile(&sorted, 50),
            latency_p95_ms: percentile(&sorted, 95),
            latency_max_ms: f64::from(self.latency_max_ms),
            busy_ms: self.busy.as_secs_f64() * 1e3,
        }
    }
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
    devices: Mutex<BTreeMap<DeviceId, DeviceCounters>>,
    /// Input layouts given to the router (lotus#63), by session, and the settings revision
    /// they were resolved at. Lock order: `router`, then `layouts`.
    layouts: Mutex<Layouts>,
}

#[derive(Default)]
struct Layouts {
    revision: Option<u64>,
    sessions: BTreeMap<String, LayoutStatus>,
}

/// One session's layout in `frontend_metrics.controllers.routing.layouts`.
#[derive(Debug, Clone, Serialize)]
pub struct LayoutStatus {
    pub preset: String,
    /// Selected presets that are missing and bindings this build cannot read.
    pub problems: Vec<String>,
    /// Bindings on more than one action, as `binding -> actions`.
    pub conflicts: Vec<String>,
    pub unbound: Vec<lotus_input::layout::Action>,
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
            devices: Mutex::default(),
            layouts: Mutex::default(),
        })
    }

    /// Give each running session the layout its settings resolve to, when the settings
    /// revision changed or for sessions not seen yet; `force` re-resolves all. A changed
    /// layout suppresses what that session's sources hold (`Router::set_mapping`).
    pub fn sync_layouts(&self, force: bool) {
        let revision = self.host.settings_revision();
        let live = self.host.sessions();
        let wanted: Vec<String> = {
            let layouts = lock(&self.layouts);
            let stale = force || layouts.revision != Some(revision);
            live.iter()
                .filter(|s| stale || !layouts.sessions.contains_key(*s))
                .cloned()
                .collect()
        };
        // Resolve (file reads) before taking the router lock, so routing never waits on it.
        let resolved: Vec<(String, LayoutStatus, Arc<Mapping>)> = wanted
            .into_iter()
            .filter_map(|session| {
                let (resolved, profile) = self.host.input_layout(&session).ok()?;
                let status = LayoutStatus {
                    preset: resolved.preset.clone(),
                    problems: resolved.problems.iter().map(|p| format!("{p:?}")).collect(),
                    conflicts: resolved
                        .conflicts()
                        .iter()
                        .map(|(binding, actions)| format!("{binding:?} -> {actions:?}"))
                        .collect(),
                    unbound: resolved.unbound(),
                };
                Some((session, status, Arc::new(Mapping::new(&resolved, profile))))
            })
            .collect();
        let mut router = lock(&self.router);
        let mut layouts = lock(&self.layouts);
        layouts.sessions.retain(|session, _| live.contains(session));
        let mut changed = Vec::new();
        for (session, status, mapping) in resolved {
            changed.extend(router.set_mapping(&session, Some(mapping)));
            layouts.sessions.insert(session, status);
        }
        layouts.revision = Some(revision);
        drop(layouts);
        self.submit(&mut router, changed);
    }

    /// Apply a router change and submit what it returned, under the router lock so states
    /// for one session reach its mailbox in the order they were composed.
    /// `Supervisor::submit_input` only queues, so a stalled worker blocks nobody here.
    fn route(&self, change: impl FnOnce(&mut Router) -> Vec<(String, GuestState)>) {
        let mut router = lock(&self.router);
        let changed = change(&mut router);
        self.submit(&mut router, changed);
    }

    /// Per-controller routing metrics, connected controllers first.
    #[must_use]
    pub fn device_metrics(&self) -> Vec<DeviceMetrics> {
        let now = Instant::now();
        let devices = lock(&self.devices);
        let mut list: Vec<DeviceMetrics> = devices
            .iter()
            .map(|(id, counters)| counters.report(*id, now))
            .collect();
        list.sort_by_key(|m| (!m.connected, m.device));
        list
    }

    /// Returns the sessions whose state the supervisor refused.
    fn submit(&self, router: &mut Router, changed: Vec<(String, GuestState)>) -> Vec<String> {
        let mut refused = Vec::new();
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
                refused.push(session);
            }
        }
        refused
    }

    /// Keyboard buttons (3DS bits) for the session keyboard input targets, if any.
    pub fn keyboard(&self, session: Option<&str>, buttons: u32) {
        self.route(|router| router.keyboard(session, buttons));
    }

    /// Keyboard keys held (GPUI names) for the session keyboard input targets, mapped through
    /// that session's input layout (lotus#63).
    pub fn keyboard_keys(&self, session: Option<&str>, keys: std::collections::BTreeSet<String>) {
        if session.is_some_and(|s| !lock(&self.layouts).sessions.contains_key(s)) {
            self.sync_layouts(false);
        }
        self.route(|router| router.keyboard_keys(session, keys));
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
            if !self
                .host
                .summary(session)
                .is_ok_and(|summary| summary.telemetry.state.is_active())
            {
                return Err(format!("session {session} is not running"));
            }
            if !lock(&self.layouts).sessions.contains_key(session) {
                self.sync_layouts(false);
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

    #[cfg(feature = "silvestris-ui")]
    pub(crate) fn guest_state(&self, session: &str) -> GuestState {
        lock(&self.router).compose(session)
    }

    pub fn event(&self, event: &Event) {
        let started = Instant::now();
        let mut router = lock(&self.router);
        let session = router.assigned(event.device).map(str::to_owned);
        let changed = router.event(event);
        let submitted = session
            .as_ref()
            .is_some_and(|s| changed.iter().any(|(changed, _)| changed == s));
        let refused = self.submit(&mut router, changed);
        drop(router);
        // Taken after the supervisor accepted the state, so the sample covers the queue from
        // the device service, routing, composition and the mailbox handoff.
        let latency = SystemTime::now()
            .duration_since(event.time)
            .unwrap_or_default();
        let mut devices = lock(&self.devices);
        let counters = devices.entry(event.device).or_default();
        counters.session.clone_from(&session);
        counters.received += 1;
        let now = Instant::now();
        counters.busy += now.duration_since(started);
        counters.last = Some(now);
        // Connection changes are not guest input, so they never count as unrouted or
        // unchanged; a disconnect that releases held buttons still counts as submitted.
        let input = match &event.kind {
            EventKind::Connected { .. } => {
                counters.connected = true;
                false
            }
            EventKind::Disconnected => {
                counters.connected = false;
                false
            }
            EventKind::Button { .. } | EventKind::Axis { .. } => true,
        };
        if session.as_ref().is_some_and(|s| refused.contains(s)) {
            counters.refused += 1;
        } else if submitted {
            counters.submitted += 1;
            let ms = (latency.as_secs_f64() * 1e3) as f32;
            counters.latency_max_ms = counters.latency_max_ms.max(ms);
            if counters.latencies.len() == LATENCY_WINDOW {
                counters.latencies.pop_front();
            }
            counters.latencies.push_back(ms);
        } else if input && session.is_none() {
            counters.unrouted += 1;
        } else if input {
            counters.unchanged += 1;
        }
        if !counters.connected {
            let retired: Vec<DeviceId> = devices
                .iter()
                .filter(|(_, c)| !c.connected)
                .map(|(id, _)| *id)
                .collect();
            // IDs only grow, so the first ones are the oldest connections.
            for id in retired.iter().take(retired.len().saturating_sub(RETIRED)) {
                devices.remove(id);
            }
        }
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
            "devices": self.device_metrics(),
            "layouts": lock(&self.layouts).sessions,
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
        let mut maintained = Instant::now();
        while !shutdown.load(Ordering::Acquire) {
            match events.recv_timeout(RECONCILE.saturating_sub(maintained.elapsed())) {
                Ok(event) => self.event(&event),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            // Maintenance runs on a deadline, not on a quiet receiver: under a steady event
            // stream that never times out, ended sessions must still lose their pads (#111)
            // and settings edits still reach running sessions within one period.
            if maintained.elapsed() >= RECONCILE {
                maintained = Instant::now();
                self.reconcile(service, false);
                self.sync_layouts(false);
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
