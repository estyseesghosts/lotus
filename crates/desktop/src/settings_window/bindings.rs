use gpui::{Keystroke, Subscription};
use lotus_input::layout::{reserved_key, Action, Binding, Direction, Layer, Resolved};
use lotus_input::router::GuestState;
use lotus_input::{Axis, Button as PadButton};
use silvestris::controls::{Button, ControlSize, PopUpButton};
use silvestris::ActiveTheme;

use super::*;

#[derive(Default)]
pub(super) struct Editor {
    pub(super) layout: Option<Resolved>,
    pub(super) pending: Vec<Target>,
    capture: Option<&'static Descriptor>,
    consumed: std::collections::BTreeSet<String>,
    hint: Option<String>,
    session: Option<String>,
    live: Option<GuestState>,
    _interceptor: Option<Subscription>,
}

impl Editor {
    pub(super) fn clear_scope(&mut self) {
        self.layout = None;
        self.session = None;
        self.live = None;
    }
}

impl SettingsWindow {
    pub(super) fn init_binding_editor(&mut self, window: &Window, cx: &mut Context<Self>) {
        let handle = window.window_handle();
        let this = cx.entity().downgrade();
        self.binding_editor._interceptor =
            Some(cx.intercept_keystrokes(move |event, window, cx| {
                if window.window_handle() == handle {
                    let _ = this.update(cx, |view, cx| view.capture_key(&event.keystroke, cx));
                }
            }));

        if self.controllers.routing.is_some() {
            cx.spawn(async move |this, cx| loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(33))
                    .await;
                if this
                    .update(cx, |view, cx| {
                        if view.binding_preview_visible() {
                            view.refresh_binding_preview(cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            })
            .detach();
        }
    }

    pub(super) fn cancel_binding_capture(&mut self, cx: &mut Context<Self>) {
        if self.binding_editor.capture.take().is_some() {
            self.binding_editor.hint = None;
            cx.notify();
        }
    }

    fn capture_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        // GPUI intercepts before Settings' Escape handler and app actions.
        if self.binding_editor.consumed.contains(&keystroke.key) {
            cx.stop_propagation();
            return;
        }
        let Some(descriptor) = self.binding_editor.capture else {
            return;
        };
        cx.stop_propagation();
        self.binding_editor.consumed.insert(keystroke.key.clone());
        if keystroke.key == "escape" {
            self.cancel_binding_capture(cx);
            return;
        }
        let reason = if keystroke.modifiers.modified() {
            Some("Choose a key without modifiers.".into())
        } else {
            reserved_key(&keystroke.key).map(|reason| {
                format!(
                    "{} is reserved for {reason}. Choose another key.",
                    keystroke.key
                )
            })
        };
        if let Some(reason) = reason {
            self.binding_editor.hint = Some(reason);
            cx.notify();
            return;
        }
        self.cancel_binding_capture(cx);
        self.add_binding(
            descriptor,
            Binding::Keyboard {
                key: keystroke.key.clone(),
            },
            cx,
        );
    }

    pub(super) fn release_captured_key(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.binding_editor.consumed.remove(key) {
            cx.stop_propagation();
        }
    }

    pub(super) fn bindings(&self, descriptor: &Descriptor) -> &[Binding] {
        action(descriptor)
            .and_then(|action| self.binding_editor.layout.as_ref()?.bindings.get(&action))
            .map_or(&[], |(bindings, _)| bindings)
    }

    pub(super) fn binding_source(&self, descriptor: &Descriptor) -> String {
        let source = action(descriptor)
            .and_then(|action| self.binding_editor.layout.as_ref()?.bindings.get(&action))
            .map(|(_, source)| source);
        match source {
            Some(Layer::Preset) => "From selected preset",
            Some(Layer::Global) if self.target == Target::Global => "Global override",
            Some(Layer::Global) => "Inherited from Global",
            Some(Layer::Card) if matches!(self.target, Target::Card(_)) => "Card override",
            Some(Layer::Card) => "Inherited from card",
            Some(Layer::Session) => "Temporary override",
            None => "",
        }
        .into()
    }

    fn add_binding(&mut self, descriptor: &Descriptor, binding: Binding, cx: &mut Context<Self>) {
        let mut bindings = self.bindings(descriptor).to_vec();
        if !bindings.contains(&binding) {
            bindings.push(binding);
            self.write(descriptor, Some(serde_json::json!(bindings)), cx);
        }
    }

    pub(super) fn render_binding(
        &self,
        descriptor: &'static Descriptor,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let key = &descriptor.key;
        let mut row = div().flex().flex_wrap().items_center().gap(px(6.0));
        let bindings = self.bindings(descriptor);
        if self.binding_editor.layout.is_none() {
            row = row.child("Bindings unavailable");
        } else if bindings.is_empty() {
            row = row.child("Unbound");
        }
        for (index, binding) in bindings.iter().enumerate() {
            let label = binding_label(binding);
            row = row.child(if enabled {
                let binding = binding.clone();
                Button::new(format!("remove-{key}-{index}"), format!("{label} ×"))
                    .size(ControlSize::Mini)
                    .tooltip(format!("Remove {label} from {}", descriptor.label))
                    .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                        let mut bindings = view.bindings(descriptor).to_vec();
                        if let Some(index) = bindings.iter().position(|entry| *entry == binding) {
                            bindings.remove(index);
                            view.write(descriptor, Some(serde_json::json!(bindings)), cx);
                        }
                    }))
                    .into_any_element()
            } else {
                div().child(label).into_any_element()
            });
        }

        let capturing = self
            .binding_editor
            .capture
            .is_some_and(|active| active.key == *key);
        let can_add =
            enabled && matches!(descriptor.kind, Kind::Bindings { max } if bindings.len() < max);
        if can_add {
            row = row.child(
                Button::new(
                    format!("capture-{key}"),
                    if capturing { "Cancel" } else { "Add Key…" },
                )
                .size(ControlSize::Mini)
                .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                    if capturing {
                        view.cancel_binding_capture(cx);
                    } else {
                        view.start_binding_capture(descriptor, cx);
                    }
                })),
            );
        }
        if capturing {
            row = row.child(self.binding_editor.hint.clone().unwrap_or_default());
        } else if can_add {
            let buttons = PadButton::ALL
                .into_iter()
                .map(|button| Binding::Button { button });
            let axes = Axis::ALL.into_iter().flat_map(|axis| {
                [Direction::Negative, Direction::Positive]
                    .map(|direction| Binding::Axis { axis, direction })
            });
            row = row
                .child(self.binding_menu(descriptor, "button", "Add Pad Button…", buttons, cx))
                .child(self.binding_menu(descriptor, "axis", "Add Pad Axis…", axes, cx));
        }
        row
    }

    fn binding_menu(
        &self,
        descriptor: &'static Descriptor,
        id: &str,
        label: &str,
        options: impl Iterator<Item = Binding>,
        cx: &mut Context<Self>,
    ) -> PopUpButton {
        let existing = self.bindings(descriptor);
        let options: Vec<_> = options
            .filter(|binding| !existing.contains(binding))
            .collect();
        let items = std::iter::once(SharedString::from(label.to_string()))
            .chain(
                options
                    .iter()
                    .map(|binding| SharedString::from(binding_label(binding))),
            )
            .collect();
        PopUpButton::new(
            format!("add-pad-{id}-{}", descriptor.key),
            descriptor.label.clone(),
            items,
        )
        .selected(Some(0))
        .on_choose(cx.listener(move |view, index: &usize, _, cx| {
            if let Some(binding) = index.checked_sub(1).and_then(|i| options.get(i)) {
                view.add_binding(descriptor, binding.clone(), cx);
            }
        }))
    }

    pub(super) fn binding_window_deactivated(&mut self, cx: &mut Context<Self>) {
        self.cancel_binding_capture(cx);
        self.binding_editor.consumed.clear();
    }

    fn start_binding_capture(&mut self, descriptor: &'static Descriptor, cx: &mut Context<Self>) {
        self.binding_editor.capture = Some(descriptor);
        self.binding_editor.hint = Some("Press a key. Escape cancels.".into());
        cx.notify();
    }

    fn preview_session(&self) -> Option<&str> {
        let sessions = &self.controllers.sessions;
        if let Target::Secondary(id) = &self.target {
            return sessions
                .iter()
                .find(|(session, _)| session == id)
                .map(|(id, _)| id.as_str());
        }
        self.binding_editor
            .session
            .as_deref()
            .filter(|id| sessions.iter().any(|(session, _)| session == id))
    }

    fn refresh_binding_preview(&mut self, cx: &mut Context<Self>) {
        let live = self
            .preview_session()
            .zip(self.controllers.routing.as_ref())
            .map(|(session, routing)| routing.guest_state(session));
        if live != self.binding_editor.live {
            self.binding_editor.live = live;
            cx.notify();
        }
    }

    pub(super) fn render_binding_preview(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let sessions = self.controllers.sessions.clone();
        let mut panel = div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(div().font_weight(FontWeight::BOLD).child("3DS Layout"));
        if matches!(self.target, Target::Secondary(_)) {
            if self.preview_session().is_none() {
                panel = panel.child("This secondary session has stopped.");
            }
        } else if !sessions.is_empty() {
            panel = panel.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child("Live input")
                    .child(
                        PopUpButton::new(
                            "binding-preview-session",
                            "Choose a session",
                            std::iter::once(SharedString::from("Choose a session"))
                                .chain(
                                    sessions
                                        .iter()
                                        .map(|(_, label)| SharedString::from(label.clone())),
                                )
                                .collect(),
                        )
                        .selected(
                            sessions
                                .iter()
                                .position(|(id, _)| Some(id.as_str()) == self.preview_session())
                                .map_or(Some(0), |index| Some(index + 1)),
                        )
                        .on_choose(cx.listener(
                            move |view, index: &usize, _, cx| {
                                view.binding_editor.session = index
                                    .checked_sub(1)
                                    .and_then(|i| sessions.get(i))
                                    .map(|(id, _)| id.clone());
                                view.refresh_binding_preview(cx);
                                cx.notify();
                            },
                        )),
                    ),
            );
        }
        let theme = cx.theme();
        let colors = &theme.colors;
        let live = self.binding_editor.live;
        let mut controls = div()
            .p(px(8.0))
            .rounded_md()
            .border_1()
            .border_color(silvestris::convert::hsla(colors.separator))
            .flex()
            .flex_col()
            .gap(px(6.0));
        let line = |actions: &[(Action, &str)]| {
            div()
                .flex()
                .gap(px(5.0))
                .items_center()
                .justify_center()
                .children(actions.iter().map(|&(action, label)| {
                    let key = format!("lotus.input.bind.{}", action.id());
                    let setting = Registry::builtin()
                        .get(&key)
                        .expect("registered 3DS action");
                    let bindings = self.bindings(setting);
                    let mapped = bindings
                        .iter()
                        .map(binding_label)
                        .collect::<Vec<_>>()
                        .join(", ");
                    let pressed = live.is_some_and(|state| action_pressed(action, state));
                    Button::new(format!("layout-{}", action.id()), label.to_owned())
                        .size(ControlSize::Mini)
                        .style(silvestris::controls::ButtonStyle::Textured)
                        .selected(pressed)
                        .min_width(30.0)
                        .tooltip(if mapped.is_empty() {
                            format!("{}: Unbound", setting.label)
                        } else {
                            format!("{}: {mapped}", setting.label)
                        })
                        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                            view.reveal_binding(setting, cx);
                        }))
                }))
        };
        use Action::*;
        let column = |label: &'static str| {
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(4.0))
                .child(label)
        };
        let circle = column("Circle Pad")
            .child(line(&[(CirclePadUp, "↑")]))
            .child(line(&[(CirclePadLeft, "←"), (CirclePadRight, "→")]))
            .child(line(&[(CirclePadDown, "↓")]));
        let dpad = column("D-pad")
            .child(line(&[(Up, "↑")]))
            .child(line(&[(Left, "←"), (Right, "→")]))
            .child(line(&[(Down, "↓")]));
        let cstick = column("C-stick")
            .child(line(&[(CStickUp, "↑")]))
            .child(line(&[(CStickLeft, "←"), (CStickRight, "→")]))
            .child(line(&[(CStickDown, "↓")]));
        let face = column("Face Buttons")
            .child(line(&[(X, "X")]))
            .child(line(&[(Y, "Y"), (A, "A")]))
            .child(line(&[(B, "B")]));
        controls = controls
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(line(&[(Zl, "ZL"), (L, "L")]))
                    .child(line(&[(R, "R"), (Zr, "ZR")])),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .gap(px(8.0))
                    .child(circle)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .w(px(180.0))
                            .h(px(82.0))
                            .rounded_md()
                            .border_1()
                            .border_color(silvestris::convert::hsla(colors.separator))
                            .child("Touch Screen"),
                    )
                    .child(cstick),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .gap(px(8.0))
                    .child(dpad)
                    .child(line(&[
                        (Select, "Select"),
                        (Home, "Home"),
                        (Start, "Start"),
                    ]))
                    .child(face),
            );
        panel = panel.child(controls);
        if let Some(state) = live {
            let [x, y] = state.circle_pad;
            let [cx, cy] = state.c_stick;
            panel = panel.child(format!(
                "Circle Pad {x:+.2}, {y:+.2}    C-stick {cx:+.2}, {cy:+.2}"
            ));
        }
        panel
    }
}

