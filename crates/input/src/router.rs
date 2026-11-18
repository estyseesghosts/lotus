//! Source-aware routing of desktop input to sessions (lotus#61).
//!
//! The router keeps what each *source* holds (the keyboard, the pointer, each controller
//! connection) and which session that source targets, and composes one complete guest state
//! per session from every source targeting it. It returns only the sessions whose composed
//! state changed, so a caller submits each state once. Nothing here knows about workers,
//! mailboxes or GPUI; the desktop feeds it and submits its output to the supervisor.
//!
//! - The keyboard and the pointer follow the session the UI targets (the selected or focused
//!   viewport). Controllers follow explicit assignment only ([`Router::assign`]), so changing
//!   the selection, focus or layout never moves or releases a controller.
//! - Composition: digital buttons are ORed over sources. Touch comes from the pointer only.
//!   Each stick takes the source with the largest deflection, ties to the earlier source in
//!   [`Source`] order, so two sources never race last-write-wins.
//! - A source that moves to another session first leaves the old one (which is recomposed
//!   without it) and brings nothing held: a controller's buttons and sticks held at assignment
//!   stay suppressed until released or centred, so an old held state never reaches the new
//!   session.
//! - Disconnecting a controller removes only its contribution.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::{Axis, Button, DeviceId, DeviceInfo, Event, EventKind, InputSnapshot};

/// Controllers assigned at once (lotus#62 v1). One controller per session and one session
/// per controller always hold; this caps how many pairs exist.
pub const MAX_ASSIGNED: usize = 2;

/// Why [`Router::assign`] refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssignError {
    UnknownDevice(DeviceId),
    /// The session already has this controller; pass `replace` to move it off.
    SessionOccupied(DeviceId),
    CapReached,
}

impl std::fmt::Display for AssignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownDevice(id) => write!(f, "controller {id} is not connected"),
            Self::SessionOccupied(id) => write!(
                f,
                "this session already has controller {id}; unassign or replace it"
            ),
            Self::CapReached => write!(
                f,
                "{MAX_ASSIGNED} controllers are already assigned; unassign one first"
            ),
        }
    }
}

impl std::error::Error for AssignError {}

/// One connected controller, for the UI and `frontend_metrics`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PadStatus {
    pub id: DeviceId,
    pub name: String,
    pub model_uuid: String,
    pub session: Option<String>,
}

/// A disconnected controller's live assignment, kept until it reconnects, the session ends
/// or the session is given another controller.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Remembered {
    pub name: String,
    pub model_uuid: String,
    pub session: String,
}

/// An input source on the desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Keyboard,
    Pointer,
    Pad(DeviceId),
}

/// Complete guest input for one session: buttons in Azahar `Settings::NativeButton` bit
/// order, touch in native bottom-screen pixels, sticks `[x, y]` in -1..=1, x right, y up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct GuestState {
    pub buttons: u32,
    pub touch: Option<(u16, u16)>,
    pub circle_pad: [f32; 2],
    pub c_stick: [f32; 2],
}

/// 3DS button bits (`common/settings.h` `NativeButton::Values`).
pub mod bits {
    pub const A: u32 = 1 << 0;
    pub const B: u32 = 1 << 1;
    pub const X: u32 = 1 << 2;
    pub const Y: u32 = 1 << 3;
    pub const UP: u32 = 1 << 4;
    pub const DOWN: u32 = 1 << 5;
    pub const LEFT: u32 = 1 << 6;
    pub const RIGHT: u32 = 1 << 7;
    pub const L: u32 = 1 << 8;
    pub const R: u32 = 1 << 9;
    pub const START: u32 = 1 << 10;
    pub const SELECT: u32 = 1 << 11;
    pub const ZL: u32 = 1 << 14;
    pub const ZR: u32 = 1 << 15;
    pub const HOME: u32 = 1 << 16;
}

