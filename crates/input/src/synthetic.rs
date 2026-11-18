//! Scripted backend for tests: no hardware, deterministic event order.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, SystemTime};

use crate::{Axis, Button, ControllerBackend, DeviceInfo, InputSnapshot, RawEvent};

/// Backend half; give it to [`crate::ControllerService::start`].
pub struct Synthetic {
    events: Receiver<RawEvent>,
}

/// Script half; events are delivered in the order they are sent.
#[derive(Clone)]
pub struct Script {
    events: Sender<RawEvent>,
}

impl Synthetic {
    #[must_use]
    pub fn new() -> (Self, Script) {
        let (sender, receiver) = channel();
        (Self { events: receiver }, Script { events: sender })
    }
}

impl Script {
    pub fn connect(&self, handle: usize, info: DeviceInfo, state: InputSnapshot) {
        let _ = self.events.send(RawEvent::Connected {
            handle,
            info,
            state,
        });
    }

    pub fn disconnect(&self, handle: usize) {
        let _ = self.events.send(RawEvent::Disconnected { handle });
    }

    pub fn button(&self, handle: usize, button: Button, pressed: bool) {
        let time = SystemTime::now();
        let _ = self.events.send(RawEvent::Button {
            handle,
            button,
            pressed,
            time,
        });
    }

    pub fn axis(&self, handle: usize, axis: Axis, value: f32) {
        let time = SystemTime::now();
        let _ = self.events.send(RawEvent::Axis {
            handle,
            axis,
            value,
            time,
        });
    }
}

impl ControllerBackend for Synthetic {
    /// Scripted connects arrive as ordinary events, so nothing is listed up front.
    fn connected(&mut self) -> Vec<RawEvent> {
        Vec::new()
    }

    fn next(&mut self, timeout: Duration) -> Option<RawEvent> {
        self.events.recv_timeout(timeout).ok()
    }
}
