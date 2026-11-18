//! Host-owned physical controller discovery and events (lotus#59).
//!
//! One [`ControllerService`] per LOTUS host process owns the controller backend on its own
//! thread, so device polling never depends on GPUI render cadence, window selection or a
//! worker. Workers and GPUI never enumerate gamepads. This crate only discovers devices and
//! normalizes their events; [`router`] composes them per session (lotus#61).
//!
//! Identity: a [`DeviceId`] names one *connection* of one device for this host run. It is
//! never reused, and a reconnect gets a new one. The backend's model UUID is reported but is
//! not an identity: two identical pads share it.
//!
//! Backend: gilrs 0.11.2 ([`gilrs_backend`]). On Windows the default `wgi` feature uses
//! Windows Gaming Input, which per gilrs' README "requires an in focus window to be
//! associated with the process to receive events"; the `xinput` feature (build with
//! `--no-default-features --features xinput`) uses XInput instead. The two are mutually
//! exclusive in gilrs-core (`platform/mod.rs`).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// Without `wgi` or `xinput` the crate has no backend (the layout and router only), so
/// `lotus-session` can use [`layout`] without linking gilrs.
#[cfg(feature = "backend")]
pub mod gilrs_backend;
pub mod layout;
pub mod router;
pub mod synthetic;

/// One connection of one physical controller during this host run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct DeviceId(pub u64);

impl std::fmt::Display for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "pad-{}", self.0)
    }
}

/// Logical buttons by position, independent of the backend (gilrs' `Button` naming, which
/// follows the SDL game controller layout).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    South,
    East,
    North,
    West,
    LeftShoulder,
    RightShoulder,
    LeftTrigger,
    RightTrigger,
    Select,
    Start,
    Mode,
    LeftThumb,
    RightThumb,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
}

/// Axes in -1.0..=1.0; sticks are positive right and up (gilrs convention).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    LeftX,
    LeftY,
    RightX,
    RightY,
    LeftZ,
    RightZ,
}

impl Button {
    pub const ALL: [Button; 17] = [
        Button::South,
        Button::East,
        Button::North,
        Button::West,
        Button::LeftShoulder,
        Button::RightShoulder,
        Button::LeftTrigger,
        Button::RightTrigger,
        Button::Select,
        Button::Start,
        Button::Mode,
        Button::LeftThumb,
        Button::RightThumb,
        Button::DpadUp,
        Button::DpadDown,
        Button::DpadLeft,
        Button::DpadRight,
    ];

    /// Position in [`Button::ALL`].
    #[must_use]
    pub fn index(self) -> usize {
        self as usize
    }
}

impl Axis {
    pub const ALL: [Axis; 6] = [
        Axis::LeftX,
        Axis::LeftY,
        Axis::RightX,
        Axis::RightY,
        Axis::LeftZ,
        Axis::RightZ,
    ];
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeviceInfo {
    pub name: String,
    /// Backend model UUID; shared by identical devices, never used as identity.
    pub model_uuid: String,
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    /// Whether the backend has a mapping that makes the logical layout meaningful.
    pub mapped: bool,
}

/// Every pressed button and non-zero axis of a device at one moment.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct InputSnapshot {
    pub buttons: Vec<Button>,
    pub axes: BTreeMap<Axis, f32>,
}

/// Raw backend event; `handle` is the backend's runtime index for a device.
#[derive(Debug, Clone, PartialEq)]
pub enum RawEvent {
    Connected {
        handle: usize,
        info: DeviceInfo,
        state: InputSnapshot,
    },
    Disconnected {
        handle: usize,
    },
    Button {
        handle: usize,
        button: Button,
        pressed: bool,
        time: SystemTime,
    },
    Axis {
        handle: usize,
        axis: Axis,
        value: f32,
        time: SystemTime,
    },
}

