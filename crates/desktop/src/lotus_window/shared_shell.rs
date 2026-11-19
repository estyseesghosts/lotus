//! #131/#132/#138: one measured shell for single and split workspaces.
//! Controller inspection remains explicit per tile, otherwise follows selection.
use super::*;
use super::shared_commands::{GlobalCommand, SessionCommand};
use silvestris::command::CommandButton;
use silvestris::controls::{Button, ButtonStyle, ControlSize};
use silvestris::core::command::CommandId;
use silvestris::core::layout::{self as shared, InspectorPreference, SidebarPreference};
use silvestris::shell::{Shell, ShellContent};
use silvestris::ActiveTheme;

static LOTUS_ART: std::sync::LazyLock<Arc<gpui::Image>> = std::sync::LazyLock::new(|| {
    Arc::new(gpui::Image::from_bytes(
        gpui::ImageFormat::Png,
        include_bytes!("../../assets/lotus.png").to_vec(),
    ))
});

fn legacy_plan(plan: &shared::ShellPlan) -> Plan {
    Plan {
        sessions: plan.sidebar_expanded.then_some(plan.sidebar.width),
        inspector: plan.dock.map(|span| span.width),
        inspector_rail: plan.rail.is_some(),
        squeezed: plan.sidebar_demoted.then_some(Pane::Sessions),
        main: plan.workspace.width,
    }
}

impl LotusWindow {
    // Handbook 05/13 and #127/#133: the frame owns one window-wide unified
    // toolbar; shell measurement receives only the remaining content bounds.
    pub(super) fn render_shared_window(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let title = self
            .selected_summary()
            .map(|s| format!("{} — LOTUS", session_tree::session_label(&s.info)))
            .unwrap_or_else(|| "LOTUS".into());
        let height = cx.theme().metrics.toolbar_height;
        silvestris::chrome::WindowFrame::new(title)
            .toolbar(self.render_shared_toolbar(window, cx), height)
            .bottom_bar(self.render_shared_status(cx))
            .child(self.render_shared_shell(cx))
            .into_any_element()
    }

