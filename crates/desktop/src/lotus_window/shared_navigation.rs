//! Staged source-list composition. The generic list owns navigation input;
//! Lotus retains worker/card actions and the original SessionDrag payload.
use super::*;
use crate::navigation::{Navigation, SessionTarget, Target as NavigationTarget};
use silvestris::controls::{Button, ButtonStyle, ControlSize, SourceList, SourceListEvent};
use silvestris::core::source_list::SourceRow;

#[derive(Clone)]
enum SourceAction {
    Settings(String),
    Start(String),
    Fork {
        expected: SessionTarget,
        card: String,
        asset: Option<String>,
    },
    Tile {
        expected: SessionTarget,
        revision: u64,
        operation: Operation,
    },
    Stop(SessionTarget),
}

struct RowAction {
    key: &'static str,
    label: &'static str,
    icon: Icon,
    action: SourceAction,
}

impl RowAction {
    fn new(key: &'static str, label: &'static str, icon: Icon, action: SourceAction) -> Self {
        Self {
            key,
            label,
            icon,
            action,
        }
    }
}

impl LotusWindow {
    pub(super) fn make_shared_sources(cx: &mut Context<Self>) -> gpui::Entity<SourceList> {
        let sources = cx.new(|cx| SourceList::new(Vec::new(), cx).expect("empty source list"));
        let icons = cx.weak_entity();
        let accessories = cx.weak_entity();
        let decoration = cx.weak_entity();
        sources.update(cx, |list, cx| {
            list.set_icon_renderer(
                move |role, _, _, _, cx| {
                    icons
                        .read_with(cx, |view, cx| {
                            view.shared_source_icon(theme::palette(cx), role)
                        })
                        .unwrap_or_else(|_| div().into_any_element())
                },
                cx,
            );
            list.set_row_accessory(
                move |row, _, cx| {
                    accessories
                        .update(cx, |view, cx| view.shared_source_actions(row, cx))
                        .unwrap_or_else(|_| div().into_any_element())
                },
                cx,
            );
            list.set_row_decorator(
                move |row, element, _, cx| {
                    // Move the element only after upgrading the owner.
                    let mut element = Some(element);
                    decoration
                        .update(cx, |view, cx| {
                            view.decorate_source_row(row, element.take().unwrap(), cx)
                        })
                        .unwrap_or_else(|_| element.unwrap())
                },
                cx,
            );
        });
        cx.subscribe(&sources, |view, _, event: &SourceListEvent, cx| {
            if let SourceListEvent::Selected(row) = event {
                if let Some(id) = view
                    .navigation
                    .selection(row, &view.sessions)
                    .map(|s| s.info.id.clone())
                {
                    view.select(id);
                    cx.notify();
                }
            }
        })
        .detach();
        sources
    }

    pub(super) fn sync_shared_sources(&mut self, cx: &mut Context<Self>) {
        let navigation = Navigation::project(&self.cards, &self.sessions);
        let branches = navigation.expandable_rows();
        let first: Vec<_> = branches
            .difference(&self.source_branches)
            .cloned()
            .collect();
        self.source_branches = branches;
        let context = (self.workspace.revision, self.control, self.selected.clone());
        let changed = self.navigation.nodes != navigation.nodes
            || self.navigation.targets != navigation.targets
            || self.source_context != context;
        let selected = navigation.selected_row(&self.selected, &self.sessions);
        self.navigation = navigation;
        self.source_context = context;
        self.shared_sources.update(cx, |list, cx| {
            list.replace(self.navigation.nodes.clone(), cx)
                .expect("namespaced source IDs");
            // Previously every copy was visible. Open newly introduced branches once;
            // later polling preserves explicit disclosure choices.
            for id in first {
                list.set_expanded(&id, true, cx);
            }
            if let Some(id) = selected {
                list.select(&id, cx);
            } else {
                list.clear_selection(cx);
            }
            // App-owned accessories depend on target epoch, permission and workspace.
            if changed {
                cx.notify();
            }
        });
    }

