use gpui::prelude::FluentBuilder;
use lotus_input::DeviceId;
use silvestris::controls::{Button, ControlSize, PopUpButton};
use silvestris::preferences::{Form, FormRow};

use super::*;
use crate::{input_routing, session_tree};

#[derive(Default)]
pub(super) struct Panel {
    routing: Option<Arc<input_routing::InputRouting>>,
    devices: Option<input_routing::Controllers>,
    sessions: Vec<(String, String)>,
    error: Option<String>,
}

impl SettingsWindow {
    pub(super) fn init_controllers(&mut self, cx: &App) {
        self.controllers.routing = self
            .lotus
            .read_with(cx, |main, _| main.input_routing())
            .ok()
            .flatten();
    }

    pub(super) fn refresh_controllers(&mut self, cx: &mut Context<Self>) {
        let devices = self
            .controllers
            .routing
            .as_ref()
            .map(|input| input.controllers());
        let sessions = self
            .host
            .iter()
            .flat_map(|host| host.summaries())
            .filter(|row| row.info.asset.is_some() && row.telemetry.state.is_active())
            .map(|row| {
                let label = format!(
                    "{} · {}",
                    session_tree::session_label(&row.info),
                    session_tree::short_id(&row.info.id)
                );
                (row.info.id, label)
            })
            .collect();
        if self.controllers.devices != devices || self.controllers.sessions != sessions {
            self.controllers.devices = devices;
            self.controllers.sessions = sessions;
            cx.notify();
        }
    }

    fn assign_from_settings(
        &mut self,
        device: DeviceId,
        session: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        if !self.control {
            return;
        }
        if let Some(input) = &self.controllers.routing {
            self.controllers.error = input.assign(device, session, false).err();
            self.refresh_controllers(cx);
            cx.notify();
        }
    }

    pub(super) fn render_controllers(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = &self.controllers;
        let root = div().flex().flex_col().gap(px(6.0)).child(
            div()
                .font_weight(FontWeight::BOLD)
                .child("Controller Assignments"),
        );
        let Some(listing) = &panel.devices else {
            return root.child("Controller routing is unavailable.");
        };
        let at_capacity = listing
            .pads
            .iter()
            .filter(|pad| pad.session.is_some())
            .count()
            >= lotus_input::router::MAX_ASSIGNED;
        let mut form = Form::new(200.0);
        for pad in &listing.pads {
            let mut available = vec![(None, SharedString::from("Unassigned"))];
            let mut occupied = Vec::new();
            for (id, label) in &panel.sessions {
                if let Some(other) = listing.pads.iter().find(|other| {
                    other.id != pad.id && other.session.as_deref() == Some(id.as_str())
                }) {
                    occupied.push(format!("{label} (controller {})", other.id));
                } else {
                    available.push((Some(id.clone()), SharedString::from(label.clone())));
                }
            }
            let selected = available.iter().position(|(id, _)| *id == pad.session);
            let device = pad.id;
            let label = format!("{} ({device})", pad.name);
            let capacity_blocked = at_capacity && pad.session.is_none();
            let items = available.iter().map(|(_, label)| label.clone()).collect();
            let assignment = PopUpButton::new(format!("assign-{device}"), label.clone(), items)
                .selected(selected)
                .min_width(150.)
                .disabled(
                    !self.control
                        || capacity_blocked
                        || (pad.session.is_none() && available.len() == 1),
                )
                .on_choose(cx.listener(move |view, index: &usize, _, cx| {
                    if let Some((session, _)) = available.get(*index) {
                        view.assign_from_settings(device, session.as_deref(), cx);
                    }
                }));
            let mut row = FormRow::new(label, assignment);
            if listing.identified == Some(device) {
                row = row.note("Identified by button press.");
            } else if capacity_blocked {
                row = row.note("Two controllers are already assigned. Unassign one first.");
            } else if !occupied.is_empty() {
                row = row.note(format!(
                    "Unassign the other controller first to use {}.",
                    occupied.join(", ")
                ));
            }
            form = form.row(row);
        }
        for remembered in &listing.remembered {
            if let Some((_, session)) = panel
                .sessions
                .iter()
                .find(|(id, _)| *id == remembered.session)
            {
                form = form.row(FormRow::new(
                    remembered.name.clone(),
                    div().child(format!("Disconnected · last assigned to {session}")),
                ));
            }
        }
        root.when(listing.pads.is_empty(), |root| {
            root.child("No controllers connected.")
        })
        .when(
            panel.sessions.is_empty() && !listing.pads.is_empty(),
            |root| root.child("Load a guest in a session to assign a controller."),
        )
        .when(!listing.pads.is_empty(), |root| {
            root.child(
                Button::new(
                    "identify-controller",
                    if listing.identifying {
                        "Press a Controller Button…"
                    } else {
                        "Identify Controller"
                    },
                )
                .size(ControlSize::Small)
                .disabled(listing.identifying)
                .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                    if let Some(input) = &view.controllers.routing {
                        input.identify();
                        view.refresh_controllers(cx);
                    }
                })),
            )
        })
        .child(form)
        .children(panel.error.clone())
    }
}