fn action(descriptor: &Descriptor) -> Option<Action> {
    let id = descriptor.key.strip_prefix("lotus.input.bind.")?;
    serde_json::from_value(Value::String(id.to_owned())).ok()
}

fn binding_label(binding: &Binding) -> String {
    use Binding::*;
    match binding {
        Keyboard { key } => key.to_uppercase(),
        Button { button } => format!("{button:?}"),
        Axis { axis, direction } => format!(
            "{axis:?} {}",
            match direction {
                Direction::Positive => "+",
                Direction::Negative => "−",
            }
        ),
    }
}

fn action_pressed(action: Action, state: GuestState) -> bool {
    use Action::*;
    match action {
        CirclePadUp => state.circle_pad[1] > 0.0,
        CirclePadDown => state.circle_pad[1] < 0.0,
        CirclePadLeft => state.circle_pad[0] < 0.0,
        CirclePadRight => state.circle_pad[0] > 0.0,
        CStickUp => state.c_stick[1] > 0.0,
        CStickDown => state.c_stick[1] < 0.0,
        CStickLeft => state.c_stick[0] < 0.0,
        CStickRight => state.c_stick[0] > 0.0,
        _ => action.bit().is_some_and(|bit| state.buttons & bit != 0),
    }
}

#[cfg(all(test, debug_assertions))]
mod tests {
    use super::*;
    use gpui::{KeyBinding, Modifiers, TestAppContext, VisualTestContext};
    use lotus_input::{DeviceId, DeviceInfo, Event, EventKind, InputSnapshot};
    use lotus_protocol::envelope::SessionUuid;
    use lotus_session::supervisor::{HostConfig, SessionKind};
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::{Duration, SystemTime};