    fn source_session(&self, row: &str) -> Option<&SessionTarget> {
        match self.navigation.targets.get(row)? {
            NavigationTarget::Card { primary, .. } => primary.as_ref(),
            NavigationTarget::Session(target) => Some(target),
        }
    }
    fn source_action_current(&self, target: &SessionTarget) -> bool {
        self.control && target.resolve(&self.sessions).is_some()
    }
    fn shared_source_icon(&self, theme: theme::Palette, role: &str) -> AnyElement {
        if let Some(row) = role.strip_prefix("state:") {
            if let Some(summary) = self
                .source_session(row)
                .and_then(|t| t.resolve(&self.sessions))
            {
                return div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(state_dot(
                        theme,
                        summary,
                        matches!(summary.info.kind, SessionKind::Primary { .. }),
                    ))
                    .into_any_element();
            }
        }
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .size(px(8.))
                    .rounded(px(2.))
                    .border_1()
                    .border_color(theme.border_strong),
            )
            .into_any_element()
    }

    fn decorate_source_row(
        &self,
        row: &SourceRow,
        element: AnyElement,
        cx: &Context<Self>,
    ) -> AnyElement {
        let id = row.id.clone();
        let actions = self.source_actions(row);
        let drag = self
            .source_session(&row.id)
            .and_then(|target| target.resolve(&self.sessions))
            .filter(|summary| summary.info.asset.is_some() && summary.telemetry.state.is_active())
            .map(|summary| self.session_drag(&summary.info.id, Source::Row));
        div()
            .id(SharedString::from(format!("source-drag-{id}")))
            .child(element)
            .when(!actions.is_empty(), |el| {
                el.on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |view, event: &gpui::MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        view.clear_popup_commands(cx);
                        let mut entries = Vec::new();
                        for action in &actions {
                            let invoke = action.action.clone();
                            let mut descriptor =
                                silvestris::core::command::CommandDescriptor::action(
                                    &format!("source.{}.{}", id, action.key),
                                    action.label,
                                );
                            if matches!(invoke, SourceAction::Stop(_)) {
                                descriptor = descriptor.destructive();
                            }
                            entries.push(view.popup_action(
                                descriptor,
                                move |view, window, cx| {
                                    view.apply_source_action(&invoke, window, cx);
                                },
                                cx,
                            ));
                        }
                        view.open_popup_at(
                            silvestris::core::command::MenuModel {
                                label: "Session".into(),
                                entries,
                            },
                            event.position,
                            window,
                            cx,
                        );
                    }),
                )
            })
            .when_some(drag, |el, drag| {
                el.on_drag(drag, |drag: &SessionDrag, _, _, cx| {
                    cx.new(|_| DragGhost(drag.label.clone().into()))
                })
            })
            .into_any_element()
    }

    fn source_actions(&self, row: &SourceRow) -> Vec<RowAction> {
        let Some(target) = self.navigation.targets.get(&row.id) else {
            return Vec::new();
        };
        let mut actions = Vec::new();
        if let NavigationTarget::Card {
            card,
            primary: None,
        } = target
        {
            actions.push(RowAction::new(
                "settings",
                "Card Settings",
                Icon::Gear,
                SourceAction::Settings(card.clone()),
            ));
            if self.control {
                actions.push(RowAction::new(
                    "start",
                    "Start Primary Session",
                    Icon::Play,
                    SourceAction::Start(card.clone()),
                ));
            }
            return actions;
        }
        let Some(expected) = self.source_session(&row.id).cloned() else {
            return actions;
        };
        let Some(summary) = expected.resolve(&self.sessions) else {
            return actions;
        };
        if let SessionKind::Primary { card } = &summary.info.kind {
            actions.push(RowAction::new(
                "settings",
                "Card Settings",
                Icon::Gear,
                SourceAction::Settings(card.clone()),
            ));
            if self.control {
                actions.push(RowAction::new(
                    "fork",
                    "Create Secondary from Card",
                    Icon::Plus,
                    SourceAction::Fork {
                        expected: expected.clone(),
                        card: card.clone(),
                        asset: summary.info.asset.clone(),
                    },
                ));
            }
        }
        if let Some(operation) = self.tile_with(summary) {
            actions.push(RowAction::new(
                "tile",
                "Show Beside the Current View",
                Icon::SideBySide,
                SourceAction::Tile {
                    expected: expected.clone(),
                    revision: self.workspace.revision,
                    operation,
                },
            ));
        }
        if self.control {
            let label = if matches!(summary.info.kind, SessionKind::Primary { .. }) {
                "Stop (Card Data Is Kept)"
            } else {
                "Stop and Discard"
            };
            actions.push(RowAction::new(
                "stop",
                label,
                Icon::Close,
                SourceAction::Stop(expected),
            ));
        }
        actions
    }

    fn apply_source_action(
        &mut self,
        action: &SourceAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            SourceAction::Settings(card)
                if self
                    .navigation
                    .targets
                    .contains_key(&crate::navigation::card_row(card)) =>
            {
                self.open_settings(crate::settings_window::Target::Card(card.clone()), cx);
            }
            SourceAction::Start(card)
                if self.control
                    && matches!(
                        self.navigation
                            .targets
                            .get(&crate::navigation::card_row(card)),
                        Some(NavigationTarget::Card { primary: None, .. }),
                    ) =>
            {
                self.create(SessionKind::Primary { card: card.clone() }, None)
            }
            SourceAction::Fork {
                expected,
                card,
                asset,
            } if self.source_action_current(expected) => {
                self.create(
                    SessionKind::Secondary {
                        parent: Some(card.clone()),
                    },
                    asset.clone(),
                );
            }
            SourceAction::Tile {
                expected,
                revision,
                operation,
            } if expected.resolve(&self.sessions).is_some()
                && self.workspace.revision == *revision =>
            {
                self.send(Command::WorkspaceAt(operation.clone(), *revision));
            }
            SourceAction::Stop(expected) if self.source_action_current(expected) => {
                self.confirm_session_action(
                    super::shared_commands::SessionCommand::Stop,
                    expected.clone(),
                    window,
                    cx,
                );
            }
            _ => {}
        }
    }

    fn shared_source_actions(&self, row: &SourceRow, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme::palette(cx);
        let mut container = div()
            .debug_selector(|| format!("source-actions-{}", row.id))
            .flex()
            .items_center()
            .flex_none()
            .gap(px(2.0))
            .children(
                self.source_session(&row.id)
                    .and_then(|target| self.paired_controller_badge(&target.id, theme)),
            );
        for action in self.source_actions(row) {
            let id = SharedString::from(format!("source-{}-{}", action.key, row.id));
            container = container.child(
                Button::new(id, action.label)
                    .style(ButtonStyle::Gradient)
                    .size(ControlSize::Small)
                    .icon(move |ink| icon(action.icon, 12.0, ink))
                    .when(!self.has_confirmation(), |button| {
                        button.tooltip(action.label)
                    })
                    .on_click(cx.listener(move |view, _: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        view.apply_source_action(&action.action, window, cx);
                        cx.notify();
                    })),
            );
        }
        container.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size, Modifiers, TestAppContext, VisualTestContext};
    use lotus_session::{supervisor::SessionInfo, telemetry::Telemetry};
    fn summary(id: &str, kind: SessionKind) -> SessionSummary {
        let mut telemetry = Telemetry::new();
        telemetry.state = SessionState::Paused;
        telemetry.epoch = 1;
        SessionSummary {
            info: SessionInfo {
                id: id.into(),
                kind,
                asset: Some("demo.3dsx".into()),
                created_unix_ms: 1,
            },
            telemetry,
        }
    }
    fn draw(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
    }

    #[test]
    #[cfg(debug_assertions)]
    fn mounted_source_drag_commits_the_previewed_edge_and_shares_its_revision() {
        use lotus_protocol::envelope::SessionUuid;
        use lotus_session::telemetry::Actor;
        use std::time::Instant;

        let worker = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("lotus-mock-native{}", std::env::consts::EXE_SUFFIX));
        let directory =
            std::env::temp_dir().join(format!("lotus-drag-{}", SessionUuid::generate()));
        std::fs::create_dir_all(&directory).unwrap();
        let asset = directory.join("drag.3dsx");
        std::fs::write(&asset, b"3DSX").unwrap();
        let mut config = HostConfig::new(worker, directory.join("store"));
        config.allow_control = true;
        config.assets.insert("demo".into(), asset);
        let mut app = TestAppContext::single();
        app.update(|cx| {
            silvestris::test_support::initialize(
                silvestris::ThemePreference::explicit(silvestris::ThemeId::GEIST_DARK),
                cx,
            )
        });
        let (view, cx) = app.add_window_view(|window, cx| {
            let mut view = LotusWindow::test_preview(window, cx);
            view.controller = Some(DesktopController::start(config, McpMode::Off).unwrap());
            view.preferences.values.sessions_open = true;
            view.preferences.values.sessions_width = 264.;
            view
        });
        let host = cx.update(|_, cx| {
            let controller = view.read(cx).controller.as_ref().unwrap();
            controller
                .commands
                .send(Command::Create {
                    kind: SessionKind::Primary { card: "dev".into() },
                    asset: Some("demo".into()),
                })
                .unwrap();
            controller.host().clone()
        });
        let wait = |cx: &mut VisualTestContext, predicate: &dyn Fn(&LotusWindow) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(8);
            loop {
                let ready = cx.update(|_, cx| {
                    view.update(cx, |view, cx| {
                        view.poll(cx);
                        cx.notify();
                        predicate(view)
                    })
                });
                if ready {
                    draw(cx);
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "workspace did not reach expected state"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        cx.simulate_resize(size(px(1280.), px(880.)));
        wait(cx, &|view| view.sessions.len() == 1);
        cx.update(|_, cx| {
            view.read(cx)
                .controller
                .as_ref()
                .unwrap()
                .commands
                .send(Command::Create {
                    kind: SessionKind::Secondary {
                        parent: Some("dev".into()),
                    },
                    asset: Some("demo".into()),
                })
                .unwrap();
        });
        wait(cx, &|view| {
            view.sessions.len() == 2 && !view.workspace.layout.visible().is_empty()
        });
        let (primary, copy) = cx.update(|_, cx| {
            let view = view.read(cx);
            let id = |primary| {
                view.sessions
                    .iter()
                    .find(|row| matches!(row.info.kind, SessionKind::Primary { .. }) == primary)
                    .unwrap()
                    .info
                    .id
                    .clone()
            };
            (id(true), id(false))
        });
        let single = host
            .update_workspace(
                &Operation::Single {
                    session: primary.clone(),
                },
                None,
                Actor::Mcp,
            )
            .unwrap();
        wait(cx, &|view| view.workspace.revision == single.revision);
        // GPUI accepts a static selector even for dynamically named rows.
        let selector = format!("source-row-session:{copy}").leak();
        let source = cx.debug_bounds(selector).unwrap();
        let start = point(source.left() + px(70.), source.center().y);
        let stage = cx.debug_bounds("drop-stage").unwrap();
        let right = point(stage.right() - px(15.), stage.center().y);
        let mods = Modifiers::default();
        cx.simulate_mouse_down(start, MouseButton::Left, mods);
        cx.simulate_mouse_move(point(start.x + px(20.), start.y), MouseButton::Left, mods);
        draw(cx);
        cx.simulate_mouse_move(right, MouseButton::Left, mods);
        draw(cx);
        cx.update(|_, cx| {
            assert_eq!(
                view.read(cx).drop_target,
                Some((Target::Edge(tiling::Edge::Right), (0.5, 0.0, 0.5, 1.0)))
            );
        });
        cx.simulate_mouse_up(right, MouseButton::Left, mods);
        wait(cx, &|view| {
            view.workspace.layout.visible() == [primary.as_str(), copy.as_str()]
        });
        let tiled = host.workspace().snapshot;
        assert_eq!(tiled.revision, single.revision + 1);
        assert!(matches!(
            tiled.layout,
            Layout::Split {
                axis: Axis::Horizontal,
                ..
            }
        ));
        let vertical = host
            .update_workspace(
                &Operation::SetAxis {
                    axis: Axis::Vertical,
                },
                Some(tiled.revision),
                Actor::Mcp,
            )
            .unwrap();
        wait(cx, &|view| view.workspace == vertical);
        assert_eq!(host.workspace().snapshot, vertical);
        cx.update(|_, cx| view.update(cx, |view, _| drop(view.controller.take())));
        let _ = std::fs::remove_dir_all(directory);
    }
    #[test]
    fn mounted_source_list_selects_target_without_guest_keys_or_refresh_feedback() {
        let mut app = TestAppContext::single();
        app.update(|cx| {
            silvestris::test_support::initialize(
                silvestris::ThemePreference::explicit(silvestris::ThemeId::GEIST_DARK),
                cx,
            )
        });
        let (view, cx) = app.add_window_view(|window, cx| {
            let mut view = LotusWindow::new(None, McpMode::Off, window, cx);
            view.preferences.path = None;
            view.preferences.values.sessions_open = true;
            view.preferences.values.sessions_width = 264.;
            view.control = true;
            view.cards = vec!["dev".into(), "saved".into()];
            view.sessions = vec![
                summary("primary", SessionKind::Primary { card: "dev".into() }),
                summary(
                    "copy",
                    SessionKind::Secondary {
                        parent: Some("dev".into()),
                    },
                ),
            ];
            view.selected = "primary".into();
            view.sync_shared_sources(cx);
            view
        });
        cx.simulate_resize(size(px(1280.), px(880.)));
        draw(cx);
        let copy = cx
            .debug_bounds("source-row-session:copy")
            .expect("shared copy row is mounted");
        cx.simulate_click(
            point(copy.left() + px(70.), copy.center().y),
            Modifiers::default(),
        );
        draw(cx);
        cx.update(|_, cx| assert_eq!(view.read(cx).selected, "copy"));
        let action = cx.debug_bounds("source-actions-session:copy").unwrap();
        cx.simulate_click(action.center(), Modifiers::default());
        draw(cx);
        cx.update(|window, cx| {
            assert!(
                !view.read(cx).focus.is_focused(window),
                "source accessory must not return focus to guest input"
            )
        });
        cx.simulate_keystrokes("z");
        draw(cx);
        cx.update(|_, cx| {
            assert!(
                view.read(cx).keys.is_empty(),
                "navigation focus cannot press guest buttons"
            )
        });
        // Stop from the row opens a sheet. Escape restores the row responder.
        if cx.debug_bounds("sheet-cancel").is_some() {
            cx.simulate_keystrokes("escape");
            draw(cx);
            assert!(cx.debug_bounds("sheet-cancel").is_none());
        }
        cx.simulate_mouse_down(copy.center(), MouseButton::Right, Modifiers::default());
        cx.simulate_mouse_up(copy.center(), MouseButton::Right, Modifiers::default());
        draw(cx);
        cx.update(|_, cx| {
            assert!(view.read(cx).shared_commands.menu.read(cx).is_open());
            assert_eq!(view.read(cx).selected, "copy");
        });
        cx.simulate_keystrokes("escape");
        draw(cx);
        let start = point(copy.left() + px(70.), copy.center().y);
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(
            point(start.x + px(20.), start.y),
            MouseButton::Left,
            Modifiers::default(),
        );
        draw(cx);
        cx.simulate_mouse_move(
            point(start.x + px(50.), start.y),
            MouseButton::Left,
            Modifiers::default(),
        );
        draw(cx);
        cx.update(|window, cx| {
            assert!(
                cx.has_active_drag(),
                "loaded source rows retain drag support"
            );
            cx.stop_active_drag(window);
        });
        cx.simulate_mouse_up(
            point(start.x + px(50.), start.y),
            MouseButton::Left,
            Modifiers::default(),
        );
        draw(cx);
        let card = cx.debug_bounds("source-row-card:dev").unwrap();
        cx.simulate_click(
            point(card.left() + px(10.), card.center().y),
            Modifiers::default(),
        );
        draw(cx);
        cx.update(|_, cx| {
            assert_eq!(
                view.read(cx).selected,
                "copy",
                "disclosure is independent of workspace selection"
            );
            assert!(!view
                .read(cx)
                .shared_sources
                .read(cx)
                .state()
                .rows()
                .iter()
                .any(|r| r.id == "session:copy"));
        });
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.selected = "primary".into(); // controller/MCP-owned snapshot change
                view.keys.insert("sentinel".into());
                view.sync_shared_sources(cx);
                assert!(
                    view.keys.contains("sentinel"),
                    "model sync must not call the selection action"
                );
            })
        });
        draw(cx);
        cx.update(|_, cx| {
            assert_eq!(
                view.read(cx).shared_sources.read(cx).state().selected(),
                Some("card:dev")
            )
        });
    }

    #[test]
    fn epoch_and_permission_changes_invalidate_source_actions() {
        let mut app = TestAppContext::single();
        app.update(|cx| {
            silvestris::test_support::initialize(
                silvestris::ThemePreference::explicit(silvestris::ThemeId::GEIST_DARK),
                cx,
            )
        });
        let (view, cx) = app.add_window_view(|window, cx| {
            let mut view = LotusWindow::new(None, McpMode::Off, window, cx);
            view.preferences.path = None;
            view.control = true;
            view.sessions = vec![summary("copy", SessionKind::Secondary { parent: None })];
            view.sync_shared_sources(cx);
            view
        });
        draw(cx);
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                let expected = view.source_session("session:copy").unwrap().clone();
                assert!(view.source_action_current(&expected));
                view.control = false;
                assert!(!view.source_action_current(&expected));
                view.control = true;
                view.sessions[0].telemetry.epoch += 1;
                assert!(!view.source_action_current(&expected));
                view.sync_shared_sources(cx);
                assert!(view.source_action_current(view.source_session("session:copy").unwrap()));
            })
        });
        draw(cx);
    }
}
