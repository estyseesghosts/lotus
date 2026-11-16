//! The LOTUS window.
//!
//! Layout: a collapsible session sidebar (the primary navigation: primaries at
//! the top level, their secondaries nested below), a toolbar for the selected
//! session, its screens, a gauge strip, and an optional register inspector.
//! The window only renders snapshots and sends commands; see
//! [`crate::desktop_controller`].
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, uniform_list, AnyElement, AppContext, ClickEvent, Context, Div, FocusHandle,
    FontWeight, InteractiveElement, IntoElement, KeyDownEvent, KeyUpEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, ParentElement, PathPromptOptions, Render, RenderImage, Rgba,
    SharedString, Stateful, StatefulInteractiveElement, Styled, StyledImage, Window,
};
use image::{Frame, RgbaImage};
use lotus_session::native_session::InputState;
use lotus_session::supervisor::{HostConfig, SessionKind, SessionSummary};
use lotus_session::SessionState;
use serde_json::Value;

use crate::desktop_controller::{Command, DesktopController, McpLink, McpMode};
use crate::gauges;
use crate::inspection::{Inspection, Panel};
use crate::screen_geometry::TouchTransform;
use crate::session_tree::{self, TreeNode};
use crate::theme;
use lotus_protocol::debug::DebugCommand as D;

const SIDEBAR_WIDTH: f32 = 264.0;
const RAIL_WIDTH: f32 = 52.0;
const INSPECTOR_WIDTH: f32 = 420.0;
const TOOLBAR_HEIGHT: f32 = 52.0;
/// Vertical space reserved for the gauge strip when fitting screens.
const GAUGES_HEIGHT: f32 = 100.0;
const SCREEN_GAP: f32 = 12.0;
const CONTENT_PADDING: f32 = 24.0;