/// Default positional controller layout, until mapping profiles exist (lotus#63).
///
/// Face buttons and the rest follow Azahar's own default for XInput-style pads,
/// `input_common/sdl/sdl_impl.cpp` `xinput_to_3ds_mapping`: 3DS A from the east button, B from
/// south, X from north, Y from west, Start/Select from Start/Back, Home from Guide. That table
/// leaves ZL/ZR unbound; here they take the analog triggers, which sit where the New 3DS
/// triggers do. The left stick drives the Circle Pad and the right stick the C-stick.
#[must_use]
pub fn default_button(button: Button) -> Option<u32> {
    Some(match button {
        Button::East => bits::A,
        Button::South => bits::B,
        Button::North => bits::X,
        Button::West => bits::Y,
        Button::DpadUp => bits::UP,
        Button::DpadDown => bits::DOWN,
        Button::DpadLeft => bits::LEFT,
        Button::DpadRight => bits::RIGHT,
        Button::LeftShoulder => bits::L,
        Button::RightShoulder => bits::R,
        Button::LeftTrigger => bits::ZL,
        Button::RightTrigger => bits::ZR,
        Button::Start => bits::START,
        Button::Select => bits::SELECT,
        Button::Mode => bits::HOME,
        Button::LeftThumb | Button::RightThumb => return None,
    })
}

/// What one controller connection physically holds, whether or not it is assigned.
#[derive(Debug, Clone, Default)]
struct Pad {
    info: Option<DeviceInfo>,
    held: BTreeSet<Button>,
    axes: BTreeMap<Axis, f32>,
    session: Option<String>,
    /// Held when assigned; ignored until released.
    suppressed: BTreeSet<Button>,
    /// Sticks off centre when assigned (circle pad, C-stick); ignored until centred.
    sticks_suppressed: [bool; 2],
}

impl Pad {
    fn stick(&self, x: Axis, y: Axis) -> [f32; 2] {
        [
            self.axes.get(&x).copied().unwrap_or(0.0),
            self.axes.get(&y).copied().unwrap_or(0.0),
        ]
    }

    fn sticks(&self) -> [[f32; 2]; 2] {
        [
            self.stick(Axis::LeftX, Axis::LeftY),
            self.stick(Axis::RightX, Axis::RightY),
        ]
    }

    /// Drops suppression of anything released or centred since assignment.
    fn settle(&mut self) {
        let held = &self.held;
        self.suppressed.retain(|button| held.contains(button));
        let sticks = self.sticks();
        for (suppressed, stick) in self.sticks_suppressed.iter_mut().zip(sticks) {
            // gilrs' default deadzone filter reports a centred stick as exactly 0.
            if stick == [0.0, 0.0] {
                *suppressed = false;
            }
        }
    }

    fn contribution(&self) -> GuestState {
        let buttons = self
            .held
            .iter()
            .filter(|button| !self.suppressed.contains(button))
            .filter_map(|button| default_button(*button))
            .fold(0, |bits, bit| bits | bit);
        let [circle, cstick] = self.sticks();
        GuestState {
            buttons,
            touch: None,
            circle_pad: if self.sticks_suppressed[0] {
                [0.0; 2]
            } else {
                circle
            },
            c_stick: if self.sticks_suppressed[1] {
                [0.0; 2]
            } else {
                cstick
            },
        }
    }
}

/// Counters for `frontend_metrics.controllers.router`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct RouterMetrics {
    /// Composed states returned for submission.
    pub published: u64,
    /// Controller events from a connection the router does not know (dropped connect).
    pub unknown_device: u64,
    /// Full resynchronizations from the device service's snapshot.
    pub resyncs: u64,
}

/// See the module documentation.
#[derive(Debug, Default)]
pub struct Router {
    keyboard: Option<(String, u32)>,
    pointer: Option<(String, Option<(u16, u16)>)>,
    pads: BTreeMap<DeviceId, Pad>,
    published: BTreeMap<String, GuestState>,
    metrics: RouterMetrics,
    /// Assignments of controllers that disconnected, for the reconnect policy.
    remembered: Vec<Remembered>,
    /// Armed by [`Router::identify`]; the next controller to press a button is reported.
    identifying: bool,
    identified: Option<DeviceId>,
    identifications: u64,
}

fn magnitude(stick: [f32; 2]) -> f32 {
    stick[0].hypot(stick[1])
}

impl Router {
    #[must_use]
    pub fn metrics(&self) -> RouterMetrics {
        self.metrics
    }

    /// Controller connection -> assigned session.
    #[must_use]
    pub fn assignments(&self) -> BTreeMap<DeviceId, Option<String>> {
        self.pads
            .iter()
            .map(|(id, pad)| (*id, pad.session.clone()))
            .collect()
    }

    /// Connected controllers in ID order.
    #[must_use]
    pub fn pads(&self) -> Vec<PadStatus> {
        self.pads
            .iter()
            .map(|(id, pad)| PadStatus {
                id: *id,
                name: pad
                    .info
                    .as_ref()
                    .map(|i| i.name.clone())
                    .unwrap_or_default(),
                model_uuid: pad
                    .info
                    .as_ref()
                    .map(|i| i.model_uuid.clone())
                    .unwrap_or_default(),
                session: pad.session.clone(),
            })
            .collect()
    }