    fn render_shared_toolbar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::palette(cx);
        let wide = f32::from(window.viewport_size().width) >= 1060.;
        // #129: toolbar, Actions menu and shortcuts share registry IDs, so
        // labels and availability come from one published descriptor.
        let registry = self.shared_commands.registry.clone();
        let button = |id: &'static str, command: CommandId| {
            CommandButton::new(id, command, registry.clone()).size(ControlSize::Small)
        };
        let global = |command: GlobalCommand| CommandId::from(command.id());
        div()
            .id("lotus-unified-toolbar")
            .debug_selector(|| "lotus-unified-toolbar".into())
            .size_full()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(10.))
            // Child controls own focus. Do not send a toolbar click to the
            // root's screen-focus handler (guest input stays released).
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(button("shared-sidebar", global(GlobalCommand::Sidebar)))
            .child(div().w(px(1.)).h(px(18.)).bg(theme.border).mx(px(2.)))
            .child(button("shared-run", self.session_command_id(SessionCommand::Run)))
            .child(button("shared-step", self.session_command_id(SessionCommand::Step)))
            .child(button("shared-open", global(GlobalCommand::Open)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px(px(8.))
                    .text_size(px(11.))
                    .text_color(theme.text_secondary)
                    .truncate()
                    .child(
                        self.selected_summary()
                            .map(|s| session_tree::session_label(&s.info))
                            .unwrap_or_else(|| "No session selected".into()),
                    ),
            )
            .when(wide, |bar| {
                bar.child(button("shared-zoom", global(GlobalCommand::ZoomNext)))
                    .child(button("shared-screens", global(GlobalCommand::ScreensNext)))
            })
            .child(
                Button::new("shared-actions", "Actions")
                    .style(ButtonStyle::Textured)
                    .size(ControlSize::Small)
                    .on_click(cx.listener(|view, event, window, cx| {
                        view.open_shared_actions(event, window, cx)
                    })),
            )
            .child(button("shared-inspector", global(GlobalCommand::Inspector)))
            .child(button("shared-settings", global(GlobalCommand::Settings)))
    }

    fn render_shared_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::palette(cx);
        let mcp = match self.mcp {
            McpLink::Attached => "MCP attached",
            McpLink::Closed => "MCP disconnected",
            McpLink::Off => "MCP off",
        };
        let host = if self.controller.is_none() {
            "No host connected"
        } else if self.control {
            "Control enabled"
        } else {
            "Read-only host"
        };
        div()
            .id("lotus-status")
            .debug_selector(|| "lotus-status".into())
            .size_full()
            .flex()
            .items_center()
            .px(px(12.))
            .pr(px(26.))
            .gap(px(10.))
            .text_size(px(11.))
            .text_color(theme.text_secondary)
            .child(format!(
                "{} session{}",
                self.sessions.len(),
                if self.sessions.len() == 1 { "" } else { "s" }
            ))
            .child(div().flex_1().min_w_0().truncate().child(host))
            .child(mcp)
    }

    pub(super) fn render_shared_disconnected(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme::palette(cx);
        div().id("lotus-disconnected").flex().flex_1().min_h_0().items_center().justify_center()
            .child(div().flex().flex_col().items_center().gap(px(14.)).max_w(px(420.)).px(px(28.))
                .child(gpui::img(LOTUS_ART.clone()).size(px(144.)).object_fit(gpui::ObjectFit::Contain))
                .child(div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).child("Welcome to Lotus"))
                .child(div().text_size(px(13.)).text_color(theme.text_secondary).text_center()
                    .child("Connect a host configuration to open your cards and run Nintendo 3DS sessions."))
                .child(Button::new("empty-settings", "Open Settings").on_click(cx.listener(|view, _, _, cx| view.open_settings(crate::settings_window::Target::Global, cx))))
                .child(div().text_size(px(11.)).text_color(theme.text_secondary).text_center()
                    .child(if self.startup_error.is_empty() { "Start Lotus with a host.json configuration to connect a workspace.".into() } else { format!("Could not connect: {}", self.startup_error) })))
            .into_any_element()
    }

    pub(crate) fn ui_theme(&self) -> silvestris::ThemeId {
        self.preferences.ui.theme_id()
    }

    pub(crate) fn set_ui_theme(&mut self, id: silvestris::ThemeId, cx: &mut Context<Self>) {
        self.preferences.ui.theme_family = id.family;
        self.preferences.ui.appearance = id.appearance;
        silvestris::set_theme_preference(silvestris::ThemePreference::explicit(id), cx);
        self.preferences.save();
        cx.notify();
    }
    fn queue_shell_preferences_save(&mut self, cx: &mut Context<Self>) {
        // Shared divider callbacks run on every movement. Coalesce disk writes
        // instead of synchronously writing desktop.json on every input event.
        self.preference_save = Some(cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
            let _ = view.update(cx, |view, _| view.preferences.save());
        }));
    }
    pub(super) fn shared_inspector_target(&self) -> Option<&str> {
        let explicit = self.inspect_tile.as_deref().filter(|id| {
            self.split()
                && self.workspace.layout.visible().contains(id)
                && self.sessions.iter().any(|s| s.info.id == *id)
        });
        explicit.or_else(|| {
            self.sessions
                .iter()
                .any(|s| s.info.id == self.selected)
                .then_some(self.selected.as_str())
        })
    }

    fn shared_sidebar_preference(&self) -> SidebarPreference {
        SidebarPreference {
            expanded: self.preferences.values.sessions_open,
            width: self.preferences.values.sessions_width,
        }
    }

    fn shared_inspector_preference(&self) -> InspectorPreference {
        InspectorPreference {
            visible: self.preferences.values.inspector_open,
            pinned: self.preferences.ui.inspector_pinned,
            width: self.preferences.values.inspector_width,
        }
    }

    // Event handlers can consult the planner; actual children are built only
    // from Shell::measured's plan, including its retained hysteresis state.
    pub(super) fn shared_layout(&self, window: &Window) -> Plan {
        let previous = self.shared_plan.get();
        legacy_plan(&shared::plan(shared::PlanInput {
            viewport: window.viewport_size().width.into(),
            geometry: shared::ShellGeometry::LOTUS_LEGACY,
            sidebar: self.shared_sidebar_preference(),
            inspector: self.shared_inspector_preference(),
            rail_enabled: true,
            previous: previous.as_ref(),
        }))
    }

    pub(super) fn render_shared_shell(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme::palette(cx);
        let owner = cx.entity();
        Shell::measured(
            "lotus-shell",
            self.shared_sidebar_preference(),
            self.shared_inspector_preference(),
            true,
            move |plan, size, window, cx| {
                owner.update(cx, |view, cx| {
                    view.shared_plan.set(Some(*plan));
                    let legacy = legacy_plan(plan);
                    let sidebar = view.render_sidebar(&legacy, cx);
                    let sidebar = view.render_drop_sidebar(sidebar, cx).into_any_element();
                    let rail = view.render_rail(&legacy, cx);
                    let workspace = view.render_workspace(&legacy, size.height.into(), window, cx);
                    let target = view.shared_inspector_target().unwrap_or("");
                    let title = format!(
                        "{} · {} {}",
                        view.inspection_panel.label(),
                        view.session_name(target),
                        session_tree::short_id(target)
                    );
                    let inspector = plan.dock.or(plan.overlay).map(|span| {
                        let ready = !target.is_empty() && target == view.inspected;
                        let selector = format!(
                            "lotus-inspector-{}-{target}",
                            if ready { "ready" } else { "waiting" }
                        );
                        let content = if ready {
                            view.render_inspector(span.width, false, cx)
                                .into_any_element()
                        } else {
                            tile_message(
                                theme,
                                if target.is_empty() {
                                    "No session to inspect"
                                } else {
                                    "Switching inspector…"
                                },
                            )
                        };
                        div()
                            .id("shared-inspector-content")
                            .debug_selector(move || selector.clone())
                            .size_full()
                            .child(content)
                            .into_any_element()
                    });
                    ShellContent {
                        sidebar,
                        sidebar_rail: rail,
                        workspace,
                        inspector,
                        inspector_title: title.into(),
                        inspector_rail: Some(
                            view.render_inspector_rail(&legacy, cx).into_any_element(),
                        ),
                    }
                })
            },
        )
        .on_inspector_key_down({
            let owner = cx.entity();
            move |event, window, cx| {
                owner.update(cx, |view, cx| {
                    if event.keystroke.key == "escape" && view.menu != Menu::Closed {
                        view.menu = Menu::Closed;
                        cx.notify();
                        true
                    } else if event.keystroke.modifiers.control {
                        if event.keystroke.key == "," {
                            view.open_settings(crate::settings_window::Target::Global, cx);
                        } else {
                            view.key_down(event, window, cx);
                        }
                        cx.notify();
                        true
                    } else {
                        false
                    }
                })
            }
        })
        .on_sidebar_change(cx.listener(|view, preference: &SidebarPreference, _, cx| {
            view.set_pane(
                Pane::Sessions,
                PaneState {
                    open: preference.expanded,
                    width: preference.width,
                },
            );
            view.queue_shell_preferences_save(cx);
            cx.notify();
        }))
        .on_inspector_change(
            cx.listener(|view, preference: &InspectorPreference, _, cx| {
                view.preferences.ui.inspector_pinned = preference.pinned;
                view.set_pane(
                    Pane::Inspector,
                    PaneState {
                        open: preference.visible,
                        width: preference.width,
                    },
                );
                view.queue_shell_preferences_save(cx);
                cx.notify();
            }),
        )
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size, Modifiers, TestAppContext, VisualTestContext};
    use lotus_session::{supervisor::SessionInfo, telemetry::Telemetry};

    fn session(id: &str) -> SessionSummary {
        let mut telemetry = Telemetry::new();
        telemetry.state = SessionState::Paused;
        telemetry.epoch = 1;
        SessionSummary {
            info: SessionInfo {
                id: id.into(),
                kind: SessionKind::Secondary { parent: None },
                asset: Some("fixture.3dsx".into()),
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
    fn frame_budgets_chrome_and_keeps_toolbar_controls_reachable_at_minimum_width() {
        for theme in silvestris::ThemeId::ALL {
            let mut app = TestAppContext::single();
            app.update(|cx| {
                silvestris::test_support::initialize(
                    silvestris::ThemePreference::explicit(theme),
                    cx,
                )
            });
            let (view, cx) = app.add_window_view(LotusWindow::test_preview);
            for width in [720., 1280.] {
                cx.simulate_resize(size(px(width), px(880.)));
                draw(cx);
                let metrics = silvestris::core::theme(theme).metrics;
                let workspace = cx.debug_bounds("shell-workspace").unwrap();
                let toolbar = cx.debug_bounds("lotus-unified-toolbar").unwrap();
                let status = cx.debug_bounds("lotus-status").unwrap();
                assert_eq!(toolbar.top(), px(metrics.titlebar_height));
                assert_eq!(toolbar.bottom(), workspace.top());
                assert_eq!(workspace.bottom(), status.top());
                assert_eq!(status.bottom(), px(880.));
                for control in [
                    "shared-sidebar",
                    "shared-run",
                    "shared-step",
                    "shared-open",
                    "shared-actions",
                    "shared-inspector",
                    "shared-settings",
                ] {
                    let bounds = cx.debug_bounds(control).unwrap();
                    assert!(
                        bounds.left() >= px(0.) && bounds.right() <= px(width),
                        "{theme:?} {width} {control}: {bounds:?}"
                    );
                    assert!(bounds.top() >= toolbar.top() && bounds.bottom() <= toolbar.bottom());
                }
                // The no-host toolbar still exposes navigation/settings, and
                // disabled Run cannot synthesize a guest control action.
                let run = cx.debug_bounds("shared-run").unwrap();
                cx.simulate_click(run.center(), Modifiers::default());
                draw(cx);
                cx.update(|_, cx| assert!(view.read(cx).keys.is_empty()));
                let inspector = cx.debug_bounds("shared-inspector").unwrap();
                cx.simulate_click(inspector.center(), Modifiers::default());
                draw(cx);
                cx.update(|_, cx| assert!(view.read(cx).preferences.values.inspector_open));
                // Close before the next size; the persistent rail remains.
                cx.simulate_click(inspector.center(), Modifiers::default());
                draw(cx);
                cx.update(|_, cx| assert!(!view.read(cx).preferences.values.inspector_open));
            }
        }
    }

    #[test]
    fn divider_saves_coalesce_and_write_the_latest_preference() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("lotus-divider-{}-{nonce}.json", std::process::id()));
        let mut app = TestAppContext::single();
        app.update(|cx| {
            silvestris::test_support::initialize(
                silvestris::ThemePreference::explicit(silvestris::ThemeId::GEIST_DARK),
                cx,
            )
        });
        let (view, cx) = app.add_window_view(|window, cx| {
            let mut view = LotusWindow::new(None, McpMode::Off, window, cx);
            view.preferences = Store::load(Some(path.clone()));
            view
        });
        for width in [350., 411.] {
            cx.update(|_, cx| {
                view.update(cx, |view, cx| {
                    view.preferences.values.sessions_width = width;
                    view.queue_shell_preferences_save(cx);
                })
            });
            draw(cx);
            cx.executor().advance_clock(Duration::from_millis(100));
            draw(cx);
            assert!(!path.exists(), "do not write while the divider is moving");
        }
        cx.executor().advance_clock(Duration::from_millis(60));
        draw(cx);
        assert_eq!(Store::load(Some(path.clone())).values.sessions_width, 411.);
        cx.update(|_, cx| {
            view.update(cx, |view, _| {
                view.preferences.path = None;
            })
        });
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn mounted_shell_preserves_overlay_geometry_and_scopes_inspector_input() {
        let mut app = TestAppContext::single();
        app.update(|cx| {
            silvestris::test_support::initialize(
                silvestris::ThemePreference::explicit(silvestris::ThemeId::GEIST_DARK),
                cx,
            )
        });
        let (view, cx) = app.add_window_view(|window, cx| {
            let mut view = LotusWindow::new(None, McpMode::Off, window, cx);
            view.preferences = Store::load(None);
            view.sessions = vec![session("a"), session("b")];
            view.selected = "a".into();
            view.inspected = "a".into();
            view.control = true;
            view.workspace.layout = Layout::Single {
                session: "a".into(),
                screens: Default::default(),
            };
            view.sync_shared_sources(cx);
            view
        });
        cx.simulate_resize(size(px(1400.), px(900.)));
        draw(cx);
        let closed = cx.debug_bounds("shell-workspace").unwrap();
        cx.simulate_keystrokes("ctrl-i");
        draw(cx);
        assert_eq!(cx.debug_bounds("shell-workspace").unwrap(), closed);
        assert!(cx.debug_bounds("lotus-inspector-ready-a").is_some());
        cx.simulate_keystrokes("z");
        cx.update(|_, cx| {
            assert!(view.read(cx).keys.is_empty());
            view.update(cx, |view, cx| {
                view.menu = Menu::InspectorUtility;
                cx.notify();
            });
        });
        draw(cx);
        cx.simulate_keystrokes("escape");
        draw(cx);
        cx.update(|_, cx| {
            assert_eq!(view.read(cx).menu, Menu::Closed);
            assert!(view.read(cx).preferences.values.inspector_open);
        });
        cx.simulate_keystrokes("escape");
        draw(cx);
        cx.update(|window, cx| {
            assert!(!view.read(cx).preferences.values.inspector_open);
            assert!(view.read(cx).focus.is_focused(window));
        });

        // Existing per-tile requests now use this same inspector and wide rail.
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.workspace.layout = Layout::Split {
                    first: "a".into(),
                    second: "b".into(),
                    axis: Axis::Horizontal,
                    ratio: 0.5,
                    screens: Default::default(),
                };
                view.toggle_tile_inspector("b".into());
                cx.notify();
            })
        });
        draw(cx);
        assert_eq!(cx.debug_bounds("shell-workspace").unwrap(), closed);
        assert!(cx.debug_bounds("shell-inspector-rail").is_some());
        assert!(cx.debug_bounds("lotus-inspector-waiting-b").is_some());
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                assert!(
                    view.inspection_target().is_none(),
                    "do not act on previous target data"
                );
                view.inspected = "b".into(); // controller acknowledges the request
                let target = view.inspection_target();
                let snapshot = (
                    target.clone(),
                    Panel::Result,
                    Arc::new(inspection::Inspection::from_value(
                        &serde_json::json!({"old": 1}),
                        Panel::Result,
                    )),
                );
                view.set_inspection(Some(snapshot.clone()), cx);
                view.surface_target = target.clone();
                view.surface_image = Some(Arc::new(RenderImage::new(vec![Frame::new(
                    RgbaImage::new(1, 1),
                )])));
                view.set_inspection(Some(snapshot), cx);
                assert!(
                    view.surface_image.is_some(),
                    "ordinary poll retains the same result image"
                );
                let replacement = (
                    target.clone(),
                    Panel::Result,
                    Arc::new(inspection::Inspection::from_value(
                        &serde_json::json!({"new": 2}),
                        Panel::Result,
                    )),
                );
                view.set_inspection(Some(replacement), cx);
                assert!(
                    view.surface_image.is_none(),
                    "a different command result cannot inherit the image"
                );
                view.surface_target = target.clone();
                view.surface_image = Some(Arc::new(RenderImage::new(vec![Frame::new(
                    RgbaImage::new(1, 1),
                )])));
                view.sessions[1].telemetry.epoch += 1;
                view.retain_current_inspection(cx);
                assert!(view.inspection.is_none());
                assert!(view.surface_image.is_none(), "old epoch image is evicted");
                view.debug_command(&target, D::Registers { core: 0 });
                assert_eq!(
                    view.inspection_panel,
                    Panel::Cpu,
                    "old epoch action rejected"
                );
                view.control = false;
                view.debug_command(&view.inspection_target(), D::Registers { core: 0 });
                assert_eq!(
                    view.inspection_panel,
                    Panel::Cpu,
                    "read-only action rejected"
                );
                view.control = true;
                view.debug_command(&view.inspection_target(), D::Registers { core: 0 });
                assert_eq!(
                    view.inspection_panel,
                    Panel::Result,
                    "current target can act"
                );
                view.choose_inspection(Panel::Cpu, 0);
                view.preferences.ui.inspector_pinned = true;
                cx.notify();
            })
        });
        draw(cx);
        let docked = cx.debug_bounds("shell-workspace").unwrap();
        assert_eq!(docked.size.width, closed.size.width - px(420.));
        cx.simulate_resize(size(px(750.), px(900.)));
        draw(cx);
        cx.update(|_, cx| {
            assert!(view.read(cx).preferences.ui.inspector_pinned);
            assert_eq!(
                view.read(cx).shared_plan.get().unwrap().presentation,
                shared::Presentation::Overlay
            );
        });
        cx.simulate_resize(size(px(1400.), px(900.)));
        draw(cx);
        assert_eq!(cx.debug_bounds("shell-workspace").unwrap(), docked);
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.preferences.ui.inspector_pinned = false;
                cx.notify();
            })
        });
        draw(cx);
        assert_eq!(cx.debug_bounds("shell-workspace").unwrap(), closed);
        let hit = point(closed.left() + px(10.), closed.center().y);
        cx.simulate_click(hit, Modifiers::default());
        draw(cx);
        cx.update(|_, cx| {
            let view = view.read(cx);
            assert!(!view.preferences.values.inspector_open);
            assert!(view.inspect_tile.is_none());
            assert_eq!(view.selected, "a");
            assert!(matches!(view.workspace.layout, Layout::Split { .. }));
            assert!(view.keys.is_empty());
        });
        let before = cx.update(|_, cx| {
            let view = view.read(cx);
            (
                view.selected.clone(),
                view.inspected.clone(),
                view.workspace.clone(),
                view.inspection_panel,
                view.shared_sources.read(cx).state().rows().to_vec(),
                view.preferences.values.clone(),
            )
        });
        for id in silvestris::ThemeId::ALL {
            cx.update(|_, cx| view.update(cx, |view, cx| view.set_ui_theme(id, cx)));
            draw(cx);
            let bounds = cx.debug_bounds("shell-workspace").unwrap();
            let metrics = silvestris::core::theme(id).metrics;
            assert_eq!(
                (bounds.left(), bounds.size.width),
                (closed.left(), closed.size.width)
            );
            assert_eq!(
                bounds.top(),
                px(metrics.titlebar_height + metrics.toolbar_height)
            );
            let height = cx.update(|window, _| window.viewport_size().height);
            assert_eq!(bounds.bottom(), height - px(metrics.bottom_bar_height));
            cx.update(|_, cx| {
                let view = view.read(cx);
                assert_eq!(
                    (
                        view.selected.clone(),
                        view.inspected.clone(),
                        view.workspace.clone(),
                        view.inspection_panel,
                        view.shared_sources.read(cx).state().rows().to_vec(),
                        view.preferences.values.clone()
                    ),
                    before
                );
            });
        }
    }
}