/// A controller backend: gilrs now; another (SDL3) only if compatibility testing needs it.
/// Constructed and used on the service thread only, so it need not be `Send`.
pub trait ControllerBackend {
    /// Devices connected now with their current state, as `Connected` events.
    fn connected(&mut self) -> Vec<RawEvent>;
    /// Next event, waiting at most `timeout`.
    fn next(&mut self, timeout: Duration) -> Option<RawEvent>;
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    /// A device appeared; `state` resynchronizes anything held at connect time.
    Connected {
        info: DeviceInfo,
        state: InputSnapshot,
    },
    /// The device is gone; consumers release everything it held.
    Disconnected,
    Button {
        button: Button,
        pressed: bool,
    },
    Axis {
        axis: Axis,
        value: f32,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Event {
    pub device: DeviceId,
    pub kind: EventKind,
    #[serde(skip)]
    pub time: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Device {
    pub id: DeviceId,
    pub info: DeviceInfo,
    pub state: InputSnapshot,
    pub events: u64,
}

#[derive(Debug, Default)]
struct Counters {
    events: AtomicU64,
    /// Events not delivered because the consumer's queue was full.
    dropped: AtomicU64,
    connects: AtomicU64,
    disconnects: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Diagnostics {
    pub events: u64,
    pub dropped: u64,
    pub connects: u64,
    pub disconnects: u64,
    pub running: bool,
}

/// Events queued for the consumer before new ones are dropped and counted.
pub const QUEUE: usize = 1024;
/// Longest the service thread waits for an event before checking for shutdown.
pub const WAIT: Duration = Duration::from_millis(50);

/// Device state: handle -> (id, device); the current devices only.
#[derive(Default)]
struct Devices {
    by_handle: BTreeMap<usize, Device>,
    next_id: u64,
}

pub struct ControllerService {
    devices: Arc<Mutex<Devices>>,
    counters: Arc<Counters>,
    running: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl ControllerService {
    /// Starts the service thread. `backend` runs on it; if it fails, the service reports no
    /// devices and `running == false`. Events arrive on the returned bounded receiver.
    pub fn start<B, F>(backend: F) -> std::io::Result<(Self, Receiver<Event>)>
    where
        B: ControllerBackend,
        F: FnOnce() -> Result<B, String> + Send + 'static,
    {
        let (sender, receiver) = std::sync::mpsc::sync_channel(QUEUE);
        let devices = Arc::new(Mutex::new(Devices::default()));
        let counters = Arc::new(Counters::default());
        let running = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = Worker {
            devices: devices.clone(),
            counters: counters.clone(),
            sender,
        };
        let (thread_running, thread_stop) = (running.clone(), stop.clone());
        let thread = std::thread::Builder::new()
            .name("lotus-input".into())
            .spawn(move || {
                let mut backend = match backend() {
                    Ok(backend) => backend,
                    Err(error) => {
                        tracing::warn!(%error, "no controller backend");
                        return;
                    }
                };
                thread_running.store(true, Ordering::Release);
                for event in backend.connected() {
                    worker.handle(event);
                }
                while !thread_stop.load(Ordering::Acquire) {
                    if let Some(event) = backend.next(WAIT) {
                        worker.handle(event);
                    }
                }
                thread_running.store(false, Ordering::Release);
            })?;
        Ok((
            Self {
                devices,
                counters,
                running,
                stop,
                thread: Some(thread),
            },
            receiver,
        ))
    }

    /// Currently connected devices, in connection order.
    #[must_use]
    pub fn devices(&self) -> Vec<Device> {
        let mut devices: Vec<_> = lock(&self.devices).by_handle.values().cloned().collect();
        devices.sort_by_key(|device| device.id);
        devices
    }

    #[must_use]
    pub fn diagnostics(&self) -> Diagnostics {
        Diagnostics {
            events: self.counters.events.load(Ordering::Acquire),
            dropped: self.counters.dropped.load(Ordering::Acquire),
            connects: self.counters.connects.load(Ordering::Relaxed),
            disconnects: self.counters.disconnects.load(Ordering::Relaxed),
            running: self.running.load(Ordering::Acquire),
        }
    }
}

impl Drop for ControllerService {
    /// Joins the thread; bounded by one [`WAIT`] plus one event's handling.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Worker {
    devices: Arc<Mutex<Devices>>,
    counters: Arc<Counters>,
    sender: SyncSender<Event>,
}

impl Worker {
    fn handle(&self, raw: RawEvent) {
        let mut devices = lock(&self.devices);
        let (device, kind, time) = match raw {
            RawEvent::Connected {
                handle,
                info,
                state,
            } => {
                // A connect for a handle still listed means the backend missed the
                // disconnect: retire the old connection first so its state is released.
                if let Some(old) = devices.by_handle.remove(&handle) {
                    self.counters.disconnects.fetch_add(1, Ordering::Relaxed);
                    self.send(Event {
                        device: old.id,
                        kind: EventKind::Disconnected,
                        time: SystemTime::now(),
                    });
                }
                devices.next_id += 1;
                let id = DeviceId(devices.next_id);
                devices.by_handle.insert(
                    handle,
                    Device {
                        id,
                        info: info.clone(),
                        state: state.clone(),
                        events: 0,
                    },
                );
                self.counters.connects.fetch_add(1, Ordering::Relaxed);
                (id, EventKind::Connected { info, state }, SystemTime::now())
            }
            RawEvent::Disconnected { handle } => {
                let Some(old) = devices.by_handle.remove(&handle) else {
                    return;
                };
                self.counters.disconnects.fetch_add(1, Ordering::Relaxed);
                (old.id, EventKind::Disconnected, SystemTime::now())
            }
            RawEvent::Button {
                handle,
                button,
                pressed,
                time,
            } => {
                let Some(device) = devices.by_handle.get_mut(&handle) else {
                    return;
                };
                let held = &mut device.state.buttons;
                match (held.binary_search(&button), pressed) {
                    (Ok(_), true) | (Err(_), false) => return,
                    (Err(at), true) => held.insert(at, button),
                    (Ok(at), false) => {
                        held.remove(at);
                    }
                }
                device.events += 1;
                (device.id, EventKind::Button { button, pressed }, time)
            }
            RawEvent::Axis {
                handle,
                axis,
                value,
                time,
            } => {
                let Some(device) = devices.by_handle.get_mut(&handle) else {
                    return;
                };
                let value = value.clamp(-1.0, 1.0);
                if value == 0.0 {
                    device.state.axes.remove(&axis);
                } else {
                    device.state.axes.insert(axis, value);
                }
                device.events += 1;
                (device.id, EventKind::Axis { axis, value }, time)
            }
        };
        drop(devices);
        self.send(Event { device, kind, time });
    }

    /// Counts after the outcome, so `events` never runs ahead of `dropped`.
    fn send(&self, event: Event) {
        if let Err(TrySendError::Full(_)) = self.sender.try_send(event) {
            self.counters.dropped.fetch_add(1, Ordering::Release);
        }
        self.counters.events.fetch_add(1, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::synthetic::Synthetic;
    use super::*;

    fn info(name: &str) -> DeviceInfo {
        DeviceInfo {
            name: name.into(),
            model_uuid: "same-model".into(),
            vendor_id: Some(0x045e),
            product_id: Some(0x028e),
            mapped: true,
        }
    }

    fn recv(receiver: &Receiver<Event>) -> (DeviceId, EventKind) {
        let event = receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("event");
        (event.device, event.kind)
    }

    fn press(button: Button, pressed: bool) -> EventKind {
        EventKind::Button { button, pressed }
    }

    #[test]
    fn identical_pads_stay_distinct_and_reconnects_are_new_connections() {
        let (pad, script) = Synthetic::new();
        script.connect(0, info("Pad"), InputSnapshot::default());
        let (service, events) = ControllerService::start(move || Ok(pad)).unwrap();
        let (a, _) = recv(&events);
        script.connect(1, info("Pad"), InputSnapshot::default());
        let (b, _) = recv(&events);
        assert_ne!(a, b);

        script.button(0, Button::South, true);
        script.button(1, Button::East, true);
        script.axis(1, Axis::LeftX, 0.5);
        assert_eq!(recv(&events), (a, press(Button::South, true)));
        assert_eq!(recv(&events), (b, press(Button::East, true)));
        assert_eq!(
            recv(&events),
            (
                b,
                EventKind::Axis {
                    axis: Axis::LeftX,
                    value: 0.5
                }
            )
        );
        let devices = service.devices();
        assert_eq!(devices[0].state.buttons, [Button::South]);
        assert_eq!(devices[1].state.buttons, [Button::East]);

        script.disconnect(0);
        assert_eq!(recv(&events), (a, EventKind::Disconnected));
        // The same backend slot reconnecting, holding a button: new ID, state resynchronized.
        let held = InputSnapshot {
            buttons: vec![Button::Start],
            ..InputSnapshot::default()
        };
        script.connect(0, info("Pad"), held.clone());
        let (c, kind) = recv(&events);
        assert!(c != a && c != b);
        assert_eq!(
            kind,
            EventKind::Connected {
                info: info("Pad"),
                state: held
            }
        );
        let diagnostics = service.diagnostics();
        assert_eq!((diagnostics.connects, diagnostics.disconnects), (3, 1));
        assert!(diagnostics.running);
    }

    #[test]
    fn duplicate_edges_and_unknown_handles_are_ignored() {
        let (pad, script) = Synthetic::new();
        script.connect(0, info("Pad"), InputSnapshot::default());
        script.button(0, Button::North, true);
        script.button(0, Button::North, true);
        script.button(7, Button::North, true);
        script.disconnect(7);
        script.button(0, Button::North, false);
        let (service, events) = ControllerService::start(move || Ok(pad)).unwrap();
        let (a, _) = recv(&events);
        assert_eq!(recv(&events), (a, press(Button::North, true)));
        assert_eq!(recv(&events), (a, press(Button::North, false)));
        assert!(events.recv_timeout(WAIT * 3).is_err());
        assert_eq!(service.diagnostics().events, 3);
    }

    #[test]
    fn a_missed_disconnect_releases_the_old_connection() {
        let (pad, script) = Synthetic::new();
        script.connect(0, info("Pad"), InputSnapshot::default());
        script.connect(0, info("Pad"), InputSnapshot::default());
        let (service, events) = ControllerService::start(move || Ok(pad)).unwrap();
        let (a, _) = recv(&events);
        assert_eq!(recv(&events), (a, EventKind::Disconnected));
        let (b, _) = recv(&events);
        assert_ne!(a, b);
        assert_eq!(service.devices().len(), 1);
    }

    #[test]
    fn a_full_queue_drops_and_counts_instead_of_blocking() {
        let (pad, script) = Synthetic::new();
        script.connect(0, info("Pad"), InputSnapshot::default());
        for i in 0..QUEUE + 10 {
            script.button(0, Button::South, i % 2 == 0);
        }
        let (service, _events) = ControllerService::start(move || Ok(pad)).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while service.diagnostics().events < (QUEUE + 11) as u64 {
            assert!(
                std::time::Instant::now() < deadline,
                "{:?}",
                service.diagnostics()
            );
            std::thread::yield_now();
        }
        assert_eq!(service.diagnostics().dropped, 11);
    }

    #[test]
    fn shutdown_joins_and_a_failed_backend_reports_not_running() {
        let (pad, _script) = Synthetic::new();
        let (service, events) = ControllerService::start(move || Ok(pad)).unwrap();
        let started = std::time::Instant::now();
        drop(service);
        assert!(started.elapsed() < WAIT * 4);
        assert!(
            events.recv().is_err(),
            "the thread ended and dropped its sender"
        );

        let (service, _) =
            ControllerService::start(|| Err::<Synthetic, _>("no backend".to_string())).unwrap();
        std::thread::sleep(WAIT);
        assert!(!service.diagnostics().running);
        assert!(service.devices().is_empty());
    }
}