    gpui::actions!(settings_binding_test, [TestShortcut]);

    #[test]
    fn mounted_secondary_bindings_capture_and_reset_without_saving_a_card_override() {
        let worker = std::env::var_os("LOTUS_MOCK_NATIVE").map_or_else(
            || {
                std::env::current_exe()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join(format!("lotus-mock-native{}", std::env::consts::EXE_SUFFIX))
            },
            std::path::PathBuf::from,
        );
        let storage =
            std::env::temp_dir().join(format!("lotus-bind-ui-{}", SessionUuid::generate()));
        std::fs::create_dir_all(storage.join("cards/dev")).unwrap();
        let asset = storage.join("fixture.3dsx");
        std::fs::write(&asset, b"3DSX").unwrap();
        let mut config = HostConfig::new(worker, &storage);
        config.allow_control = true;
        config.assets.insert("fixture".into(), asset);
        let host = Arc::new(Supervisor::new(config).unwrap());
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let secondary = runtime
            .block_on(host.create(
                SessionKind::Secondary {
                    parent: Some("dev".into()),
                },
                Some("fixture"),
                Actor::Host,
            ))
            .unwrap()
            .id;
        let input = crate::input_routing::InputRouting::new(host.clone());
        let device = DeviceId(1);
        let event = |kind| {
            input.event(&Event {
                device,
                kind,
                time: SystemTime::now(),
            })
        };
        event(EventKind::Connected {
            info: DeviceInfo {
                name: "Test Pad".into(),
                model_uuid: "mock".into(),
                vendor_id: None,
                product_id: None,
                mapped: true,
            },
            state: InputSnapshot::default(),
        });
        input.assign(device, Some(&secondary), false).unwrap();

        let shortcut_count = Rc::new(Cell::new(0));
        let mut app = TestAppContext::single();
        app.update(|cx| {
            silvestris::test_support::initialize(silvestris::ThemePreference::default(), cx);
            cx.bind_keys([KeyBinding::new("ctrl-j", TestShortcut, None)]);
            let count = shortcut_count.clone();
            cx.on_action(move |_: &TestShortcut, _| count.set(count.get() + 1));
        });
        let (main, _) = app.add_window_view(LotusWindow::test_preview);
        let (settings, cx) = app.add_window_view(|window, cx| {
            let mut view = SettingsWindow::new(
                main.downgrade(),
                Some(host.clone()),
                true,
                Target::Global,
                window,
                cx,
            );
            view.controllers.routing = Some(input.clone());
            view.show_bindings(window, cx);
            view
        });
        cx.simulate_resize(gpui::size(px(780.), px(580.)));
        cx.simulate_scale_factor_change(1.5);
        let draw = |cx: &mut VisualTestContext| {
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
        };
        let click = |cx: &mut VisualTestContext, selector| {
            let bounds = cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("missing {selector}"));
            cx.simulate_click(bounds.center(), Modifiers::default());
            draw(cx);
        };
        let descriptor = Registry::builtin().get("lotus.input.bind.a").unwrap();
        let wait_for_source = |cx: &mut VisualTestContext, source: &str| {
            for _ in 0..100 {
                draw(cx);
                if cx.update(|_, cx| {
                    let view = settings.read(cx);
                    !view.binding_editor.pending.contains(&view.target)
                        && view.binding_source(descriptor) == source
                }) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("bindings did not refresh to {source}");
        };
        wait_for_source(cx, "From selected preset");
        click(cx, "settings-scope");
        click(cx, "popup-item-2");
        assert_eq!(
            cx.update(|_, cx| settings.read(cx).target.clone()),
            Target::Secondary(secondary.clone())
        );
        wait_for_source(cx, "From selected preset");
        click(cx, "layout-a");
        draw(cx);
        click(cx, "capture-lotus.input.bind.a");
        cx.simulate_keystrokes("ctrl-j");
        assert_eq!(shortcut_count.get(), 0);
        cx.simulate_event(gpui::KeyUpEvent {
            keystroke: Keystroke::parse("ctrl-j").unwrap(),
        });
        assert!(cx.update(|_, cx| settings.read(cx).binding_editor.capture.is_some()));
        cx.simulate_keystrokes("escape escape");
        cx.simulate_event(gpui::KeyUpEvent {
            keystroke: Keystroke::parse("escape").unwrap(),
        });
        assert!(cx.update(|_, cx| settings.read(cx).binding_editor.capture.is_none()));
        cx.simulate_keystrokes("ctrl-j");
        assert_eq!(shortcut_count.get(), 1);
        draw(cx);
        click(cx, "capture-lotus.input.bind.a");
        cx.simulate_keystrokes("j");
        wait_for_source(cx, "Temporary override");
        assert!(cx.update(|_, cx| settings
            .read(cx)
            .bindings(descriptor)
            .contains(&Binding::Keyboard { key: "j".into() })));
        assert_eq!(input.guest_state(&secondary), GuestState::default());

        click(cx, "add-pad-button-lotus.input.bind.a");
        click(cx, "popup-item-1");
        wait_for_source(cx, "Temporary override");
        input.sync_layouts(false);
        event(EventKind::Button {
            button: PadButton::South,
            pressed: true,
        });
        cx.update(|_, cx| settings.update(cx, |view, cx| view.refresh_binding_preview(cx)));
        let state = cx.update(|_, cx| settings.read(cx).binding_editor.live.unwrap());
        assert!(action_pressed(Action::A, state));

        click(cx, "reset-lotus.input.bind.a");
        wait_for_source(cx, "From selected preset");
        assert!(!storage.join("cards/dev/lotus-settings.json").exists());
        runtime.block_on(host.close());
        let _ = std::fs::remove_dir_all(storage);
    }
}