/// Keyboard reference shown in the inspector; mirrors `key_down` and `button`.
const KEYS: [(&str, &str); 11] = [
    ("Z / X", "A / B"),
    ("A / S", "X / Y"),
    ("Q / W", "L / R"),
    ("Arrows", "D-pad"),
    ("Enter", "Start"),
    ("Backspace", "Select"),
    ("Space", "Run / pause"),
    (".", "Step one frame"),
    ("Ctrl+O", "Open file"),
    ("Ctrl+B", "Sidebar"),
    ("Ctrl+I", "Inspector"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sidebar {
    Expanded,
    Collapsed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Inspector {
    Open,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Menu {
    Closed,
    NewSession,
    Load,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Zoom {
    Fit,
    Native,
    Double,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScreenLayout {
    Stacked,
    SideBySide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Depth {
    Top,
    Nested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    Primary,
    Plain,
    Danger,
}

pub struct LotusWindow {
    controller: Option<DesktopController>,
    startup_error: String,
    sessions: Vec<SessionSummary>,
    cards: Vec<String>,
    assets: Vec<String>,
    max_sessions: usize,
    control: bool,
    selected: String,
    /// Displayed screens and the session they belong to.
    images: Vec<Arc<RenderImage>>,
    images_session: String,
    registers: Option<(String, Value)>,
    inspection: Option<(String, Panel, Arc<Inspection>)>,
    inspection_panel: Panel,
    surface_image: Option<Arc<RenderImage>>,
    error: String,
    dismissed_error: String,
    mcp: McpLink,
    pending: usize,
    focus: FocusHandle,
    input: InputState,
    bottom_rect: Arc<Mutex<Option<TouchTransform>>>,
    sidebar: Sidebar,
    inspector: Inspector,
    menu: Menu,
    zoom: Zoom,
    layout: ScreenLayout,
}

impl LotusWindow {
    pub fn new(
        config: Option<HostConfig>,
        mcp: McpMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        cx.on_blur(&focus, window, |view, _, _| view.release_input())
            .detach();
        cx.observe_window_activation(window, |view, window, _| {
            if !window.is_window_active() {
                view.release_input();
            }
        })
        .detach();
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(33))
                .await;
            if this
                .update(cx, |view, cx| {
                    view.poll(cx);
                    cx.notify();
                })
                .is_err()
            {
                break;
            }
        })
        .detach();
        let (controller, startup_error) = match config
            .map(|config| DesktopController::start(config, mcp))
            .transpose()
        {
            Ok(controller) => (controller, String::new()),
            Err(error) => (None, error.to_string()),
        };
        Self {
            controller,
            startup_error,
            sessions: Vec::new(),
            cards: Vec::new(),
            assets: Vec::new(),
            max_sessions: 0,
            control: false,
            selected: String::new(),
            images: Vec::new(),
            images_session: String::new(),
            registers: None,
            inspection: None,
            inspection_panel: Panel::Cpu,
            surface_image: None,
            error: String::new(),
            dismissed_error: String::new(),
            mcp: McpLink::Off,
            pending: 0,
            focus,
            input: InputState::default(),
            bottom_rect: Arc::default(),
            sidebar: Sidebar::Expanded,
            inspector: Inspector::Closed,
            menu: Menu::Closed,
            zoom: Zoom::Fit,
            layout: ScreenLayout::Stacked,
        }
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        let Some(controller) = &self.controller else {
            return;
        };
        let (preview, selected, surface) = {
            let Ok(mut update) = controller.latest.lock() else {
                return;
            };
            self.sessions = update.sessions.clone();
            self.cards = update.cards.clone();
            self.assets = update.assets.clone();
            self.max_sessions = update.max_sessions;
            self.control = update.control;
            self.mcp = update.mcp;
            self.pending = update.pending;
            self.registers = update.registers.clone();
            self.inspection = update.inspection.clone();
            if update.error != self.error {
                self.error = update.error.clone();
            }
            (
                update.preview.take(),
                update.selected.clone(),
                update.debug_image.take(),
            )
        };
        if selected != self.selected {
            self.release_input();
            self.selected = selected;
            if let Some(old) = self.surface_image.take() {
                cx.drop_image(old, None);
            }
        }
        if let Some(surface) = surface.filter(|surface| surface.session == self.selected) {
            if let Some(old) = self.surface_image.take() {
                cx.drop_image(old, None);
            }
            if let Some(pixels) = RgbaImage::from_raw(surface.width, surface.height, surface.bgra) {
                self.surface_image = Some(Arc::new(RenderImage::new(vec![Frame::new(pixels)])));
            }
        }
        if self.images_session != self.selected {
            self.replace_images(Vec::new(), cx);
            self.images_session = self.selected.clone();
        }
        if let Some(preview) = preview.filter(|preview| preview.session == self.selected) {
            let images = preview
                .frames
                .into_iter()
                .filter_map(|frame| {
                    RgbaImage::from_raw(frame.lease.width, frame.lease.height, frame.bgra)
                })
                .map(|image| Arc::new(RenderImage::new(vec![Frame::new(image)])))
                .collect();
            self.replace_images(images, cx);
        }
        if self
            .selected_summary()
            .is_some_and(|summary| summary.telemetry.state == SessionState::Crashed)
        {
            self.replace_images(Vec::new(), cx);
        }
    }

    /// Replaced frames leave the sprite atlas immediately so streaming stays bounded.
    fn replace_images(&mut self, images: Vec<Arc<RenderImage>>, cx: &mut Context<Self>) {
        for old in std::mem::replace(&mut self.images, images) {
            cx.drop_image(old, None);
        }
    }

    fn send(&mut self, command: Command) {
        let Some(controller) = &self.controller else {
            return;
        };
        if let Err(error) = controller.commands.try_send(command) {
            self.error = format!("Controls busy: {error}");
        }
    }

    fn selected_summary(&self) -> Option<&SessionSummary> {
        self.sessions
            .iter()
            .find(|summary| summary.info.id == self.selected)
    }

    fn select(&mut self, id: String) {
        self.release_input();
        self.menu = Menu::Closed;
        self.selected = id.clone();
        self.send(Command::Select(id));
    }

    fn create(&mut self, kind: SessionKind, asset: Option<String>) {
        self.menu = Menu::Closed;
        self.send(Command::Create { kind, asset });
    }

    fn toggle_run(&mut self) {
        let Some(summary) = self.selected_summary() else {
            return;
        };
        let id = summary.info.id.clone();
        if summary.telemetry.playing {
            self.send(Command::Pause(id));
        } else if summary.info.asset.is_some() {
            self.send(Command::Play(id));
        }
    }

    fn step(&mut self) {
        if let Some(summary) = self.selected_summary() {
            if summary.info.asset.is_some() {
                let id = summary.info.id.clone();
                self.send(Command::Step(id));
            }
        }
    }

    fn set_inspector(&mut self, inspector: Inspector) {
        self.inspector = inspector;
        if let Some(controller) = &self.controller {
            controller
                .inspector
                .store(inspector == Inspector::Open, Ordering::Release);
        }
    }

    fn open_file(&mut self, session: Option<String>, cx: &mut Context<Self>) {
        self.menu = Menu::Closed;
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open homebrew (.3dsx)".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            if let Some(path) = paths.into_iter().next() {
                let _ = this.update(cx, |view, _| view.send(Command::OpenFile { session, path }));
            }
        })
        .detach();
    }

    fn publish_input(&mut self) {
        let Some(controller) = &self.controller else {
            return;
        };
        if let Ok(mut inputs) = controller.input.lock() {
            if self.sessions.iter().any(|s| s.info.id == self.selected) {
                inputs.insert(self.selected.clone(), self.input);
            }
        }
    }

    fn release_input(&mut self) {
        self.input = InputState::default();
        self.publish_input();
    }

    fn key_down(&mut self, event: &KeyDownEvent) {
        let modifiers = &event.keystroke.modifiers;
        if modifiers.control {
            match event.keystroke.key.as_str() {
                "b" => {
                    self.sidebar = match self.sidebar {
                        Sidebar::Expanded => Sidebar::Collapsed,
                        Sidebar::Collapsed => Sidebar::Expanded,
                    };
                }
                "i" => self.set_inspector(match self.inspector {
                    Inspector::Open => Inspector::Closed,
                    Inspector::Closed => Inspector::Open,
                }),
                // Ctrl+O needs a context and is handled in the root listener.
                _ => {}
            }
            return;
        }
        match event.keystroke.key.as_str() {
            "space" if !event.is_held => self.toggle_run(),
            "." if !event.is_held => self.step(),
            "escape" => self.menu = Menu::Closed,
            key => self.button(key, true),
        }
    }

    fn button(&mut self, key: &str, down: bool) {
        let mask = match key {
            "z" => 1,
            "x" => 2,
            "a" => 4,
            "s" => 8,
            "up" => 16,
            "down" => 32,
            "left" => 64,
            "right" => 128,
            "q" => 256,
            "w" => 512,
            "enter" => 1024,
            "backspace" => 2048,
            _ => return,
        };
        if down {
            self.input.buttons |= mask;
        } else {
            self.input.buttons &= !mask;
        }
        self.publish_input();
    }

    fn touch(&mut self, x: f32, y: f32) {
        let coordinate = self
            .bottom_rect
            .lock()
            .ok()
            .and_then(|rect| rect.as_ref().and_then(|rect| rect.map_logical(x, y)));
        if let Some((x, y)) = coordinate {
            self.input.x = x;
            self.input.y = y;
            self.input.touch = true;
        } else {
            self.input.touch = false;
        }
        self.publish_input();
    }

    fn release_touch(&mut self) {
        self.input.touch = false;
        self.publish_input();
    }

    // ---- sidebar -------------------------------------------------------------

    fn render_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.sidebar == Sidebar::Collapsed {
            return self.render_rail(cx);
        }
        let tree = session_tree::session_tree(&self.sessions);
        let mut list = div()
            .id("session-tree")
            .flex()
            .flex_col()
            .flex_1()
            .gap(px(2.0))
            .py_1()
            .overflow_y_scroll();
        if tree.is_empty() {
            list = list.child(
                div()
                    .px_4()
                    .py_2()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child("No sessions running."),
            );
        }
        for node in &tree {
            match node {
                TreeNode::Card {
                    card,
                    primary,
                    secondaries,
                } => {
                    list = list.child(match primary {
                        Some(primary) => {
                            self.session_row(primary, Depth::Top, cx).into_any_element()
                        }
                        None => self.offline_card_row(card, cx).into_any_element(),
                    });
                    for secondary in secondaries {
                        list = list.child(self.session_row(secondary, Depth::Nested, cx));
                    }
                }
                TreeNode::Detached(secondary) => {
                    list = list.child(self.session_row(secondary, Depth::Top, cx));
                }
            }
        }
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .bg(theme::panel())
            .border_r_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(TOOLBAR_HEIGHT))
                    .px_4()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().size(px(10.0)).rounded(px(2.0)).bg(theme::text()))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("LOTUS"),
                            ),
                    )
                    .child(
                        icon_button("collapse", "‹", "Collapse sidebar (Ctrl+B)").on_click(
                            cx.listener(|view, _: &ClickEvent, _, _| {
                                view.sidebar = Sidebar::Collapsed;
                            }),
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .pl_4()
                    .pr_2()
                    .pt_3()
                    .pb_1()
                    .child(section_label("SESSIONS"))
                    .when(self.control, |row| {
                        row.child(icon_button("new-session", "+", "New session").on_click(
                            cx.listener(|view, _: &ClickEvent, _, _| {
                                view.menu = match view.menu {
                                    Menu::NewSession => Menu::Closed,
                                    _ => Menu::NewSession,
                                };
                            }),
                        ))
                    }),
            )
            .child(list)
            .child(self.render_sidebar_footer())
            .when(self.menu == Menu::NewSession, |sidebar| {
                sidebar.child(self.render_new_menu(cx))
            })
            .into_any_element()
    }

    fn render_rail(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut rail = div()
            .flex()
            .flex_col()
            .items_center()
            .flex_none()
            .w(px(RAIL_WIDTH))
            .h_full()
            .bg(theme::panel())
            .border_r_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(TOOLBAR_HEIGHT))
                    .w_full()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(
                        icon_button("expand", "›", "Expand sidebar (Ctrl+B)").on_click(
                            cx.listener(|view, _: &ClickEvent, _, _| {
                                view.sidebar = Sidebar::Expanded;
                            }),
                        ),
                    ),
            );
        let mut chips = div()
            .id("session-rail")
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .py_2()
            .overflow_y_scroll();
        for node in session_tree::session_tree(&self.sessions) {
            let rows: Vec<(SessionSummary, Depth)> = match node {
                TreeNode::Card {
                    primary,
                    secondaries,
                    ..
                } => primary
                    .map(|primary| (primary, Depth::Top))
                    .into_iter()
                    .chain(secondaries.into_iter().map(|s| (s, Depth::Nested)))
                    .collect(),
                TreeNode::Detached(summary) => vec![(summary, Depth::Top)],
            };
            for (summary, depth) in rows {
                let id = summary.info.id.clone();
                let selected = id == self.selected;
                let label = session_tree::session_label(&summary.info);
                let initial = label.chars().next().unwrap_or('?').to_ascii_uppercase();
                let size = if depth == Depth::Top { 32.0 } else { 24.0 };
                let tip = format!("{label} · {}", state_name(&summary));
                chips = chips.child(with_tooltip(
                    div()
                        .id(SharedString::from(format!("chip-{id}")))
                        .relative()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(size))
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(if selected {
                            theme::border_strong()
                        } else {
                            theme::border()
                        })
                        .when(selected, |chip| chip.bg(theme::surface_active()))
                        .when(depth == Depth::Nested, |chip| chip.border_dashed())
                        .hover(|style| style.bg(theme::surface_hover()))
                        .cursor_pointer()
                        .text_xs()
                        .text_color(theme::text_secondary())
                        .child(initial.to_string())
                        .child(
                            div()
                                .absolute()
                                .top(px(-2.0))
                                .right(px(-2.0))
                                .size(px(7.0))
                                .rounded_full()
                                .bg(state_colour(&summary)),
                        )
                        .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                            view.select(id.clone());
                        })),
                    tip,
                ));
            }
        }
        rail = rail.child(chips);
        rail.into_any_element()
    }

    fn session_row(
        &self,
        summary: &SessionSummary,
        depth: Depth,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = summary.info.id.clone();
        let selected = id == self.selected;
        let group = SharedString::from(format!("row-{id}"));
        let is_primary = matches!(summary.info.kind, SessionKind::Primary { .. });
        let fork = match (&summary.info.kind, &summary.info.asset) {
            (SessionKind::Primary { card }, asset) => Some((card.clone(), asset.clone())),
            _ => None,
        };
        let label = session_tree::session_label(&summary.info);
        let mut name = div()
            .flex()
            .items_center()
            .gap_1()
            .flex_1()
            .min_w_0()
            .child(
                div()
                    .truncate()
                    .text_sm()
                    .text_color(if selected {
                        theme::text()
                    } else {
                        theme::text_secondary()
                    })
                    .child(label),
            );
        if !is_primary {
            name = name.child(
                div()
                    .flex_none()
                    .font_family(theme::MONO)
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(session_tree::short_id(&id)),
            );
        }
        let stop_id = id.clone();
        let mut actions = div()
            .flex()
            .flex_none()
            .gap(px(2.0))
            .invisible()
            .group_hover(group.clone(), |style| style.visible());
        if let Some((card, asset)) = fork.filter(|_| self.control) {
            actions = actions.child(
                icon_button(
                    SharedString::from(format!("fork-{id}")),
                    "+",
                    "Secondary from this card",
                )
                .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    view.create(
                        SessionKind::Secondary {
                            parent: Some(card.clone()),
                        },
                        asset.clone(),
                    );
                })),
            );
        }
        if self.control {
            actions = actions.child(
                icon_button(
                    SharedString::from(format!("stop-{id}")),
                    "×",
                    if is_primary {
                        "Stop (card data is kept)"
                    } else {
                        "Stop and discard"
                    },
                )
                .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    view.send(Command::Stop(stop_id.clone()));
                })),
            );
        }
        div()
            .id(SharedString::from(format!("session-{id}")))
            .group(group.clone())
            .relative()
            .flex()
            .items_center()
            .gap_2()
            .h(px(30.0))
            .mx_2()
            .pl(px(if depth == Depth::Nested { 30.0 } else { 10.0 }))
            .pr_1()
            .rounded(px(6.0))
            .cursor_pointer()
            .when(selected, |row| row.bg(theme::surface_active()))
            .when(!selected, |row| {
                row.hover(|style| style.bg(theme::surface_hover()))
            })
            .when(depth == Depth::Nested, |row| {
                row.child(
                    div()
                        .absolute()
                        .left(px(16.0))
                        .top_0()
                        .bottom_0()
                        .w(px(1.0))
                        .bg(theme::border_strong()),
                )
            })
            .child(state_dot(summary, is_primary))
            .child(name)
            .child(
                div()
                    .flex_none()
                    .font_family(theme::MONO)
                    .text_xs()
                    .text_color(theme::text_muted())
                    .group_hover(group, |style| style.invisible())
                    .child(row_meta(summary)),
            )
            .child(div().absolute().right(px(4.0)).child(actions))
            .on_click(cx.listener(move |view, _: &ClickEvent, _, _| view.select(id.clone())))
    }

    fn offline_card_row(&self, card: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let mounted = card.to_string();
        div()
            .id(SharedString::from(format!("offline-{card}")))
            .group("offline")
            .flex()
            .items_center()
            .gap_2()
            .h(px(30.0))
            .mx_2()
            .pl(px(10.0))
            .pr_1()
            .rounded(px(6.0))
            .child(
                div()
                    .size(px(8.0))
                    .rounded(px(2.0))
                    .border_1()
                    .border_color(theme::border_strong()),
            )
            .child(
                div()
                    .flex_1()
                    .truncate()
                    .text_sm()
                    .text_color(theme::text_muted())
                    .child(card.to_string()),
            )
            .when(self.control, |row| {
                row.child(
                    icon_button(
                        SharedString::from(format!("mount-{card}")),
                        "▸",
                        "Start primary session",
                    )
                    .on_click(cx.listener(
                        move |view, _: &ClickEvent, _, _| {
                            view.create(
                                SessionKind::Primary {
                                    card: mounted.clone(),
                                },
                                None,
                            );
                        },
                    )),
                )
            })
    }

    fn render_new_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mounted: Vec<&str> = self
            .sessions
            .iter()
            .filter_map(|summary| match &summary.info.kind {
                SessionKind::Primary { card } => Some(card.as_str()),
                SessionKind::Secondary { .. } => None,
            })
            .collect();
        let mut menu = popover("new-session-menu")
            .top(px(TOOLBAR_HEIGHT + 34.0))
            .left(px(8.0))
            .right(px(8.0))
            .on_mouse_down_out(cx.listener(|view, _, _, _| view.menu = Menu::Closed))
            .child(menu_heading("PRIMARY · PERSISTENT SD"));
        for card in self.cards.iter().filter(|p| !mounted.contains(&p.as_str())) {
            let name = card.clone();
            menu = menu.child(
                menu_item(
                    SharedString::from(format!("open-{card}")),
                    card.clone(),
                    "open",
                )
                .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                    view.create(SessionKind::Primary { card: name.clone() }, None);
                })),
            );
        }
        let fresh = session_tree::next_card_name(&self.cards);
        menu = menu
            .child(
                menu_item("new-card", "New card", fresh.clone()).on_click(cx.listener(
                    move |view, _: &ClickEvent, _, _| {
                        view.create(
                            SessionKind::Primary {
                                card: fresh.clone(),
                            },
                            None,
                        );
                    },
                )),
            )
            .child(div().h(px(1.0)).my_1().bg(theme::border()))
            .child(menu_heading("SECONDARY · DISCARDED ON STOP"))
            .child(
                menu_item("new-empty", "Empty", "blank SD").on_click(cx.listener(
                    |view, _: &ClickEvent, _, _| {
                        view.create(SessionKind::Secondary { parent: None }, None);
                    },
                )),
            );
        for card in &self.cards {
            let parent = card.clone();
            menu = menu.child(
                menu_item(
                    SharedString::from(format!("copy-{card}")),
                    format!("Copy of {card}"),
                    "snapshot",
                )
                .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                    view.create(
                        SessionKind::Secondary {
                            parent: Some(parent.clone()),
                        },
                        None,
                    );
                })),
            );
        }
        menu
    }

    fn render_sidebar_footer(&self) -> impl IntoElement {
        let (colour, text) = match self.mcp {
            McpLink::Attached => (theme::success(), "MCP attached"),
            McpLink::Closed => (theme::warning(), "MCP disconnected"),
            McpLink::Off => (theme::border_strong(), "MCP off"),
        };
        let slots = format!("{} / {}", self.sessions.len(), self.max_sessions);
        div()
            .flex()
            .flex_col()
            .gap_1()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(theme::border())
            .text_xs()
            .text_color(theme::text_muted())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().size(px(6.0)).rounded_full().bg(colour))
                            .child(text),
                    )
                    .child(div().font_family(theme::MONO).child(slots)),
            )
            .when(!self.control, |footer| footer.child("Read-only host"))
    }

    // ---- main ----------------------------------------------------------------

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut bar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .h(px(TOOLBAR_HEIGHT))
            .px_4()
            .border_b_1()
            .border_color(theme::border());
        let Some(summary) = self.selected_summary() else {
            return bar.child(
                div()
                    .text_sm()
                    .text_color(theme::text_muted())
                    .child("No session selected"),
            );
        };
        let id = summary.info.id.clone();
        let telemetry = &summary.telemetry;
        let loaded = summary.info.asset.is_some();
        let active = telemetry.state.is_active();
        let (kind_label, kind_detail) = match &summary.info.kind {
            SessionKind::Primary { card } => ("PRIMARY", format!("card {card}")),
            SessionKind::Secondary {
                parent: Some(parent),
            } => ("SECONDARY", format!("copy of {parent}")),
            SessionKind::Secondary { parent: None } => ("SECONDARY", "empty".to_string()),
        };
        bar = bar
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(session_tree::session_label(&summary.info)),
            )
            .child(badge(
                kind_label,
                if kind_label == "PRIMARY" {
                    theme::info()
                } else {
                    theme::warning()
                },
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .truncate()
                    .child(match (&summary.info.kind, summary.info.asset.as_deref()) {
                        // A secondary is already named after its content.
                        (SessionKind::Secondary { .. }, Some(_)) => format!(
                            "{kind_detail} · {} · {}",
                            session_tree::short_id(&id),
                            state_name(summary)
                        ),
                        (_, asset) => format!(
                            "{kind_detail} · {} · {}",
                            state_name(summary),
                            asset.unwrap_or("no content")
                        ),
                    }),
            )
            .child(div().flex_1());
        if self.pending > 0 {
            bar = bar.child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child("working…"),
            );
        }
        if self.control && active {
            let run_id = id.clone();
            let playing = telemetry.playing;
            bar = bar
                .child(
                    button(
                        "run",
                        if playing { "Pause" } else { "Run" },
                        Tone::Primary,
                        loaded,
                    )
                    .on_click(cx.listener(
                        move |view, _: &ClickEvent, _, _| {
                            view.send(if playing {
                                Command::Pause(run_id.clone())
                            } else {
                                Command::Play(run_id.clone())
                            });
                        },
                    )),
                )
                .child(
                    button("step", "Step", Tone::Plain, loaded && !playing)
                        .on_click(cx.listener(|view, _: &ClickEvent, _, _| view.step())),
                )
                .child({
                    let reset_id = id.clone();
                    button("reset", "Reset", Tone::Plain, loaded).on_click(cx.listener(
                        move |view, _: &ClickEvent, _, _| {
                            view.send(Command::Reset(reset_id.clone()))
                        },
                    ))
                })
                .child(
                    button("load", "Load", Tone::Plain, true).on_click(cx.listener(
                        |view, _: &ClickEvent, _, _| {
                            view.menu = match view.menu {
                                Menu::Load => Menu::Closed,
                                _ => Menu::Load,
                            };
                        },
                    )),
                );
        }
        if self.control {
            let stop_id = id.clone();
            bar = bar.child(
                button("stop", "Stop", Tone::Danger, true).on_click(cx.listener(
                    move |view, _: &ClickEvent, _, _| view.send(Command::Stop(stop_id.clone())),
                )),
            );
        }
        bar.child(div().w(px(1.0)).h(px(20.0)).mx_1().bg(theme::border()))
            .child(
                button(
                    "zoom",
                    match self.zoom {
                        Zoom::Fit => "Fit",
                        Zoom::Native => "1×",
                        Zoom::Double => "2×",
                    },
                    Tone::Plain,
                    true,
                )
                .on_click(cx.listener(|view, _: &ClickEvent, _, _| {
                    view.release_input();
                    view.zoom = match view.zoom {
                        Zoom::Fit => Zoom::Native,
                        Zoom::Native => Zoom::Double,
                        Zoom::Double => Zoom::Fit,
                    };
                })),
            )
            .child(
                button(
                    "layout",
                    match self.layout {
                        ScreenLayout::Stacked => "Stacked",
                        ScreenLayout::SideBySide => "Side by side",
                    },
                    Tone::Plain,
                    true,
                )
                .on_click(cx.listener(|view, _: &ClickEvent, _, _| {
                    view.release_input();
                    view.layout = match view.layout {
                        ScreenLayout::Stacked => ScreenLayout::SideBySide,
                        ScreenLayout::SideBySide => ScreenLayout::Stacked,
                    };
                })),
            )
            .child(
                button(
                    "inspector",
                    "Inspect",
                    if self.inspector == Inspector::Open {
                        Tone::Primary
                    } else {
                        Tone::Plain
                    },
                    true,
                )
                .on_click(cx.listener(|view, _: &ClickEvent, _, _| {
                    view.set_inspector(match view.inspector {
                        Inspector::Open => Inspector::Closed,
                        Inspector::Closed => Inspector::Open,
                    });
                })),
            )
    }

    fn render_load_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let session = self.selected.clone();
        popover("load-menu")
            .top(px(TOOLBAR_HEIGHT - 4.0))
            .right(px(16.0))
            .w(px(280.0))
            .on_mouse_down_out(cx.listener(|view, _, _, _| view.menu = Menu::Closed))
            .child(menu_heading("APPROVED CONTENT"))
            .child(self.asset_list(Some(session.clone()), cx))
            .child(div().h(px(1.0)).my_1().bg(theme::border()))
            .child(
                menu_item("open-file", "Open file…", "Ctrl+O").on_click(cx.listener(
                    move |view, _: &ClickEvent, _, cx| view.open_file(Some(session.clone()), cx),
                )),
            )
    }

    fn asset_list(&self, session: Option<String>, cx: &mut Context<Self>) -> Div {
        let mut list = div().flex().flex_col();
        if self.assets.is_empty() {
            list = list.child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child("None configured for this host."),
            );
        }
        for asset in &self.assets {
            let handle = asset.clone();
            let session = session.clone();
            list = list.child(
                menu_item(
                    SharedString::from(format!("asset-{asset}")),
                    session_tree::asset_label(asset),
                    asset.clone(),
                )
                .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                    view.menu = Menu::Closed;
                    match &session {
                        Some(session) => view.send(Command::Load {
                            session: session.clone(),
                            asset: handle.clone(),
                        }),
                        None => view.create(
                            SessionKind::Secondary { parent: None },
                            Some(handle.clone()),
                        ),
                    }
                })),
            );
        }
        list
    }

    fn render_stage(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        if self.controller.is_none() {
            return empty_state(
                "No host configuration",
                if self.startup_error.is_empty() {
                    "Start LOTUS with a host configuration: lotus-desktop [--mcp] host.json"
                        .to_string()
                } else {
                    format!("Host configuration failed: {}", self.startup_error)
                },
            )
            .into_any_element();
        }
        let Some(summary) = self.selected_summary() else {
            let mut state = empty_state(
                "No sessions",
                "A primary session keeps a persistent SD card. A secondary is a disposable copy, discarded when it stops.".to_string(),
            );
            if self.control {
                let fresh = session_tree::next_card_name(&self.cards);
                let primary = self.cards.first().cloned().unwrap_or(fresh);
                state =
                    state.child(
                        div()
                            .flex()
                            .gap_2()
                            .mt_2()
                            .child(
                                button("empty-primary", "New primary", Tone::Primary, true)
                                    .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                                        view.create(
                                            SessionKind::Primary {
                                                card: primary.clone(),
                                            },
                                            None,
                                        );
                                    })),
                            )
                            .child(
                                button("empty-secondary", "New secondary", Tone::Plain, true)
                                    .on_click(cx.listener(|view, _: &ClickEvent, _, _| {
                                        view.create(SessionKind::Secondary { parent: None }, None);
                                    })),
                            ),
                    );
            }
            return state.into_any_element();
        };
        if summary.telemetry.state == SessionState::Crashed {
            return empty_state(
                "Worker crashed",
                format!(
                    "{} Logs are kept in the session root. Stop the session to release its slot.",
                    summary.telemetry.last_error
                ),
            )
            .into_any_element();
        }
        if summary.info.asset.is_none() {
            let session = summary.info.id.clone();
            let mut state = empty_state(
                "Nothing loaded",
                "Choose approved content, or open a .3dsx file.".to_string(),
            );
            if self.control {
                state = state.child(
                    div()
                        .w(px(320.0))
                        .mt_2()
                        .p_1()
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(theme::border())
                        .child(self.asset_list(Some(session.clone()), cx))
                        .child(div().h(px(1.0)).my_1().bg(theme::border()))
                        .child(
                            menu_item("idle-open-file", "Open file…", "Ctrl+O").on_click(
                                cx.listener(move |view, _: &ClickEvent, _, cx| {
                                    view.open_file(Some(session.clone()), cx)
                                }),
                            ),
                        ),
                );
            }
            return state.into_any_element();
        }
        self.render_screens(window, cx).into_any_element()
    }

    fn render_screens(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let scale = self.screen_scale(window);
        let mut top = screen_frame(400.0 * scale, 240.0 * scale).id("top-screen");
        if let Some(image) = self.images.first() {
            top = top.child(gpui::img(image.clone()).size_full());
        }
        let rect = self.bottom_rect.clone();
        let mut bottom = screen_frame(320.0 * scale, 240.0 * scale)
            .id("touch-screen")
            .cursor_crosshair()
            .child(
                gpui::canvas(
                    move |bounds, _, _| {
                        if let Ok(mut rect) = rect.lock() {
                            *rect = Some(TouchTransform {
                                left: bounds.origin.x.into(),
                                top: bounds.origin.y.into(),
                                width: bounds.size.width.into(),
                                height: bounds.size.height.into(),
                            });
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, event: &MouseDownEvent, _, _| {
                    view.touch(event.position.x.into(), event.position.y.into())
                }),
            )
            .on_mouse_move(cx.listener(|view, event: &MouseMoveEvent, _, _| {
                if view.input.touch && event.dragging() {
                    view.touch(event.position.x.into(), event.position.y.into());
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|view, _, _, _| view.release_touch()),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|view, _, _, _| view.release_touch()),
            );
        if let Some(image) = self.images.get(1) {
            bottom = bottom.child(gpui::img(image.clone()).size_full());
        }
        let screens = div()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(SCREEN_GAP))
            .when(self.layout == ScreenLayout::Stacked, |screens| {
                screens.flex_col()
            })
            .child(top)
            .child(bottom);
        div()
            .id("stage")
            .flex()
            .flex_1()
            .items_center()
            .justify_center()
            .overflow_scroll()
            .child(screens)
    }

    /// Fit picks the largest scale that shows both screens without scrolling.
    fn screen_scale(&self, window: &Window) -> f32 {
        match self.zoom {
            Zoom::Native => 1.0,
            Zoom::Double => 2.0,
            Zoom::Fit => {
                let viewport = window.viewport_size();
                let sidebar = match self.sidebar {
                    Sidebar::Expanded => SIDEBAR_WIDTH,
                    Sidebar::Collapsed => RAIL_WIDTH,
                };
                let inspector = match self.inspector {
                    Inspector::Open => INSPECTOR_WIDTH,
                    Inspector::Closed => 0.0,
                };
                let width = f32::from(viewport.width) - sidebar - inspector - 2.0 * CONTENT_PADDING;
                let height = f32::from(viewport.height)
                    - TOOLBAR_HEIGHT
                    - GAUGES_HEIGHT
                    - 2.0 * CONTENT_PADDING;
                fit_scale(self.layout, width, height)
            }
        }
    }

    fn choose_inspection(&mut self, panel: Panel, offset: u32) {
        self.inspection_panel = panel;
        if let Some(controller) = &self.controller {
            *controller.inspection_selection.lock().unwrap() = (panel, offset);
        }
    }
    fn debug_command(&mut self, command: D) {
        if self.selected.is_empty() {
            return;
        }
        self.choose_inspection(Panel::Result, 0);
        self.send(Command::Debug {
            session: self.selected.clone(),
            command,
        });
        self.inspection_panel = Panel::Result;
    }
    fn open_trace(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open event trace (JSONL, up to 64 MiB)".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                if let Some(path) = paths.into_iter().next() {
                    let _ = this.update(cx, |view, _| {
                        view.choose_inspection(Panel::Result, 0);
                        view.send(Command::OpenTrace { path });
                    });
                }
            }
        })
        .detach();
    }
    fn render_inspector(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut panel = div()
            .id("inspector")
            .flex()
            .flex_col()
            .flex_none()
            .w(px(INSPECTOR_WIDTH))
            .h_full()
            .bg(theme::panel())
            .border_l_1()
            .border_color(theme::border());
        let mut tabs = div().flex().flex_wrap().gap_1().p_2();
        for tab in Panel::ALL {
            tabs = tabs.child(
                button(
                    format!("inspect-{}", tab as usize),
                    tab.label(),
                    if self.inspection_panel == tab {
                        Tone::Primary
                    } else {
                        Tone::Plain
                    },
                    true,
                )
                .on_click(cx.listener(move |view, _, _, _| view.choose_inspection(tab, 0))),
            );
        }
        panel = panel.child(tabs);
        let mut actions = div().flex().flex_wrap().gap_1().px_2().pb_2();
        let commands: Vec<(&'static str, D)> = match self.inspection_panel {
            Panel::Cpu => vec![
                ("Step", D::Step { core: 0, count: 1 }),
                ("Over", D::StepOver { core: 0 }),
                ("Out", D::StepOut { core: 0 }),
                ("Back", D::ReverseStep { core: 0 }),
            ],
            Panel::States => vec![
                (
                    "Save desktop",
                    D::SaveState {
                        name: "desktop".into(),
                    },
                ),
                (
                    "Enable rewind",
                    D::RewindConfigure {
                        enabled: true,
                        capacity: 64,
                        budget_mib: 128,
                    },
                ),
                ("Reverse stop", D::ReverseContinue),
            ],
            Panel::Gpu => vec![
                (
                    "Capture draws",
                    D::GpuConfigure {
                        enabled: true,
                        stop_events: vec![],
                    },
                ),
                (
                    "Disable capture",
                    D::GpuConfigure {
                        enabled: false,
                        stop_events: vec![],
                    },
                ),
                ("Record CiTrace", D::CitraceStart),
                ("Finish trace", D::CitraceStop),
                ("Replay trace", D::CitraceReplay),
            ],
            Panel::Ipc | Panel::Timeline => vec![
                ("Trace on", D::Trace { enabled: true }),
                ("Trace off", D::Trace { enabled: false }),
            ],
            Panel::Audio => vec![
                ("Focus audio", D::AudioFocus { enabled: true }),
                ("Mute", D::AudioFocus { enabled: false }),
                ("Record WAV", D::AudioCapture { enabled: true }),
                ("Finish WAV", D::AudioCapture { enabled: false }),
            ],
            _ => vec![],
        };
        for (label, command) in commands {
            actions = actions.child(
                button(format!("debug-{label}"), label, Tone::Plain, self.control).on_click(
                    cx.listener(move |view, _, _, _| view.debug_command(command.clone())),
                ),
            );
        }
        actions = actions.child(
            button("open-trace", "Open JSONL", Tone::Plain, true)
                .on_click(cx.listener(|view, _, _, cx| view.open_trace(cx))),
        );
        actions = actions.child(
            button("clipboard-command", "Run clipboard JSON", Tone::Plain, true).on_click(
                cx.listener(|view, _, _, cx| {
                    if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                        let command = if text.len() <= 65536 {
                            serde_json::from_str::<D>(&text).map_err(|e| e.to_string())
                        } else {
                            Err("Command exceeds 64 KiB".into())
                        };
                        match command {
                            Ok(command) => match command.validate() {
                                Ok(()) => view.debug_command(command),
                                Err(error) => view.error = error.into(),
                            },
                            Err(error) => view.error = error,
                        }
                    }
                }),
            ),
        );
        actions = actions.child(with_tooltip(
            button("debug-keys", "Keys", Tone::Plain, true),
            KEYS.iter()
                .map(|(key, action)| format!("{key}: {action}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
        panel = panel.child(actions);
        let view = self
            .inspection
            .as_ref()
            .filter(|(session, tab, _)| *session == self.selected && *tab == self.inspection_panel);
        let Some((_, _, view)) = view else {
            return panel.child(div().p_3().child("Select a session and inspector. Results are safe-point snapshots; unsupported backend operations show their reason."));
        };
        panel = panel
            .child(
                div()
                    .px_2()
                    .pb_2()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(view.source.clone()),
            )
            .child(div().px_2().text_xs().child(format!(
                "{} rows · {} lost",
                view.rows.len(),
                view.lost
            )));
        if let Some(next) = view.next {
            let tab = self.inspection_panel;
            panel = panel.child(
                button("inspect-next", "Next page", Tone::Plain, true)
                    .on_click(cx.listener(move |view, _, _, _| view.choose_inspection(tab, next))),
            );
        }
        if self.inspection_panel == Panel::Result {
            if let Some(image) = &self.surface_image {
                panel = panel.child(
                    gpui::img(image.clone())
                        .w_full()
                        .h(px(180.0))
                        .object_fit(gpui::ObjectFit::Contain),
                );
            }
        }
        let rows = view.rows.clone();
        let weak = cx.entity().downgrade();
        panel.child(
            uniform_list("inspection-rows", rows.len(), move |range, _, _| {
                range
                    .map(|index| {
                        let row = &rows[index];
                        let weak = weak.clone();
                        let action = row.action.clone();
                        let tooltip = format!("{}\n{}", row.key, row.value);
                        with_tooltip(
                            div()
                                .id(("inspection-row", index))
                                .flex()
                                .h(px(28.0))
                                .px_2()
                                .gap_2()
                                .overflow_hidden()
                                .font_family(theme::MONO)
                                .text_xs()
                                .child(
                                    div()
                                        .w(px(48.0))
                                        .flex_none()
                                        .text_color(theme::text_muted())
                                        .child(row.lane),
                                )
                                .child(
                                    div()
                                        .w(px(150.0))
                                        .flex_none()
                                        .truncate()
                                        .child(row.key.clone()),
                                )
                                .child(
                                    div().flex_1().min_w_0().truncate().child(row.value.clone()),
                                ),
                            tooltip,
                        )
                        .when_some(action, |element, command| {
                            element
                                .cursor_pointer()
                                .hover(|style| style.bg(theme::surface_hover()))
                                .on_click(move |_, _, cx| {
                                    let _ = weak
                                        .update(cx, |view, _| view.debug_command(command.clone()));
                                })
                        })
                    })
                    .collect()
            })
            .flex_1()
            .min_h_0(),
        )
    }

    fn render_error(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if self.error.is_empty() || self.error == self.dismissed_error {
            return None;
        }
        Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_3()
                .mx_4()
                .mt_3()
                .px_3()
                .py_2()
                .rounded(px(6.0))
                .border_1()
                .border_color(theme::error())
                .text_sm()
                .child(div().size(px(6.0)).rounded_full().bg(theme::error()))
                .child(div().flex_1().child(self.error.clone()))
                .child(
                    icon_button("dismiss-error", "×", "Dismiss").on_click(cx.listener(
                        |view, _: &ClickEvent, _, _| view.dismissed_error = view.error.clone(),
                    )),
                ),
        )
    }
}

impl Render for LotusWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let stage = self.render_stage(window, cx);
        let gauges = self
            .selected_summary()
            .filter(|summary| summary.info.asset.is_some())
            .map(|summary| gauges::gauge_strip(&summary.telemetry));
        let main = div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(self.render_toolbar(cx))
            .children(self.render_error(cx))
            .child(stage)
            .children(gauges)
            .when(self.menu == Menu::Load, |main| {
                main.child(self.render_load_menu(cx))
            });
        div()
            .id("lotus")
            .flex()
            .size_full()
            .bg(theme::background())
            .text_color(theme::text())
            .font_family(theme::FONT)
            .text_sm()
            .track_focus(&self.focus)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _, window, cx| window.focus(&view.focus, cx)),
            )
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                if event.keystroke.modifiers.control && event.keystroke.key == "o" {
                    let session = view
                        .selected_summary()
                        .filter(|summary| summary.telemetry.state.is_active())
                        .map(|summary| summary.info.id.clone());
                    view.open_file(session, cx);
                    return;
                }
                view.key_down(event);
            }))
            .on_key_up(cx.listener(|view, event: &KeyUpEvent, _, _| {
                view.button(&event.keystroke.key, false)
            }))
            .child(self.render_sidebar(cx))
            .child(main)
            .when(self.inspector == Inspector::Open, |root| {
                root.child(self.render_inspector(cx))
            })
    }
}