    /// Assignments waiting for their controller to reconnect, or for a choice when the
    /// reconnect was ambiguous.
    #[must_use]
    pub fn remembered(&self) -> &[Remembered] {
        &self.remembered
    }

    /// Report the next controller that presses a button. The press is routed as usual.
    pub fn identify(&mut self) {
        self.identifying = true;
    }

    /// Whether identification is armed, the last controller identified, and how many
    /// identifications completed (so a UI can tell a repeat of the same controller).
    #[must_use]
    pub fn identification(&self) -> (bool, Option<DeviceId>, u64) {
        (self.identifying, self.identified, self.identifications)
    }

    /// Sessions the keyboard and the pointer target.
    pub fn targets(&self) -> impl Iterator<Item = String> + '_ {
        self.keyboard
            .iter()
            .map(|(session, _)| session.clone())
            .chain(self.pointer.iter().map(|(session, _)| session.clone()))
    }

    /// Composed state of `session` from every source targeting it.
    #[must_use]
    pub fn compose(&self, session: &str) -> GuestState {
        let mut contributions: Vec<GuestState> = Vec::new();
        if let Some((_, buttons)) = self.keyboard.as_ref().filter(|(s, _)| s == session) {
            contributions.push(GuestState {
                buttons: *buttons,
                ..GuestState::default()
            });
        }
        let touch = self
            .pointer
            .as_ref()
            .filter(|(s, _)| s == session)
            .and_then(|(_, touch)| *touch);
        contributions.extend(
            self.pads
                .values()
                .filter(|pad| pad.session.as_deref() == Some(session))
                .map(Pad::contribution),
        );
        let pick = |stick: fn(&GuestState) -> [f32; 2]| {
            contributions
                .iter()
                .map(stick)
                // max_by keeps the last maximum; reverse so ties go to the earlier source.
                .rev()
                .max_by(|a, b| magnitude(*a).total_cmp(&magnitude(*b)))
                .unwrap_or_default()
        };
        GuestState {
            buttons: contributions.iter().fold(0, |bits, c| bits | c.buttons),
            touch,
            circle_pad: pick(|c| c.circle_pad),
            c_stick: pick(|c| c.c_stick),
        }
    }

    /// Recompose `sessions` and return those whose state changed.
    fn publish(&mut self, sessions: BTreeSet<String>) -> Vec<(String, GuestState)> {
        let mut changed = Vec::new();
        for session in sessions {
            let state = self.compose(&session);
            let previous = self.published.get(&session).copied().unwrap_or_default();
            if state != previous {
                self.published.insert(session.clone(), state);
                self.metrics.published += 1;
                changed.push((session, state));
            }
        }
        changed
    }

    /// Keyboard buttons held for `session` (`None`: no session takes keyboard input).
    pub fn keyboard(&mut self, session: Option<&str>, buttons: u32) -> Vec<(String, GuestState)> {
        let mut touched: BTreeSet<String> = self.keyboard.iter().map(|(s, _)| s.clone()).collect();
        self.keyboard = session.map(|session| (session.to_string(), buttons));
        touched.extend(session.map(str::to_string));
        self.publish(touched)
    }

    /// Pointer touch for `session` (`None` touch: not touching).
    pub fn pointer(
        &mut self,
        session: Option<&str>,
        touch: Option<(u16, u16)>,
    ) -> Vec<(String, GuestState)> {
        let mut touched: BTreeSet<String> = self.pointer.iter().map(|(s, _)| s.clone()).collect();
        self.pointer = session.map(|session| (session.to_string(), touch));
        touched.extend(session.map(str::to_string));
        self.publish(touched)
    }

    /// Assign a controller to `session`, or unassign it with `None`.
    ///
    /// Policy (lotus#62 v1): one controller per session, one session per controller, at most
    /// [`MAX_ASSIGNED`] pairs. A session that already has another controller is refused
    /// unless `replace`, which unassigns that one first. Moving an assigned controller to
    /// another session is always allowed. Assigning clears what was remembered for that
    /// session, so a later reconnect does not take it back.
    pub fn assign(
        &mut self,
        device: DeviceId,
        session: Option<&str>,
        replace: bool,
    ) -> Result<Vec<(String, GuestState)>, AssignError> {
        let Some(pad) = self.pads.get(&device) else {
            return Err(AssignError::UnknownDevice(device));
        };
        let Some(target) = session else {
            return Ok(self.route_pad(device, None));
        };
        if pad.session.as_deref() == Some(target) {
            return Ok(Vec::new());
        }
        let occupant = self
            .pads
            .iter()
            .find(|(id, pad)| **id != device && pad.session.as_deref() == Some(target))
            .map(|(id, _)| *id);
        if let Some(occupant) = occupant.filter(|_| !replace) {
            return Err(AssignError::SessionOccupied(occupant));
        }
        let others = self
            .pads
            .iter()
            .filter(|(id, pad)| **id != device && Some(**id) != occupant && pad.session.is_some())
            .count();
        if others >= MAX_ASSIGNED {
            return Err(AssignError::CapReached);
        }
        let mut changed = Vec::new();
        if let Some(occupant) = occupant {
            changed.extend(self.route_pad(occupant, None));
        }
        self.remembered.retain(|entry| entry.session != target);
        changed.extend(self.route_pad(device, Some(target)));
        Ok(changed)
    }

    /// Reconnect policy: a new connection takes back a remembered assignment only when that
    /// is unambiguous. Exactly one remembered assignment has its model, no other unassigned
    /// connected controller shares the model, and [`Router::assign`] allows it (the session
    /// has no controller, the cap holds). Otherwise it stays unassigned and the entry waits
    /// for a choice. Identical pads share a model UUID, so two of them never resolve alone.
    fn reconnect(&mut self, device: DeviceId) -> Vec<(String, GuestState)> {
        let Some(model) = self
            .pads
            .get(&device)
            .and_then(|pad| pad.info.as_ref())
            .map(|info| info.model_uuid.clone())
        else {
            return Vec::new();
        };
        let mut entries = self.remembered.iter().filter(|e| e.model_uuid == model);
        let (Some(entry), None) = (entries.next(), entries.next()) else {
            return Vec::new();
        };
        let session = entry.session.clone();
        let twins = self.pads.iter().any(|(id, pad)| {
            *id != device
                && pad.session.is_none()
                && pad.info.as_ref().is_some_and(|i| i.model_uuid == model)
        });
        if twins {
            return Vec::new();
        }
        self.assign(device, Some(&session), false)
            .unwrap_or_default()
    }

    /// Remove a connection, remembering its assignment for [`Router::reconnect`].
    fn disconnect(&mut self, device: DeviceId) -> Option<String> {
        let pad = self.pads.remove(&device)?;
        let session = pad.session?;
        if let Some(info) = pad.info {
            self.remembered.push(Remembered {
                name: info.name,
                model_uuid: info.model_uuid,
                session: session.clone(),
            });
        }
        Some(session)
    }

    /// Route a controller connection to `session`, or to none, without policy. The previous
    /// session loses the controller's contribution; whatever the controller holds now is
    /// suppressed for the new one until released or centred.
    fn route_pad(&mut self, device: DeviceId, session: Option<&str>) -> Vec<(String, GuestState)> {
        let Some(pad) = self.pads.get_mut(&device) else {
            return Vec::new();
        };
        if pad.session.as_deref() == session {
            return Vec::new();
        }
        let mut touched: BTreeSet<String> = pad.session.iter().cloned().collect();
        pad.session = session.map(str::to_string);
        pad.suppressed = pad.held.clone();
        pad.sticks_suppressed = pad.sticks().map(|stick| stick != [0.0, 0.0]);
        touched.extend(session.map(str::to_string));
        self.publish(touched)
    }

    /// Apply one event from the device service.
    pub fn event(&mut self, event: &Event) -> Vec<(String, GuestState)> {
        let session = match &event.kind {
            EventKind::Connected { info, state } => {
                // A connection is new by construction (ids are never reused).
                self.pads.insert(
                    event.device,
                    Pad {
                        info: Some(info.clone()),
                        ..Pad::default()
                    },
                );
                let mut changed = self.resync(event.device, state);
                changed.extend(self.reconnect(event.device));
                return changed;
            }
            EventKind::Disconnected => self.disconnect(event.device),
            EventKind::Button { button, pressed } => {
                if !self.pads.contains_key(&event.device) {
                    self.metrics.unknown_device += 1;
                    return Vec::new();
                }
                if *pressed && self.identifying {
                    self.identifying = false;
                    self.identified = Some(event.device);
                    self.identifications += 1;
                }
                let Some(pad) = self.pads.get_mut(&event.device) else {
                    return Vec::new();
                };
                if *pressed {
                    pad.held.insert(*button);
                } else {
                    pad.held.remove(button);
                }
                pad.settle();
                pad.session.clone()
            }
            EventKind::Axis { axis, value } => {
                let Some(pad) = self.pads.get_mut(&event.device) else {
                    self.metrics.unknown_device += 1;
                    return Vec::new();
                };
                pad.axes.insert(*axis, value.clamp(-1.0, 1.0));
                pad.settle();
                pad.session.clone()
            }
        };
        self.publish(session.into_iter().collect())
    }

    /// Replace what a controller holds with the device service's snapshot, after events were
    /// dropped (`Diagnostics::dropped`) or at connect.
    pub fn resync(&mut self, device: DeviceId, state: &InputSnapshot) -> Vec<(String, GuestState)> {
        let Some(pad) = self.pads.get_mut(&device) else {
            return Vec::new();
        };
        pad.held = state.buttons.iter().copied().collect();
        pad.axes = state
            .axes
            .iter()
            .map(|(axis, value)| (*axis, value.clamp(-1.0, 1.0)))
            .collect();
        pad.settle();
        self.metrics.resyncs += 1;
        let session = pad.session.clone();
        self.publish(session.into_iter().collect())
    }

    /// Bring controllers in line with the device service's list: add connections whose
    /// `Connected` event was lost, drop those whose `Disconnected` was, and with
    /// `lost_events` replace every known controller's state with its snapshot.
    pub fn reconcile(
        &mut self,
        devices: &[crate::Device],
        lost_events: bool,
    ) -> Vec<(String, GuestState)> {
        let live: BTreeSet<DeviceId> = devices.iter().map(|device| device.id).collect();
        let mut changed = self.retain_devices(&live);
        for device in devices {
            let known = self.pads.contains_key(&device.id);
            if !known {
                self.pads.insert(
                    device.id,
                    Pad {
                        info: Some(device.info.clone()),
                        ..Pad::default()
                    },
                );
            }
            if !known || lost_events {
                changed.extend(self.resync(device.id, &device.state));
            }
            if !known {
                changed.extend(self.reconnect(device.id));
            }
        }
        changed
    }

    /// The last state published for `session` may not have reached it (the submission was
    /// refused); the next change publishes the full composed state rather than a diff
    /// against it.
    pub fn invalidate(&mut self, session: &str) {
        self.published.remove(session);
    }

    /// Forget connections the device service no longer lists (a dropped disconnect).
    pub fn retain_devices(&mut self, live: &BTreeSet<DeviceId>) -> Vec<(String, GuestState)> {
        let gone: Vec<DeviceId> = self
            .pads
            .keys()
            .filter(|id| !live.contains(id))
            .copied()
            .collect();
        let mut touched = BTreeSet::new();
        for id in gone {
            if let Some(session) = self.disconnect(id) {
                touched.insert(session);
            }
        }
        self.publish(touched)
    }

    /// Drop every source's link to a session that ended. Nothing is returned for it: there is
    /// no worker left to release.
    pub fn remove_session(&mut self, session: &str) {
        if self.keyboard.as_ref().is_some_and(|(s, _)| s == session) {
            self.keyboard = None;
        }
        if self.pointer.as_ref().is_some_and(|(s, _)| s == session) {
            self.pointer = None;
        }
        for pad in self.pads.values_mut() {
            if pad.session.as_deref() == Some(session) {
                pad.session = None;
            }
        }
        self.remembered.retain(|entry| entry.session != session);
        self.published.remove(session);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn event(device: u64, kind: EventKind) -> Event {
        Event {
            device: DeviceId(device),
            kind,
            time: SystemTime::UNIX_EPOCH,
        }
    }

    fn connect(router: &mut Router, device: u64) {
        router.event(&event(
            device,
            EventKind::Connected {
                info: crate::DeviceInfo {
                    name: "pad".into(),
                    model_uuid: "same-model".into(),
                    vendor_id: None,
                    product_id: None,
                    mapped: true,
                },
                state: InputSnapshot::default(),
            },
        ));
    }

    fn button(
        router: &mut Router,
        device: u64,
        button: Button,
        pressed: bool,
    ) -> Vec<(String, GuestState)> {
        router.event(&event(device, EventKind::Button { button, pressed }))
    }

    fn axis(router: &mut Router, device: u64, axis: Axis, value: f32) -> Vec<(String, GuestState)> {
        router.event(&event(device, EventKind::Axis { axis, value }))
    }

    fn two_pads() -> Router {
        let mut router = Router::default();
        connect(&mut router, 1);
        connect(&mut router, 2);
        router
            .assign(DeviceId(1), Some("a"), false)
            .expect("assign");
        router
            .assign(DeviceId(2), Some("b"), false)
            .expect("assign");
        router
    }

    #[test]
    fn keyboard_on_a_never_touches_a_controller_on_b() {
        let mut router = two_pads();
        assert_eq!(
            button(&mut router, 2, Button::East, true),
            vec![(
                "b".into(),
                GuestState {
                    buttons: bits::A,
                    ..GuestState::default()
                }
            )]
        );
        // Keyboard presses and releases on A, and moves focus: B is never republished.
        for out in [
            router.keyboard(Some("a"), bits::B),
            router.keyboard(Some("a"), 0),
            router.keyboard(None, 0),
        ] {
            assert!(out.iter().all(|(session, _)| session == "a"), "{out:?}");
        }
        assert_eq!(router.compose("b").buttons, bits::A);
    }

    #[test]
    fn sources_on_one_session_are_ored_and_released_separately() {
        let mut router = two_pads();
        router.keyboard(Some("a"), bits::L);
        button(&mut router, 1, Button::South, true);
        assert_eq!(router.compose("a").buttons, bits::L | bits::B);
        // Releasing keyboard focus keeps the held pad button.
        assert_eq!(
            router.keyboard(None, 0),
            vec![(
                "a".into(),
                GuestState {
                    buttons: bits::B,
                    ..GuestState::default()
                }
            )]
        );
        // Unplugging the pad keeps a held keyboard key and leaves B alone.
        router.keyboard(Some("a"), bits::R);
        button(&mut router, 2, Button::North, true);
        let out = router.event(&event(1, EventKind::Disconnected));
        assert_eq!(
            out,
            vec![(
                "a".into(),
                GuestState {
                    buttons: bits::R,
                    ..GuestState::default()
                }
            )]
        );
        assert_eq!(router.compose("b").buttons, bits::X);
    }

    #[test]
    fn largest_stick_wins_and_ties_go_to_the_earlier_source() {
        let mut router = Router::default();
        connect(&mut router, 1);
        connect(&mut router, 2);
        router.route_pad(DeviceId(1), Some("a"));
        // Two controllers on one session only by direct routing; the v1 policy forbids it.
        router.route_pad(DeviceId(2), Some("a"));
        axis(&mut router, 1, Axis::LeftX, 0.3);
        axis(&mut router, 2, Axis::LeftY, -0.8);
        assert_eq!(router.compose("a").circle_pad, [0.0, -0.8]);
        axis(&mut router, 2, Axis::LeftY, 0.3);
        assert_eq!(router.compose("a").circle_pad, [0.3, 0.0]);
        axis(&mut router, 1, Axis::RightY, 1.0);
        assert_eq!(router.compose("a").c_stick, [0.0, 1.0]);
    }

    #[test]
    fn pointer_owns_touch_and_follows_its_target() {
        let mut router = two_pads();
        router.pointer(Some("a"), Some((100, 50)));
        assert_eq!(router.compose("a").touch, Some((100, 50)));
        // Retargeting releases A's touch and gives B none until it is touched.
        let out = router.pointer(Some("b"), None);
        assert_eq!(out, vec![("a".into(), GuestState::default())]);
        assert_eq!(router.compose("b").touch, None);
    }

    #[test]
    fn reassignment_neutralizes_the_old_session_and_suppresses_held_input() {
        let mut router = two_pads();
        button(&mut router, 1, Button::East, true);
        axis(&mut router, 1, Axis::LeftX, 1.0);
        let out = router
            .assign(DeviceId(1), Some("b"), true)
            .expect("replace");
        // A is released; B gains nothing from what was held at the moment of assignment.
        assert_eq!(out, vec![("a".into(), GuestState::default())]);
        assert_eq!(router.compose("b"), GuestState::default());
        // A fresh press on B counts; the old one only after release.
        button(&mut router, 1, Button::South, true);
        assert_eq!(router.compose("b").buttons, bits::B);
        button(&mut router, 1, Button::East, false);
        button(&mut router, 1, Button::East, true);
        assert_eq!(router.compose("b").buttons, bits::A | bits::B);
        // The stick stays suppressed until centred.
        axis(&mut router, 1, Axis::LeftX, 0.5);
        assert_eq!(router.compose("b").circle_pad, [0.0, 0.0]);
        axis(&mut router, 1, Axis::LeftX, 0.0);
        axis(&mut router, 1, Axis::LeftX, -0.5);
        assert_eq!(router.compose("b").circle_pad, [-0.5, 0.0]);
    }

    #[test]
    fn identical_pads_stay_distinct_and_unassigned_pads_route_nowhere() {
        let mut router = Router::default();
        connect(&mut router, 1);
        connect(&mut router, 2);
        assert!(button(&mut router, 1, Button::East, true).is_empty());
        router
            .assign(DeviceId(2), Some("a"), false)
            .expect("assign");
        // Pad 1 holds A but is unassigned; only pad 2 reaches the session.
        button(&mut router, 2, Button::South, true);
        assert_eq!(router.compose("a").buttons, bits::B);
        assert_eq!(router.assignments().len(), 2);
    }

    #[test]
    fn ended_sessions_and_lost_events_are_cleaned_up() {
        let mut router = two_pads();
        button(&mut router, 1, Button::East, true);
        router.keyboard(Some("a"), bits::L);
        router.remove_session("a");
        assert_eq!(router.assignments()[&DeviceId(1)], None);
        // Input for the ended session goes nowhere and is not replayed later.
        assert!(button(&mut router, 1, Button::South, true).is_empty());
        router
            .assign(DeviceId(1), Some("c"), false)
            .expect("assign");
        assert_eq!(router.compose("c"), GuestState::default());
        // A dropped release is repaired from the service snapshot.
        button(&mut router, 2, Button::West, true);
        let out = router.resync(DeviceId(2), &InputSnapshot::default());
        assert_eq!(out, vec![("b".into(), GuestState::default())]);
        // A dropped disconnect is repaired from the device list.
        button(&mut router, 2, Button::West, true);
        let out = router.retain_devices(&BTreeSet::from([DeviceId(1)]));
        assert_eq!(out, vec![("b".into(), GuestState::default())]);
        // A lost connect is recovered from the list, unassigned, with its held state.
        let snapshot = InputSnapshot {
            buttons: vec![Button::East],
            ..InputSnapshot::default()
        };
        let device = crate::Device {
            id: DeviceId(7),
            info: crate::DeviceInfo {
                name: "pad".into(),
                model_uuid: "same-model".into(),
                vendor_id: None,
                product_id: None,
                mapped: true,
            },
            state: snapshot,
            events: 0,
        };
        assert!(router.reconcile(&[device], false).is_empty());
        assert_eq!(router.assignments()[&DeviceId(7)], None);
        assert!(!router.assignments().contains_key(&DeviceId(1)));
        // Events of an unknown connection are counted, not routed.
        assert!(button(&mut router, 9, Button::East, true).is_empty());
        assert_eq!(router.metrics().unknown_device, 1);
    }

    #[test]
    fn unchanged_states_are_not_republished() {
        let mut router = two_pads();
        assert_eq!(button(&mut router, 1, Button::East, true).len(), 1);
        // Thumb clicks have no 3DS button: no change, nothing to send.
        assert!(button(&mut router, 1, Button::LeftThumb, true).is_empty());
        assert!(router.keyboard(Some("a"), 0).is_empty());
    }

    fn connect_model(router: &mut Router, device: u64, model: &str) {
        router.event(&event(
            device,
            EventKind::Connected {
                info: crate::DeviceInfo {
                    name: format!("pad {model}"),
                    model_uuid: model.into(),
                    vendor_id: None,
                    product_id: None,
                    mapped: true,
                },
                state: InputSnapshot::default(),
            },
        ));
    }

    #[test]
    fn one_controller_per_session_and_at_most_two_pairs() {
        let mut router = two_pads();
        connect(&mut router, 3);
        // A third pad is listed but cannot take an occupied session without `replace`.
        assert_eq!(
            router.assign(DeviceId(3), Some("a"), false),
            Err(AssignError::SessionOccupied(DeviceId(1)))
        );
        // Nor a third session past the cap.
        assert_eq!(
            router.assign(DeviceId(3), Some("c"), false),
            Err(AssignError::CapReached)
        );
        assert_eq!(
            router.assign(DeviceId(9), Some("c"), false),
            Err(AssignError::UnknownDevice(DeviceId(9)))
        );
        // Replacing frees pad 1 and keeps the cap.
        button(&mut router, 1, Button::East, true);
        let out = router
            .assign(DeviceId(3), Some("a"), true)
            .expect("replace");
        assert_eq!(out, vec![("a".into(), GuestState::default())]);
        assert_eq!(router.assignments()[&DeviceId(1)], None);
        assert_eq!(router.assignments()[&DeviceId(3)].as_deref(), Some("a"));
        // An assigned pad may move to a free session; the cap counts pairs, not moves.
        router.assign(DeviceId(2), None, false).expect("unassign");
        router
            .assign(DeviceId(1), Some("c"), false)
            .expect("assign c");
        router
            .assign(DeviceId(1), Some("b"), false)
            .expect("move to b");
        assert_eq!(
            router.pads().iter().filter(|p| p.session.is_some()).count(),
            2
        );
    }

    #[test]
    fn identify_reports_the_next_controller_to_press() {
        let mut router = two_pads();
        button(&mut router, 1, Button::East, true);
        assert_eq!(router.identification(), (false, None, 0));
        router.identify();
        // Releases and sticks do not identify; the next press does, once.
        button(&mut router, 1, Button::East, false);
        axis(&mut router, 1, Axis::LeftX, 0.5);
        assert_eq!(router.identification(), (true, None, 0));
        button(&mut router, 2, Button::South, true);
        assert_eq!(router.identification(), (false, Some(DeviceId(2)), 1));
        // The press was routed as usual.
        assert_eq!(router.compose("b").buttons, bits::B);
        button(&mut router, 1, Button::South, true);
        assert_eq!(router.identification(), (false, Some(DeviceId(2)), 1));
    }

    #[test]
    fn a_reconnect_takes_back_its_session_only_when_unambiguous() {
        let mut router = Router::default();
        connect_model(&mut router, 1, "pro");
        connect_model(&mut router, 2, "pad");
        router.assign(DeviceId(1), Some("a"), false).expect("a");
        router.assign(DeviceId(2), Some("b"), false).expect("b");
        // A unique model reconnects (as a new connection ID) to its live session.
        router.event(&event(1, EventKind::Disconnected));
        assert_eq!(router.remembered().len(), 1);
        connect_model(&mut router, 11, "pro");
        assert_eq!(router.assignments()[&DeviceId(11)].as_deref(), Some("a"));
        assert!(router.remembered().is_empty());

        // Two identical pads were assigned; one reconnects while its twin is idle: ambiguous.
        let mut router = Router::default();
        connect_model(&mut router, 1, "same");
        connect_model(&mut router, 2, "same");
        router.assign(DeviceId(1), Some("a"), false).expect("a");
        router.assign(DeviceId(2), Some("b"), false).expect("b");
        router.event(&event(1, EventKind::Disconnected));
        router.event(&event(2, EventKind::Disconnected));
        connect_model(&mut router, 3, "same");
        assert_eq!(router.assignments()[&DeviceId(3)], None);
        assert_eq!(router.remembered().len(), 2);
        // Choosing explicitly clears that session's entry; the other still waits.
        router
            .assign(DeviceId(3), Some("b"), false)
            .expect("choice");
        assert_eq!(router.remembered().len(), 1);
        // With one entry left, the next pad of the model resolves, unless an unassigned twin
        // is already connected.
        let mut twins = Router::default();
        connect_model(&mut twins, 6, "same");
        twins.remembered = router.remembered.clone();
        connect_model(&mut twins, 7, "same");
        assert_eq!(twins.assignments()[&DeviceId(7)], None);
        connect_model(&mut router, 4, "same");
        assert_eq!(router.assignments()[&DeviceId(4)].as_deref(), Some("a"));

        // A session that ended is forgotten; its controller returns unassigned.
        let mut router = Router::default();
        connect_model(&mut router, 1, "pro");
        router.assign(DeviceId(1), Some("a"), false).expect("a");
        router.event(&event(1, EventKind::Disconnected));
        router.remove_session("a");
        connect_model(&mut router, 2, "pro");
        assert_eq!(router.assignments()[&DeviceId(2)], None);

        // A session given another controller meanwhile is not taken back.
        let mut router = Router::default();
        connect_model(&mut router, 1, "pro");
        connect_model(&mut router, 2, "pad");
        router.assign(DeviceId(1), Some("a"), false).expect("a");
        router.event(&event(1, EventKind::Disconnected));
        router
            .assign(DeviceId(2), Some("a"), false)
            .expect("other pad");
        connect_model(&mut router, 3, "pro");
        assert_eq!(router.assignments()[&DeviceId(3)], None);
    }
}
