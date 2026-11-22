use super::*;
use crate::desktop_controller::{GdbAction, GdbFeedback, Update};
use gpui::{Entity, Subscription};
use lotus_gdb::{State, Status};
use silvestris::controls::TextField;
use std::io::ErrorKind;

#[derive(Default)]
pub(super) struct GdbPanel {
    latest: Option<(SessionTarget, Status, Option<GdbFeedback>)>,
    port: Option<Port>,
}

struct Port {
    target: SessionTarget,
    field: Entity<TextField>,
    _subscription: Subscription,
}

impl GdbPanel {
    pub(super) fn refresh(&mut self, update: &Update) -> bool {
        let latest = update
            .gdb_status
            .iter()
            .find(|(target, _)| target.id == update.inspected)
            .map(|(target, status)| {
                (
                    target.clone(),
                    status.clone(),
                    update.gdb_feedback.get(target).cloned(),
                )
            });
        let changed = self.latest != latest;
        self.latest = latest;
        changed
    }
}

impl LotusWindow {
    pub(super) fn ensure_gdb_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.inspection_target();
        if self
            .gdb_panel
            .port
            .as_ref()
            .is_some_and(|port| Some(&port.target) != target.as_ref())
        {
            self.gdb_panel.port = None;
        }
        if self.inspection_panel == Panel::Gdb && self.gdb_panel.port.is_none() {
            if let Some(target) = target {
                let field =
                    command_palette::field(window, cx, "Loopback port", "0 (automatic)", false);
                let subscription = cx.subscribe(&field, |_, _, _, cx| cx.notify());
                self.gdb_panel.port = Some(Port {
                    target,
                    field,
                    _subscription: subscription,
                });
            }
        }
    }

    pub(super) fn render_gdb_panel(
        &self,
        target: Option<&SessionTarget>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = theme::palette(cx);
        let content = div().flex().flex_col().gap_2().p_3().text_sm();
        let Some(target) = target else {
            return content
                .child("Select a session to inspect")
                .into_any_element();
        };
        let Some((_, status, feedback)) = self
            .gdb_panel
            .latest
            .as_ref()
            .filter(|(bound, _, _)| bound == target)
        else {
            return content.child("Waiting for GDB status…").into_any_element();
        };
        let support = status.backend_support();
        let backend = match support {
            Some(true) => format!("{} supports GDB", status.backend),
            Some(false) => format!("Unsupported on {}. GDB requires CITnLO.", status.backend),
            None if status.backend.is_empty() => "Backend support is checked on Start.".into(),
            None => format!("{} support is checked on Start.", status.backend),
        };
        let state = match feedback {
            Some(GdbFeedback::Pending(GdbAction::Start { .. })) => "Starting…",
            Some(GdbFeedback::Pending(GdbAction::Stop)) => "Stopping…",
            _ => match status.state {
                State::Stopped => "Stopped",
                State::Changing => "Starting or stopping…",
                State::Listening if status.leased => "Client connected · Mutation lease held",
                State::Listening => "Listening · No client connected",
            },
        };
        let busy =
            status.state == State::Changing || matches!(feedback, Some(GdbFeedback::Pending(_)));
        let mut content = content
            .child(div().font_weight(FontWeight::SEMIBOLD).child(state))
            .child(div().text_color(palette.text_secondary).child(backend));
        if !self.control {
            content = content.child("Host is read-only");
        }
        if let Some(address) = status.address {
            let bound = target.clone();
            content = content.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(address.to_string())
                    .child(
                        small_button(palette, "gdb-copy", "Copy address", Tone::Plain, !busy)
                            .on_click(cx.listener(move |view, _, _, cx| {
                                if view.inspection_target().as_ref() == Some(&bound)
                                    && view.gdb_panel.latest.as_ref().is_some_and(
                                        |(target, status, _)| {
                                            *target == bound && status.address == Some(address)
                                        },
                                    )
                                {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                        address.to_string(),
                                    ));
                                }
                            })),
                    ),
            );
        }
        let mut actions = div().flex().flex_wrap().gap_1();
        match status.state {
            State::Stopped if support != Some(false) && self.control => {
                if let Some(port) = self
                    .gdb_panel
                    .port
                    .as_ref()
                    .filter(|port| &port.target == target)
                {
                    let raw = port.field.read(cx).text();
                    let value = parse_port(raw);
                    content = content.child(port.field.clone());
                    if value.is_none() {
                        content = content.child("Enter a port from 0 to 65535");
                    }
                    let target = target.clone();
                    actions = actions.child(
                        small_button(
                            palette,
                            "gdb-start",
                            "Start",
                            Tone::Primary,
                            !busy && value.is_some(),
                        )
                        .on_click(cx.listener(move |view, _, _, _| {
                            if let Some(port) = value {
                                view.gdb_command(&target, GdbAction::Start { port });
                            }
                        })),
                    );
                }
            }
            State::Listening => {
                let target = target.clone();
                actions =
                    actions.child(
                        small_button(
                            palette,
                            "gdb-stop",
                            "Stop",
                            Tone::Danger,
                            !busy && self.control,
                        )
                        .on_click(cx.listener(move |view, _, _, _| {
                            view.gdb_command(&target, GdbAction::Stop)
                        })),
                    );
            }
            _ => {}
        }
        if let Some(GdbFeedback::Failed { kind, message }) = feedback {
            let error = match kind {
                ErrorKind::Unsupported => format!("Unsupported · {message}"),
                ErrorKind::PermissionDenied => format!("Control unavailable · {message}"),
                _ => message.clone(),
            };
            content = content.child(error);
        } else if let Some(error) = &status.error {
            content = content.child(error.clone());
        }
        let target = target.clone();
        actions = actions.child(
            small_button(palette, "gdb-status", "Refresh status", Tone::Plain, !busy).on_click(
                cx.listener(move |view, _, _, _| view.gdb_command(&target, GdbAction::Status)),
            ),
        );
        content.child(actions).into_any_element()
    }

    fn gdb_command(&mut self, target: &SessionTarget, action: GdbAction) {
        if self.inspection_panel == Panel::Gdb
            && self.inspection_target().as_ref() == Some(target)
            && (self.control || action == GdbAction::Status)
        {
            self.send(Command::Gdb {
                target: target.clone(),
                action,
            });
        }
    }
}