/// Largest scale at which both screens fit `width` x `height`, at least 0.5.
#[must_use]
fn fit_scale(layout: ScreenLayout, width: f32, height: f32) -> f32 {
    // `f32::min` ignores NaN, so reject non-finite space before comparing axes.
    if !width.is_finite() || !height.is_finite() {
        return 1.0;
    }
    let (needed_width, needed_height) = match layout {
        ScreenLayout::Stacked => (400.0, 480.0),
        ScreenLayout::SideBySide => (720.0, 240.0),
    };
    let gap_width = if layout == ScreenLayout::SideBySide {
        SCREEN_GAP
    } else {
        0.0
    };
    let gap_height = if layout == ScreenLayout::Stacked {
        SCREEN_GAP
    } else {
        0.0
    };
    let scale = ((width - gap_width) / needed_width).min((height - gap_height) / needed_height);
    if scale.is_finite() {
        scale.max(0.5)
    } else {
        1.0
    }
}

fn state_name(summary: &SessionSummary) -> &'static str {
    match summary.telemetry.state {
        SessionState::Running => "running",
        SessionState::Paused if summary.telemetry.playing => "running",
        SessionState::Paused => "paused",
        SessionState::Idle => "idle",
        SessionState::Created => "starting",
        SessionState::Stopped => "stopped",
        SessionState::Crashed => "crashed",
    }
}

