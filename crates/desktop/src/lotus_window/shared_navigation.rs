//! Staged source-list composition. The generic list owns navigation input;
//! Lotus retains worker/card actions and the original SessionDrag payload.
use super::*;
use crate::navigation::{Navigation, SessionTarget, Target as NavigationTarget};
use silvestris::controls::{SourceList, SourceListEvent};
use silvestris::core::source_list::SourceRow;

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
                        .update(cx, |view, _| {
                            view.shared_source_drag(row, element.take().unwrap())
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

    fn shared_source_drag(&self, row: &SourceRow, element: AnyElement) -> AnyElement {
        let summary = self
            .source_session(&row.id)
            .and_then(|t| t.resolve(&self.sessions));
        let Some(summary) =
            summary.filter(|s| s.info.asset.is_some() && s.telemetry.state.is_active())
        else {
            return element;
        };
        let drag = self.session_drag(&summary.info.id, Source::Row);
        div()
            .id(SharedString::from(format!("source-drag-{}", row.id)))
            .child(element)
            .on_drag(drag, |drag: &SessionDrag, _, _, cx| {
                cx.new(|_| DragGhost(drag.label.clone().into()))
            })
            .into_any_element()
    }

    fn shared_source_actions(&self, row: &SourceRow, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme::palette(cx);
        let Some(target) = self.navigation.targets.get(&row.id) else {
            return div().into_any_element();
        };
        let mut actions = div()
            .debug_selector(|| format!("source-actions-{}", row.id))
            .flex()
            .flex_none()
            .gap(px(2.));
        if let NavigationTarget::Card {
            card,
            primary: None,
        } = target
        {
            let card = card.clone();
            let settings = card.clone();
            actions = actions.child(
                icon_button(
                    theme,
                    SharedString::from(format!("source-settings-{}", row.id)),
                    Icon::Gear,
                    "Card settings",
                )
                .on_click(cx.listener(move |view, _, _, cx| {
                    cx.stop_propagation();
                    view.open_settings(crate::settings_window::Target::Card(settings.clone()), cx);
                })),
            );
            if self.control {
                actions = actions.child(
                    icon_button(
                        theme,
                        SharedString::from(format!("source-start-{}", row.id)),
                        Icon::Play,
                        "Start primary session",
                    )
                    .on_click(cx.listener(move |view, _, _, cx| {
                        cx.stop_propagation();
                        if view.control
                            && matches!(
                                view.navigation
                                    .targets
                                    .get(&crate::navigation::card_row(&card)),
                                Some(NavigationTarget::Card { primary: None, .. })
                            )
                        {
                            view.create(SessionKind::Primary { card: card.clone() }, None);
                        }
                    })),
                );
            }
            return actions.into_any_element();
        }
        let Some(expected) = self.source_session(&row.id).cloned() else {
            return actions.into_any_element();
        };
        let Some(summary) = expected.resolve(&self.sessions) else {
            return actions.into_any_element();
        };
        if let SessionKind::Primary { card } = &summary.info.kind {
            let settings = card.clone();
            actions = actions.child(
                icon_button(
                    theme,
                    SharedString::from(format!("source-settings-{}", row.id)),
                    Icon::Gear,
                    "Card settings",
                )
                .on_click(cx.listener(move |view, _, _, cx| {
                    cx.stop_propagation();
                    view.open_settings(crate::settings_window::Target::Card(settings.clone()), cx);
                })),
            );
            if self.control {
                let expected = expected.clone();
                let card = card.clone();
                let asset = summary.info.asset.clone();
                actions = actions.child(
                    icon_button(
                        theme,
                        SharedString::from(format!("source-fork-{}", row.id)),
                        Icon::Plus,
                        "Secondary from this card",
                    )
                    .on_click(cx.listener(move |view, _, _, cx| {
                        cx.stop_propagation();
                        if view.source_action_current(&expected) {
                            view.create(
                                SessionKind::Secondary {
                                    parent: Some(card.clone()),
                                },
                                asset.clone(),
                            );
                        }
                    })),
                );
            }
        }
        if let Some(operation) = self.tile_with(summary) {
            let expected = expected.clone();
            let revision = self.workspace.revision;
            actions = actions.child(
                icon_button(
                    theme,
                    SharedString::from(format!("source-tile-{}", row.id)),
                    Icon::SideBySide,
                    "Show beside the current view",
                )
                .on_click(cx.listener(move |view, _, _, cx| {
                    cx.stop_propagation();
                    if expected.resolve(&view.sessions).is_some()
                        && view.workspace.revision == revision
                    {
                        view.send(Command::WorkspaceAt(operation.clone(), revision));
                    }
                })),
            );
        }
        if self.control {
            let primary = matches!(summary.info.kind, SessionKind::Primary { .. });
            actions = actions.child(
                icon_button(
                    theme,
                    SharedString::from(format!("source-stop-{}", row.id)),
                    Icon::Close,
                    if primary {
                        "Stop (card data is kept)"
                    } else {
                        "Stop and discard"
                    },
                )
                .on_click(cx.listener(move |view, _, _, cx| {
                    cx.stop_propagation();
                    if view.source_action_current(&expected) {
                        view.send(Command::Stop(expected.id.clone()));
                    }
                })),
            );
        }
        actions.into_any_element()
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