fn parse_port(raw: &str) -> Option<u16> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Some(0);
    }
    raw.bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| raw.parse().ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use lotus_session::{supervisor::SessionInfo, telemetry::Telemetry};

    #[test]
    fn mounted_gdb_panel_copies_only_the_current_listener_address() {
        let mut app = TestAppContext::single();
        app.update(|cx| {
            silvestris::test_support::initialize(
                silvestris::ThemePreference::explicit(silvestris::ThemeId::GEIST_DARK),
                cx,
            )
        });
        let address = "127.0.0.1:41009";
        let (view, cx) = app.add_window_view(|window, cx| {
            let mut view = LotusWindow::test_preview(window, cx);
            let mut telemetry = Telemetry::new();
            telemetry.state = SessionState::Paused;
            telemetry.epoch = 2;
            telemetry.backend_name = "CITnLO".into();
            let summary = SessionSummary {
                info: SessionInfo {
                    id: "fixture".into(),
                    kind: SessionKind::Secondary { parent: None },
                    asset: Some("fixture.3dsx".into()),
                    created_unix_ms: 1,
                },
                telemetry,
            };
            let target = SessionTarget::from_summary(&summary);
            view.sessions = Arc::new(vec![summary]);
            view.selected = "fixture".into();
            view.inspected = "fixture".into();
            view.control = true;
            view.gdb_panel.latest = Some((
                target,
                Status {
                    state: State::Listening,
                    address: Some(address.parse().unwrap()),
                    leased: true,
                    backend: "CITnLO".into(),
                    error: None,
                },
                None,
            ));
            view.invoke_global(shared_commands::GlobalCommand::Gdb, window, cx);
            view
        });
        cx.simulate_resize(gpui::size(px(1400.), px(900.)));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let copy = cx
            .debug_bounds("gdb-copy")
            .expect("Copy address is visible");
        assert!(cx.debug_bounds("gdb-stop").is_some());
        cx.simulate_click(copy.center(), gpui::Modifiers::default());
        cx.update(|window, cx| {
            assert_eq!(
                cx.read_from_clipboard()
                    .and_then(|item| item.text())
                    .as_deref(),
                Some(address)
            );
            view.update(cx, |view, cx| {
                Arc::make_mut(&mut view.sessions)[0].telemetry.epoch += 1;
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
        assert!(cx.debug_bounds("gdb-copy").is_none());
        assert!(cx.debug_bounds("gdb-stop").is_none());
        assert_eq!(parse_port(""), Some(0));
        assert_eq!(parse_port("65535"), Some(65535));
        assert_eq!(parse_port("65536"), None);
    }
}
