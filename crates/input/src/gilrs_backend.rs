//! gilrs 0.11.2 backend. gilrs' default filters stay on (`GilrsBuilder::new`: deadzone,
//! jitter and axis-to-D-pad conversion), so D-pads arrive as buttons.

use std::time::Duration;

use gilrs::{EventType, Gamepad, Gilrs, MappingSource};

use crate::{Axis, Button, ControllerBackend, DeviceInfo, InputSnapshot, RawEvent};

pub struct GilrsBackend {
    gilrs: Gilrs,
}

impl GilrsBackend {
    pub fn new() -> Result<Self, String> {
        Gilrs::new()
            .map(|gilrs| Self { gilrs })
            .map_err(|error| error.to_string())
    }
}

const BUTTONS: [(gilrs::Button, Button); 17] = [
    (gilrs::Button::South, Button::South),
    (gilrs::Button::East, Button::East),
    (gilrs::Button::North, Button::North),
    (gilrs::Button::West, Button::West),
    (gilrs::Button::LeftTrigger, Button::LeftShoulder),
    (gilrs::Button::RightTrigger, Button::RightShoulder),
    (gilrs::Button::LeftTrigger2, Button::LeftTrigger),
    (gilrs::Button::RightTrigger2, Button::RightTrigger),
    (gilrs::Button::Select, Button::Select),
    (gilrs::Button::Start, Button::Start),
    (gilrs::Button::Mode, Button::Mode),
    (gilrs::Button::LeftThumb, Button::LeftThumb),
    (gilrs::Button::RightThumb, Button::RightThumb),
    (gilrs::Button::DPadUp, Button::DpadUp),
    (gilrs::Button::DPadDown, Button::DpadDown),
    (gilrs::Button::DPadLeft, Button::DpadLeft),
    (gilrs::Button::DPadRight, Button::DpadRight),
];

const AXES: [(gilrs::Axis, Axis); 6] = [
    (gilrs::Axis::LeftStickX, Axis::LeftX),
    (gilrs::Axis::LeftStickY, Axis::LeftY),
    (gilrs::Axis::RightStickX, Axis::RightX),
    (gilrs::Axis::RightStickY, Axis::RightY),
    (gilrs::Axis::LeftZ, Axis::LeftZ),
    (gilrs::Axis::RightZ, Axis::RightZ),
];

/// gilrs' `LeftTrigger`/`RightTrigger` are the shoulder bumpers and `*Trigger2` the
/// triggers (gilrs `ev/mod.rs`, SDL naming).
fn button(button: gilrs::Button) -> Option<Button> {
    BUTTONS.iter().find(|(g, _)| *g == button).map(|(_, b)| *b)
}

fn axis(axis: gilrs::Axis) -> Option<Axis> {
    AXES.iter().find(|(g, _)| *g == axis).map(|(_, a)| *a)
}

fn connected(handle: usize, pad: &Gamepad<'_>) -> RawEvent {
    let uuid = pad.uuid();
    let mut state = InputSnapshot::default();
    for (g, b) in BUTTONS {
        if pad.is_pressed(g) {
            state.buttons.push(b);
        }
    }
    state.buttons.sort();
    for (g, a) in AXES {
        let value = pad.value(g);
        if value != 0.0 {
            state.axes.insert(a, value);
        }
    }
    RawEvent::Connected {
        handle,
        info: DeviceInfo {
            name: pad.name().to_string(),
            model_uuid: uuid.iter().map(|b| format!("{b:02x}")).collect(),
            vendor_id: pad.vendor_id(),
            product_id: pad.product_id(),
            mapped: pad.mapping_source() != MappingSource::None,
        },
        state,
    }
}

impl ControllerBackend for GilrsBackend {
    fn connected(&mut self) -> Vec<RawEvent> {
        self.gilrs
            .gamepads()
            .map(|(id, pad)| connected(id.into(), &pad))
            .collect()
    }

    fn next(&mut self, timeout: Duration) -> Option<RawEvent> {
        let event = self.gilrs.next_event_blocking(Some(timeout))?;
        let handle: usize = event.id.into();
        let time = event.time;
        match event.event {
            EventType::Connected => Some(connected(handle, &self.gilrs.gamepad(event.id))),
            EventType::Disconnected => Some(RawEvent::Disconnected { handle }),
            EventType::ButtonPressed(b, _) => button(b).map(|button| RawEvent::Button {
                handle,
                button,
                pressed: true,
                time,
            }),
            EventType::ButtonReleased(b, _) => button(b).map(|button| RawEvent::Button {
                handle,
                button,
                pressed: false,
                time,
            }),
            EventType::AxisChanged(a, value, _) => axis(a).map(|axis| RawEvent::Axis {
                handle,
                axis,
                value,
                time,
            }),
            // Analog button values, repeats and force feedback are not used.
            _ => None,
        }
    }
}