fn state_colour(summary: &SessionSummary) -> Rgba {
    match state_name(summary) {
        "running" => theme::success(),
        "paused" => theme::warning(),
        "crashed" => theme::error(),
        _ => theme::text_muted(),
    }
}

/// Square dot for primaries, round for secondaries: shape, not only colour.
fn state_dot(summary: &SessionSummary, primary: bool) -> impl IntoElement {
    div()
        .flex_none()
        .size(px(8.0))
        .rounded(px(if primary { 2.0 } else { 4.0 }))
        .bg(state_colour(summary))
}

fn row_meta(summary: &SessionSummary) -> String {
    match state_name(summary) {
        "running" if summary.telemetry.fps > 0.0 => format!("{:.0} fps", summary.telemetry.fps),
        state => state.to_string(),
    }
}

fn section_label(text: &'static str) -> impl IntoElement {
    div()
        .text_size(px(10.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text_muted())
        .child(text)
}

fn badge(text: &'static str, colour: Rgba) -> impl IntoElement {
    div()
        .px(px(6.0))
        .py(px(1.0))
        .rounded(px(4.0))
        .border_1()
        .border_color(colour)
        .text_size(px(10.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(colour)
        .child(text)
}

fn button(
    id: impl Into<SharedString>,
    label: &'static str,
    tone: Tone,
    enabled: bool,
) -> Stateful<Div> {
    let base = div()
        .id(id.into())
        .flex()
        .items_center()
        .h(px(30.0))
        .px_3()
        .rounded(px(6.0))
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .child(label);
    let base = match tone {
        Tone::Primary => base.bg(theme::text()).text_color(theme::background()),
        Tone::Plain => base
            .border_1()
            .border_color(theme::border())
            .text_color(theme::text()),
        Tone::Danger => base
            .border_1()
            .border_color(theme::border())
            .text_color(theme::error()),
    };
    if enabled {
        base.cursor_pointer().hover(move |style| match tone {
            Tone::Primary => style.opacity(0.88),
            Tone::Plain | Tone::Danger => style
                .bg(theme::surface_hover())
                .border_color(theme::border_strong()),
        })
    } else {
        base.opacity(0.4)
    }
}

/// Small text tooltip; GPUI tooltips are views.
struct Tooltip(SharedString);

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded(px(4.0))
            .bg(theme::panel())
            .border_1()
            .border_color(theme::border_strong())
            .font_family(theme::FONT)
            .text_xs()
            .text_color(theme::text())
            .child(self.0.clone())
    }
}

fn with_tooltip(element: Stateful<Div>, text: impl Into<SharedString>) -> Stateful<Div> {
    let text = text.into();
    element.tooltip(move |_, cx| cx.new(|_| Tooltip(text.clone())).into())
}

fn icon_button(
    id: impl Into<SharedString>,
    glyph: &'static str,
    description: &'static str,
) -> Stateful<Div> {
    let button = div()
        .id(id.into())
        .flex()
        .items_center()
        .justify_center()
        .size(px(24.0))
        .rounded(px(5.0))
        .text_color(theme::text_secondary())
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_active()).text_color(theme::text()))
        .child(glyph);
    with_tooltip(button, description)
}