#[cfg(all(test, debug_assertions))]
mod tests {
    use super::*;
    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use lotus_input::{DeviceInfo, Event, EventKind, InputSnapshot};
    use lotus_protocol::envelope::SessionUuid;
    use lotus_session::supervisor::{HostConfig, SessionKind};
    use std::time::{Duration, Instant, SystemTime};

    #[test]
    fn mounted_controls_pair_identical_pads_to_two_sessions() {
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
            std::env::temp_dir().join(format!("lotus-pad-ui-{}", SessionUuid::generate()));
        std::fs::create_dir_all(&storage).unwrap();
        let asset = storage.join("fixture.3dsx");
        std::fs::write(&asset, b"3DSX").unwrap();
        let mut config = HostConfig::new(worker, &storage);
        config.allow_control = true;
        config.assets.insert("fixture".into(), asset);
        let host = Arc::new(Supervisor::new(config).unwrap());
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (a, b) = runtime.block_on(async {
            let primary = host
                .create(
                    SessionKind::Primary { card: "dev".into() },
                    Some("fixture"),
                    Actor::Host,
                )
                .await
                .unwrap();
            let secondary = host
                .create(
                    SessionKind::Secondary {
                        parent: Some("dev".into()),
                    },
                    Some("fixture"),
                    Actor::Host,
                )
                .await
                .unwrap();
            (primary.id, secondary.id)
        });
        let a_root = storage.join("cards/dev");
        let b_root = storage.join("scratch").join(&b);
        for root in [&a_root, &b_root] {
            std::fs::write(root.join("mock-continuous-run"), []).unwrap();
        }
        runtime.block_on(async {
            for session in [&a, &b] {
                host.run(session, Actor::Host).await.unwrap();
            }
        });
        let input = input_routing::InputRouting::new(host.clone());
        let event = |device, kind| {
            input.event(&Event {
                device,
                kind,
                time: SystemTime::now(),
            })
        };
        for n in [1, 2] {
            event(
                DeviceId(n),
                EventKind::Connected {
                    info: DeviceInfo {
                        name: "Identical Pad".into(),
                        model_uuid: "same".into(),
                        vendor_id: None,
                        product_id: None,
                        mapped: true,
                    },
                    state: InputSnapshot::default(),
                },
            );
        }
        let mut app = TestAppContext::single();
        app.update(|cx| {
            silvestris::test_support::initialize(silvestris::ThemePreference::default(), cx)
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
            view.show_group(Group::Controls, window, cx);
            view
        });
        cx.simulate_resize(gpui::size(px(780.), px(580.)));
        let draw = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| window.draw(cx).clear(cx));
        };
        let choose = |cx: &mut VisualTestContext, selector| {
            let popup = cx.debug_bounds(selector).unwrap();
            cx.simulate_click(popup.center(), Modifiers::default());
            draw(cx);
            let option = cx.debug_bounds("popup-item-1").unwrap();
            cx.simulate_click(option.center(), Modifiers::default());
            draw(cx);
        };
        draw(cx);
        let identify = cx.debug_bounds("identify-controller").unwrap();
        cx.simulate_click(identify.center(), Modifiers::default());
        let button = |device, button, pressed| event(device, EventKind::Button { button, pressed });
        button(DeviceId(2), lotus_input::Button::East, true);
        button(DeviceId(2), lotus_input::Button::East, false);
        assert_eq!(input.controllers().identified, Some(DeviceId(2)));
        cx.update(|_, cx| settings.update(cx, |view, cx| view.refresh_controllers(cx)));
        draw(cx);
        choose(cx, "assign-pad-1");
        choose(cx, "assign-pad-2");
        let pads = input.controllers().pads;
        assert_eq!(pads[0].session.as_deref(), Some(a.as_str()));
        assert_eq!(pads[1].session.as_deref(), Some(b.as_str()));

        button(DeviceId(1), lotus_input::Button::East, true);
        button(DeviceId(2), lotus_input::Button::North, true);
        let has_buttons = |root: &std::path::Path, bits: u32| {
            std::fs::read_to_string(root.join("inputs.jsonl")).is_ok_and(|log| {
                log.lines()
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                    .any(|state| state["buttons"].as_u64() == Some(u64::from(bits)))
            })
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        let a_bit = lotus_input::router::bits::A;
        let b_bit = lotus_input::router::bits::X;
        while !(has_buttons(&a_root, a_bit) && has_buttons(&b_root, b_bit)) {
            assert!(
                Instant::now() < deadline,
                "the workers did not receive their assigned pads"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!has_buttons(&a_root, b_bit) && !has_buttons(&b_root, a_bit));
        event(DeviceId(1), EventKind::Disconnected);
        assert_eq!(
            input.controllers().pads[0].session.as_deref(),
            Some(b.as_str())
        );
        assert_eq!(input.controllers().remembered[0].session, a);
        runtime.block_on(host.close());
        let _ = std::fs::remove_dir_all(storage);
    }
}