fn popover(id: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .absolute()
        .flex()
        .flex_col()
        .p_1()
        .rounded(px(8.0))
        .bg(theme::panel())
        .border_1()
        .border_color(theme::border_strong())
        .shadow_lg()
        .occlude()
}

fn menu_heading(text: &'static str) -> impl IntoElement {
    div().px_2().pt_2().pb_1().child(section_label(text))
}

fn menu_item(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    detail: impl Into<SharedString>,
) -> Stateful<Div> {
    div()
        .id(id.into())
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .h(px(28.0))
        .px_2()
        .rounded(px(5.0))
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_active()))
        .child(div().truncate().child(label.into()))
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(theme::text_muted())
                .child(detail.into()),
        )
}

fn empty_state(title: &'static str, body: String) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .items_center()
        .justify_center()
        .gap_2()
        .p_6()
        .child(
            div()
                .text_base()
                .font_weight(FontWeight::MEDIUM)
                .child(title),
        )
        .child(
            div()
                .max_w(px(420.0))
                .text_center()
                .text_sm()
                .text_color(theme::text_muted())
                .child(body),
        )
}

fn screen_frame(width: f32, height: f32) -> Div {
    div()
        .relative()
        .flex_none()
        .w(px(width))
        .h(px(height))
        .bg(rgb_black())
        .border_1()
        .border_color(theme::border())
}

fn rgb_black() -> Rgba {
    theme::background()
}

pub(crate) fn hex_string(value: &Value) -> String {
    value
        .as_u64()
        .map(|value| format!("{value:08x}"))
        .unwrap_or_else(|| "—".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_uses_the_tighter_axis_and_never_collapses() {
        let stacked = fit_scale(ScreenLayout::Stacked, 800.0, 480.0 + SCREEN_GAP);
        assert!((stacked - 1.0).abs() < 1e-6, "height bound: {stacked}");
        let side = fit_scale(ScreenLayout::SideBySide, 1440.0 + SCREEN_GAP, 1000.0);
        assert!((side - 2.0).abs() < 1e-6, "width bound: {side}");
        assert_eq!(fit_scale(ScreenLayout::Stacked, 10.0, 10.0), 0.5);
        assert_eq!(fit_scale(ScreenLayout::Stacked, f32::NAN, 10.0), 1.0);
    }

    #[test]
    fn registers_render_as_fixed_width_hex() {
        assert_eq!(hex_string(&serde_json::json!(0x1f)), "0000001f");
        assert_eq!(hex_string(&Value::Null), "—");
    }
}
