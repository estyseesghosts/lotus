//! The LOTUS window.
//!
//! Layout, left to right: the session sidebar (or its chip rail), the centre
//! column (toolbar, screens, gauges), the inspector panel when open, and the
//! inspector rail, which is always shown. Both sidebars resize from a divider
//! and snap to their rail; widths and open state persist in
//! [`crate::preferences`]. Pane widths, toolbar density and gauge mode come
//! from [`crate::layout`] for the space actually available. The window only
//! renders snapshots and sends commands; see [`crate::desktop_controller`].
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, uniform_list, AnyElement, AppContext, ClickEvent, Context, Div, FocusHandle,
    FontWeight, InteractiveElement, IntoElement, KeyDownEvent, KeyUpEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, PathPromptOptions, Render,
    RenderImage, Rgba, SharedString, Stateful, StatefulInteractiveElement, Styled, StyledImage,
    Window,
};
use image::{Frame, RgbaImage};
use lotus_session::supervisor::{HostConfig, SessionKind, SessionSummary};
use lotus_session::workspace::{self, Axis, Layout, Operation, Snapshot, Viewport};
use lotus_session::SessionState;
use serde_json::Value;

use crate::desktop_controller::{Command, DesktopController, McpLink, McpMode};
use crate::gauges;
use crate::icons::{icon, Icon};
use crate::inspection::{Inspection, Panel};
use crate::layout::{self, Drag, GaugeMode, Pane, PaneState, Plan, ToolbarDensity};
use crate::preferences::{self, Store};
use crate::screen_geometry::TouchTransform;
use crate::session_tree::{self, TreeNode};
use crate::theme;
use lotus_protocol::debug::{DebugCommand as D, GpuCapture};

const TOOLBAR_HEIGHT: f32 = 52.0;
const SCREEN_GAP: f32 = 12.0;
const CONTENT_PADDING: f32 = 24.0;
/// Session name, controller and layout controls above each tile of a split.
const TILE_HEADER: f32 = 28.0;
/// Space between the two tiles of a split.
const TILE_GAP: f32 = 12.0;
/// Hit width of a pane divider, inside the pane's edge.
const DIVIDER_WIDTH: f32 = 6.0;

/// Keyboard reference shown in the inspector's utility menu; mirrors
/// `key_down` and `button`.
const KEYS: [(&str, &str); 16] = [
    ("Z / X", "A / B"),
    ("A / S", "X / Y"),
    ("Q / W", "L / R"),
    ("Arrows", "D-pad"),
    ("Enter", "Start"),
    ("Backspace", "Select"),
    ("Space", "Run / pause"),
    (".", "Step one frame"),
    ("Ctrl+O", "Open file"),
    ("Ctrl+B", "Session sidebar"),
    ("Ctrl+I", "Inspector"),
    ("Ctrl+1…7", "Inspector rail views"),
    ("Ctrl+[ / ]", "Previous / next view"),
    ("Tab", "Move focus through controls"),
    ("Enter / Space", "Activate focused control"),
    ("Esc", "Close menu, return keys to the game"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Menu {
    Closed,
    NewSession,
    Load,
    /// Toolbar actions that do not fit.
    Toolbar,
    /// Inspector views not on the rail.
    InspectorMore,
    /// Trace import, clipboard command, keyboard reference.
    InspectorUtility,
    Preferences,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Zoom {
    Fit,
    Native,
    Double,
}

impl Zoom {
    fn label(self) -> &'static str {
        match self {
            Self::Fit => "Fit",
            Self::Native => "1×",
            Self::Double => "2×",
        }
    }
    fn next(self) -> Self {
        match self {
            Self::Fit => Self::Native,
            Self::Native => Self::Double,
            Self::Double => Self::Fit,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScreenLayout {
    Stacked,
    SideBySide,
}

impl ScreenLayout {
    fn label(self) -> &'static str {
        match self {
            Self::Stacked => "Stacked",
            Self::SideBySide => "Side by side",
        }
    }
    fn icon(self) -> Icon {
        match self {
            Self::Stacked => Icon::Stacked,
            Self::SideBySide => Icon::SideBySide,
        }
    }
    fn other(self) -> Self {
        match self {
            Self::Stacked => Self::SideBySide,
            Self::SideBySide => Self::Stacked,
        }
    }
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
    /// What the window shows, as last published by the shared workspace (lotus#67).
    workspace: Snapshot,
    /// Displayed screens of each shown session (lotus#65).
    tiles: BTreeMap<String, TileView>,
    /// Last split plan, so `Auto` holds its choice near a crossover (lotus#69).
    split_held: Cell<Option<SplitPlan>>,
    /// Last resolved layout handed to the supervisor, to report only changes.
    resolved_sent: Cell<Option<workspace::Resolved>>,
    registers: Option<(String, Value)>,
    inspection: Option<(String, Panel, Arc<Inspection>)>,
    inspection_panel: Panel,
    surface_image: Option<Arc<RenderImage>>,
    error: String,
    dismissed_error: String,
    mcp: McpLink,
    pending: usize,
    waiting: Vec<String>,
    artifacts: Option<lotus_session::artifacts::Usage>,
    /// Connected controllers and their assignments (lotus#62), refreshed with each update.
    controllers: crate::input_routing::Controllers,
    focus: FocusHandle,
    /// Keyboard buttons held (3DS bits), for [`Self::keyboard_target`], and pointer touch,
    /// for the tile it started on; the router composes them with the controllers assigned
    /// to each session (lotus#61).
    keys: u32,
    touching: Option<(u16, u16)>,
    touch_session: Option<String>,
    preferences: Store,
    /// The pane opened most recently keeps its width when space runs out.
    recent: Pane,
    drag: Option<Drag>,
    gauges_expanded: bool,
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
            if let Some(controller) = &view.controller {
                controller.ui.activation(window.is_window_active());
            }
            if !window.is_window_active() {
                view.release_input();
                view.end_drag();
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
        let preferences = Store::load(preferences::default_path());
        if let Some(controller) = &controller {
            controller
                .inspector
                .store(preferences.values.inspector_open, Ordering::Release);
            controller.presence.configure(&preferences.values);
            controller
                .host()
                .enable_workspace(lotus_session::workspace::SplitDefaults {
                    axis: preferences.values.split_axis,
                    ratio: preferences.values.split_ratio,
                });
        }
        Self {
            controller,
            startup_error,
            sessions: Vec::new(),
            cards: Vec::new(),
            assets: Vec::new(),
            max_sessions: 0,
            control: false,
            selected: String::new(),
            workspace: Snapshot::default(),
            tiles: BTreeMap::new(),
            split_held: Cell::new(None),
            resolved_sent: Cell::new(None),
            registers: None,
            inspection: None,
            inspection_panel: Panel::Cpu,
            surface_image: None,
            error: String::new(),
            dismissed_error: String::new(),
            mcp: McpLink::Off,
            pending: 0,
            waiting: Vec::new(),
            artifacts: None,
            controllers: crate::input_routing::Controllers::default(),
            focus,
            keys: 0,
            touching: None,
            touch_session: None,
            recent: if preferences.values.inspector_open {
                Pane::Inspector
            } else {
                Pane::Sessions
            },
            preferences,
            drag: None,
            gauges_expanded: false,
            menu: Menu::Closed,
            zoom: Zoom::Fit,
            layout: ScreenLayout::Stacked,
        }
    }

    fn publish_presence(&self) {
        if let Some(controller) = &self.controller {
            controller.presence.configure(&self.preferences.values);
        }
    }

    // ---- panes ----------------------------------------------------------------

    fn pane(&self, pane: Pane) -> PaneState {
        let values = &self.preferences.values;
        match pane {
            Pane::Sessions => PaneState {
                open: values.sessions_open,
                width: values.sessions_width,
            },
            Pane::Inspector => PaneState {
                open: values.inspector_open,
                width: values.inspector_width,
            },
        }
    }

    fn set_pane(&mut self, pane: Pane, state: PaneState) {
        let values = &mut self.preferences.values;
        let was_open = match pane {
            Pane::Sessions => {
                let was = values.sessions_open;
                values.sessions_open = state.open;
                values.sessions_width = state.width;
                was
            }
            Pane::Inspector => {
                let was = values.inspector_open;
                values.inspector_open = state.open;
                values.inspector_width = state.width;
                if let Some(controller) = &self.controller {
                    controller.inspector.store(state.open, Ordering::Release);
                }
                was
            }
        };
        if state.open && !was_open {
            self.recent = pane;
        }
    }

    fn plan(&self, window: &Window) -> Plan {
        layout::plan(
            f32::from(window.viewport_size().width),
            self.pane(Pane::Sessions),
            self.pane(Pane::Inspector),
            self.recent,
        )
    }

    /// Show a pane expanded. A pane drawn as its rail only for lack of space
    /// becomes the most recent one, so the other pane yields instead.
    fn open_pane(&mut self, pane: Pane) {
        let state = PaneState {
            open: true,
            ..self.pane(pane)
        };
        self.set_pane(pane, state);
        self.recent = pane;
        self.preferences.save();
    }

    fn close_pane(&mut self, pane: Pane) {
        let state = PaneState {
            open: false,
            ..self.pane(pane)
        };
        self.set_pane(pane, state);
        self.preferences.save();
    }

    /// Ctrl+B / Ctrl+I and the rail chevrons: toggle what is actually drawn.
    fn toggle_pane(&mut self, pane: Pane, window: &Window) {
        let plan = self.plan(window);
        let shown = match pane {
            Pane::Sessions => plan.sessions.is_some(),
            Pane::Inspector => plan.inspector.is_some(),
        };
        if shown {
            self.close_pane(pane);
        } else {
            self.open_pane(pane);
        }
    }

    fn drag_to(&mut self, x: f32, window: &Window) {
        let Some(drag) = self.drag else {
            return;
        };
        let plan = self.plan(window);
        let other = match drag.pane {
            Pane::Sessions => plan.inspector_width(),
            Pane::Inspector => plan.sessions_width(),
        };
        let state = layout::drag(drag, x, f32::from(window.viewport_size().width), other);
        self.set_pane(drag.pane, state);
    }

    fn end_drag(&mut self) {
        if self.drag.take().is_some() {
            self.preferences.save();
        }
    }

    /// A pane edge that resizes on drag. Lives inside the pane so the centre
    /// column's hitboxes (including the touch screen) never overlap it.
    fn divider(&self, pane: Pane, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.drag.is_some_and(|drag| drag.pane == pane);
        let line = div()
            .absolute()
            .top_0()
            .bottom_0()
            .w(px(if active { 2.0 } else { 1.0 }))
            .when(pane == Pane::Sessions, |line| line.right_0())
            .when(pane == Pane::Inspector, |line| line.left_0())
            .bg(if active {
                theme::info()
            } else {
                theme::border()
            });
        div()
            .id(match pane {
                Pane::Sessions => "sessions-divider",
                Pane::Inspector => "inspector-divider",
            })
            .absolute()
            .top_0()
            .bottom_0()
            .w(px(DIVIDER_WIDTH))
            .when(pane == Pane::Sessions, |edge| edge.right_0())
            .when(pane == Pane::Inspector, |edge| edge.left_0())
            .cursor_col_resize()
            .hover(|style| style.bg(theme::surface_active()))
            .child(line)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |view, _: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    view.menu = Menu::Closed;
                    view.release_input();
                    view.drag = Some(Drag {
                        pane,
                        start_width: view.pane(pane).width,
                    });
                }),
            )
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        let Some(controller) = &self.controller else {
            return;
        };
        let ui = controller.ui.clone();
        ui.poll_started();
        let controllers = controller.input().controllers();
        if controllers != self.controllers {
            self.controllers = controllers;
        }
        let target = self.keyboard_target();
        let (previews, selected, surface) = {
            let mut update = controller.ui.lock_update(&controller.latest);
            self.sessions = update.sessions.clone();
            self.cards = update.cards.clone();
            self.assets = update.assets.clone();
            self.max_sessions = update.max_sessions;
            self.control = update.control;
            self.mcp = update.mcp;
            self.pending = update.pending;
            self.waiting.clone_from(&update.waiting);
            self.artifacts.clone_from(&update.artifacts);
            self.registers = update.registers.clone();
            self.inspection = update.inspection.clone();
            self.workspace.clone_from(&update.workspace);
            if let Layout::Split { axis, ratio, .. } = update.workspace.layout {
                let values = &mut self.preferences.values;
                if (values.split_axis, values.split_ratio) != (axis, ratio) {
                    (values.split_axis, values.split_ratio) = (axis, ratio);
                    self.preferences.save();
                }
            }
            if update.error != self.error {
                self.error = update.error.clone();
            }
            (
                std::mem::take(&mut update.previews),
                update.selected.clone(),
                update.debug_image.take(),
            )
        };
        if selected != self.selected {
            self.selected = selected;
            if let Some(old) = self.surface_image.take() {
                cx.drop_image(old, None);
            }
        }
        // Keyboard follows the selection outside a split and the focused tile inside one;
        // what it held is released when it moves.
        if self.keyboard_target() != target {
            self.release_input();
        }
        if let Some(surface) = surface.filter(|surface| surface.session == self.selected) {
            if let Some(old) = self.surface_image.take() {
                cx.drop_image(old, None);
            }
            if let Some(pixels) = RgbaImage::from_raw(surface.width, surface.height, surface.bgra) {
                self.surface_image = Some(Arc::new(RenderImage::new(vec![Frame::new(pixels)])));
            }
        }
        let visible: Vec<String> = self
            .workspace
            .layout
            .visible()
            .into_iter()
            .map(str::to_string)
            .collect();
        let hidden: Vec<String> = self
            .tiles
            .keys()
            .filter(|session| !visible.contains(session))
            .cloned()
            .collect();
        for session in hidden {
            if let Some(tile) = self.tiles.remove(&session) {
                drop_images(tile.images, cx);
            }
        }
        for session in &visible {
            self.tiles.entry(session.clone()).or_default();
        }
        for (session, preview) in previews {
            let Some(tile) = self.tiles.get_mut(&session) else {
                continue;
            };
            ui.preview_installed(preview.sequence);
            let images = preview
                .frames
                .into_iter()
                .filter_map(|frame| {
                    RgbaImage::from_raw(frame.lease.width, frame.lease.height, frame.bgra)
                })
                .map(|image| Arc::new(RenderImage::new(vec![Frame::new(image)])))
                .collect();
            drop_images(std::mem::replace(&mut tile.images, images), cx);
        }
        for summary in &self.sessions {
            if summary.telemetry.state == SessionState::Crashed {
                if let Some(tile) = self.tiles.get_mut(&summary.info.id) {
                    drop_images(std::mem::take(&mut tile.images), cx);
                }
            }
        }
        ui.poll_completed();
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

    /// Put the full local failure report (with log tails) on the clipboard. Local only.
    fn copy_failure_report(&mut self, session: String, cx: &mut Context<Self>) {
        let Some(controller) = &self.controller else {
            return;
        };
        let host = controller.host().clone();
        cx.spawn(async move |_, cx| {
            // tokio::sync::Mutex needs no Tokio runtime to be awaited.
            let text = match host.failure_report(&session).await {
                Ok(Some(report)) => report,
                Ok(None) => "no failure recorded for this session".to_string(),
                Err(error) => format!("failure report unavailable: {error}"),
            };
            cx.update(|cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(text)));
        })
        .detach();
    }

    /// The session keyboard input drives: the focused tile of a split, otherwise the
    /// selected session. Selecting a session in the sidebar never moves it off a tile.
    fn keyboard_target(&self) -> Option<String> {
        let id = match &self.workspace.layout {
            Layout::Split { first, second, .. } => match self.workspace.focused_viewport {
                Viewport::First => first,
                Viewport::Second => second,
            },
            _ => &self.selected,
        };
        self.sessions
            .iter()
            .any(|s| s.info.id == *id)
            .then(|| id.clone())
    }

    /// Keyboard goes to [`Self::keyboard_target`] and pointer touch to the tile it started
    /// on; controllers keep their own assignment, so nothing here releases them.
    fn publish_input(&mut self) {
        let Some(controller) = &self.controller else {
            return;
        };
        let keyboard = self.keyboard_target();
        let pointer = self
            .touch_session
            .clone()
            .filter(|id| self.sessions.iter().any(|s| s.info.id == *id))
            .or_else(|| keyboard.clone());
        let input = controller.input();
        input.keyboard(keyboard.as_deref(), self.keys);
        input.pointer(pointer.as_deref(), self.touching);
    }

    fn release_input(&mut self) {
        self.keys = 0;
        self.touching = None;
        self.publish_input();
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = &event.keystroke.modifiers;
        let key = event.keystroke.key.as_str();
        if modifiers.control {
            match key {
                "b" => self.toggle_pane(Pane::Sessions, window),
                "i" => self.toggle_pane(Pane::Inspector, window),
                "[" | "]" => self.show_panel(self.inspection_panel.cycle(key == "]")),
                digit => {
                    if let Some(panel) = digit
                        .parse::<usize>()
                        .ok()
                        .and_then(|n| n.checked_sub(1))
                        .and_then(|n| Panel::RAIL.get(n))
                    {
                        self.show_panel(*panel);
                    }
                }
            }
            // Ctrl+O needs a context and is handled in the root listener.
            return;
        }
        match key {
            "tab" => {
                if modifiers.shift {
                    window.focus_prev(cx);
                } else {
                    window.focus_next(cx);
                }
                self.release_input();
                return;
            }
            "escape" => {
                self.menu = Menu::Closed;
                window.focus(&self.focus, cx);
                return;
            }
            _ => {}
        }
        // A focused control owns Enter/Space; game keys apply only while the
        // window itself has focus (Esc or a click on the screens returns it).
        if !self.focus.is_focused(window) {
            return;
        }
        match key {
            "space" if !event.is_held => self.toggle_run(),
            "." if !event.is_held => self.step(),
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
            self.keys |= mask;
        } else {
            self.keys &= !mask;
        }
        self.publish_input();
    }

    /// Touch on `session`'s bottom screen. A touch starting on another tile first lifts
    /// the one held on the previous tile.
    fn touch(&mut self, session: &str, x: f32, y: f32) {
        if self.touch_session.as_deref() != Some(session) {
            self.touching = None;
            self.publish_input();
            self.touch_session = Some(session.to_string());
        }
        let coordinate = self.tiles.get(session).and_then(|tile| {
            tile.touch
                .lock()
                .ok()
                .and_then(|rect| rect.as_ref().and_then(|rect| rect.map_logical(x, y)))
        });
        self.touching = coordinate;
        self.publish_input();
    }

    /// Give keyboard input to a tile of the split. Takes effect when the workspace
    /// publishes the change; [`Self::poll`] then releases what the previous tile held.
    fn focus_viewport(&mut self, viewport: Viewport) {
        if self.workspace.focused_viewport != viewport {
            self.send(Command::Workspace(Operation::FocusViewport { viewport }));
        }
    }

    fn release_touch(&mut self) {
        self.touching = None;
        self.publish_input();
    }

    // ---- sidebar -------------------------------------------------------------

    fn render_sidebar(&self, plan: &Plan, cx: &mut Context<Self>) -> AnyElement {
        let Some(width) = plan.sessions else {
            return self.render_rail(plan, cx);
        };
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
            .w(px(width))
            .h_full()
            .bg(theme::panel())
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
                        icon_button("collapse", Icon::ChevronLeft, "Collapse sidebar (Ctrl+B)")
                            .on_click(cx.listener(|view, _: &ClickEvent, _, _| {
                                view.close_pane(Pane::Sessions);
                            })),
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
                        row.child(
                            icon_button("new-session", Icon::Plus, "New session").on_click(
                                cx.listener(|view, _: &ClickEvent, _, _| {
                                    view.menu = match view.menu {
                                        Menu::NewSession => Menu::Closed,
                                        _ => Menu::NewSession,
                                    };
                                }),
                            ),
                        )
                    }),
            )
            .child(list)
            .child(self.render_controllers(cx))
            .child(self.render_sidebar_footer(cx))
            .child(self.divider(Pane::Sessions, cx))
            .when(self.menu == Menu::NewSession, |sidebar| {
                sidebar.child(self.render_new_menu(cx))
            })
            .into_any_element()
    }

    fn render_rail(&self, plan: &Plan, cx: &mut Context<Self>) -> AnyElement {
        let squeezed = plan.squeezed == Some(Pane::Sessions);
        let mut rail = div()
            .relative()
            .flex()
            .flex_col()
            .items_center()
            .flex_none()
            .w(px(layout::SESSION_RAIL))
            .h_full()
            .bg(theme::panel())
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
                        icon_button(
                            "expand",
                            Icon::ChevronRight,
                            if squeezed {
                                "Expand sidebar (Ctrl+B); the inspector folds to its rail at this width"
                            } else {
                                "Expand sidebar (Ctrl+B)"
                            },
                        )
                        .on_click(cx.listener(|view, _: &ClickEvent, _, _| {
                            view.open_pane(Pane::Sessions);
                        })),
                    ),
            );
        let mut chips = div()
            .id("session-rail")
            .flex()
            .flex_col()
            .flex_1()
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
                        .tab_index(0)
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
                        .focus_visible(|style| style.border_color(theme::info()))
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
        rail = rail
            .child(chips)
            .child(
                div()
                    .flex()
                    .justify_center()
                    .w_full()
                    .py_2()
                    .border_t_1()
                    .border_color(theme::border())
                    .child(self.preferences_button(cx)),
            )
            .child(self.divider(Pane::Sessions, cx));
        rail.into_any_element()
    }

    fn preferences_button(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        icon_button("preferences", Icon::Gear, "Preferences").on_click(cx.listener(
            |view, _: &ClickEvent, _, _| {
                view.menu = match view.menu {
                    Menu::Preferences => Menu::Closed,
                    _ => Menu::Preferences,
                };
            },
        ))
    }

    /// Desktop preferences popover; position is set by the caller.
    fn render_preferences(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let values = &self.preferences.values;
        let presence = values.discord_presence;
        let hide = values.discord_hide_titles;
        let available = crate::presence::application_id(values).is_some();
        popover("preferences-menu")
            .w(px(300.0))
            .on_mouse_down_out(cx.listener(|view, _, _, _| view.menu = Menu::Closed))
            .child(menu_heading("DISCORD"))
            .child(
                checkbox_item(
                    "pref-discord",
                    "Rich Presence",
                    presence,
                    available || presence,
                )
                .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                    let values = &mut view.preferences.values;
                    if !available && !values.discord_presence {
                        return;
                    }
                    values.discord_presence = !values.discord_presence;
                    view.preferences.save();
                    view.publish_presence();
                })),
            )
            .child(
                checkbox_item("pref-hide-titles", "Hide game titles", hide, presence).on_click(
                    cx.listener(move |view, _: &ClickEvent, _, _| {
                        if !presence {
                            return;
                        }
                        let values = &mut view.preferences.values;
                        values.discord_hide_titles = !values.discord_hide_titles;
                        view.preferences.save();
                        view.publish_presence();
                    }),
                ),
            )
            .child(
                div()
                    .px_2()
                    .pb_2()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(if available {
                        "Shows which game is loaded on your Discord profile. Only the file name is sent, or nothing with titles hidden."
                    } else {
                        "Unavailable: discord_application_id in the preferences file is not a Discord application ID."
                    }),
            )
            .when(presence, |menu| {
                let status = self
                    .controller
                    .as_ref()
                    .map(|controller| controller.presence.status())
                    .unwrap_or_default();
                let (colour, text) = match status {
                    crate::presence::Status::Off => (theme::text_muted(), "Starting…".to_string()),
                    crate::presence::Status::Waiting(_) => (
                        theme::warning(),
                        "Discord not reachable; retrying".to_string(),
                    ),
                    crate::presence::Status::Shown(None) => {
                        (theme::success(), "Connected · nothing loaded".to_string())
                    }
                    crate::presence::Status::Shown(Some(activity)) => {
                        (theme::success(), format!("Connected · {}", activity.details))
                    }
                };
                menu.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .pb_2()
                        .text_xs()
                        .child(div().size(px(6.0)).rounded_full().bg(colour))
                        .child(div().truncate().child(text)),
                )
            })
            .when(self.preferences.path.is_none(), |menu| {
                menu.child(
                    div()
                        .px_2()
                        .pb_2()
                        .text_xs()
                        .text_color(theme::warning())
                        .child("Preferences are not saved: no location, or the existing file is unreadable."),
                )
            })
    }

    /// The `tile` that shows a running, loaded, hidden `summary` next to what is shown: beside
    /// the single session, or in place of the unfocused tile of a split. `None` when it is
    /// already shown or there is nothing to tile with.
    fn tile_with(&self, summary: &SessionSummary) -> Option<Operation> {
        let id = &summary.info.id;
        if summary.info.asset.is_none()
            || !summary.telemetry.state.is_active()
            || self.workspace.layout.visible().contains(&id.as_str())
        {
            return None;
        }
        let (first, second) = match &self.workspace.layout {
            Layout::Single { session, .. } => (session.clone(), id.clone()),
            Layout::Split { first, second, .. } => match self.workspace.focused_viewport {
                Viewport::First => (first.clone(), id.clone()),
                Viewport::Second => (id.clone(), second.clone()),
            },
            Layout::Empty => return None,
        };
        Some(Operation::Tile {
            first,
            second,
            axis: None,
            ratio: None,
            screens: None,
        })
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
                    Icon::Plus,
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
        if let Some(tile) = self.tile_with(summary) {
            actions = actions.child(
                icon_button(
                    SharedString::from(format!("tile-{id}")),
                    Icon::SideBySide,
                    "Show beside the current view",
                )
                .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    view.send(Command::Workspace(tile.clone()));
                })),
            );
        }
        if self.control {
            actions = actions.child(
                icon_button(
                    SharedString::from(format!("stop-{id}")),
                    Icon::Close,
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
                        Icon::Play,
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

    fn session_name(&self, id: &str) -> String {
        self.sessions
            .iter()
            .find(|s| s.info.id == id)
            .map_or_else(|| id.to_string(), |s| session_tree::session_label(&s.info))
    }

    /// Assign, unassign or replace from the controller list; refusals go to the error bar.
    fn assign_controller(
        &mut self,
        device: lotus_input::DeviceId,
        session: Option<String>,
        replace: bool,
    ) {
        let Some(controller) = &self.controller else {
            return;
        };
        if let Err(error) = controller
            .input()
            .assign(device, session.as_deref(), replace)
        {
            self.error = format!("Controller: {error}");
        }
    }

    /// Connected controllers (lotus#62): which session each drives, assign/unassign against
    /// the selected session, and "Identify" to find a physical pad by pressing a button.
    /// Assignment follows running session identity, never selection or focus.
    fn render_controllers(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let listing = &self.controllers;
        let selected = self
            .sessions
            .iter()
            .any(|s| s.info.id == self.selected)
            .then(|| self.selected.clone());
        let occupant = selected.as_ref().and_then(|session| {
            listing
                .pads
                .iter()
                .find(|p| p.session.as_ref() == Some(session))
                .map(|p| p.id)
        });
        let mut section = div()
            .flex()
            .flex_col()
            .gap_1()
            .pl_4()
            .pr_2()
            .py_2()
            .border_t_1()
            .border_color(theme::border())
            .text_xs()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(section_label("CONTROLLERS"))
                    .when(!listing.pads.is_empty(), |row| {
                        let label = if listing.identifying {
                            "Press a button…"
                        } else {
                            "Identify"
                        };
                        row.child(with_tooltip(
                            small_button("identify-controller", label, Tone::Plain, true).on_click(
                                cx.listener(|view, _: &ClickEvent, _, _| {
                                    if let Some(controller) = &view.controller {
                                        controller.input().identify();
                                    }
                                }),
                            ),
                            "Highlight the next controller that presses a button",
                        ))
                    }),
            );
        if listing.pads.is_empty() {
            section = section.child(
                div()
                    .text_color(theme::text_muted())
                    .child("No controllers connected."),
            );
        }
        for pad in &listing.pads {
            let device = pad.id;
            let identified = listing.identified == Some(device);
            let target = match &pad.session {
                Some(session) => self.session_name(session),
                None => "unassigned".to_string(),
            };
            let mut row = div()
                .id(SharedString::from(format!("controller-{device}")))
                .flex()
                .items_center()
                .gap_2()
                .child(div().size(px(6.0)).rounded_full().bg(if identified {
                    theme::warning()
                } else if pad.session.is_some() {
                    theme::success()
                } else {
                    theme::border_strong()
                }))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(div().truncate().child(format!("{} · {device}", pad.name)))
                        .child(
                            div()
                                .truncate()
                                .text_color(theme::text_muted())
                                .child(target),
                        ),
                );
            let on_selected = pad.session.is_some() && pad.session == selected;
            if let Some(session) = selected.clone().filter(|_| self.control && !on_selected) {
                // The selected session has another controller: offer to replace it.
                let replace = occupant.is_some();
                let (label, tip) = if replace {
                    (
                        "Replace",
                        "Give the selected session this controller instead",
                    )
                } else {
                    ("Assign", "Drive the selected session with this controller")
                };
                row = row.child(with_tooltip(
                    small_button(format!("assign-{device}"), label, Tone::Plain, true).on_click(
                        cx.listener(move |view, _: &ClickEvent, _, _| {
                            view.assign_controller(device, Some(session.clone()), replace);
                        }),
                    ),
                    tip,
                ));
            }
            if pad.session.is_some() && self.control {
                row = row.child(
                    small_button(format!("unassign-{device}"), "Unassign", Tone::Plain, true)
                        .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                            view.assign_controller(device, None, false);
                        })),
                );
            }
            section = section.child(row);
        }
        for entry in &listing.remembered {
            section = section.child(div().truncate().text_color(theme::text_muted()).child(
                format!(
                    "{} disconnected; was on {}",
                    entry.name,
                    self.session_name(&entry.session)
                ),
            ));
        }
        section
    }

    fn render_sidebar_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
            .pl_4()
            .pr_2()
            .py_2()
            .border_t_1()
            .border_color(theme::border())
            .text_xs()
            .text_color(theme::text_muted())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().size(px(6.0)).rounded_full().bg(colour))
                    .child(div().flex_1().truncate().child(text))
                    .child(div().font_family(theme::MONO).child(slots))
                    .child(self.preferences_button(cx)),
            )
            .when_some(self.artifacts.as_ref(), |footer, usage| {
                // Quota and retention at a glance; details per session in lotus_status.
                let pinned: usize = usage.sessions.values().map(|s| s.pinned).sum();
                footer.child(div().font_family(theme::MONO).truncate().child(format!(
                    "artifacts {}/{} · {}/{} MiB · {pinned} pinned",
                    usage.count,
                    usage.max_count,
                    usage.bytes >> 20,
                    usage.max_bytes >> 20
                )))
            })
            .when(!self.control, |footer| footer.child("Read-only host"))
    }

    // ---- main ----------------------------------------------------------------

    /// Session toolbar. Run/Pause and Step stay visible at every width; the
    /// rest become icons, then move into the overflow menu
    /// ([`layout::toolbar_density`]).
    fn render_toolbar(&self, density: ToolbarDensity, cx: &mut Context<Self>) -> impl IntoElement {
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
        let detail = match (&summary.info.kind, summary.info.asset.as_deref()) {
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
        };
        let label = session_tree::session_label(&summary.info);
        bar = bar.child(with_tooltip(
            div()
                .id("session-label")
                .flex_shrink_1()
                .min_w(px(48.0))
                .truncate()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .child(label.clone()),
            format!("{label}\n{kind_label} · {detail}"),
        ));
        if density >= ToolbarDensity::Icons {
            bar = bar.child(div().flex_none().child(badge(
                kind_label,
                if kind_label == "PRIMARY" {
                    theme::info()
                } else {
                    theme::warning()
                },
            )));
        }
        if density == ToolbarDensity::Full {
            bar = bar.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .truncate()
                    .child(detail),
            );
        } else {
            bar = bar.child(div().flex_1().min_w_0());
        }
        if (self.pending > 0 || !self.waiting.is_empty()) && density >= ToolbarDensity::Icons {
            // A slow worker operation is named instead of freezing unrelated controls.
            let text = self
                .waiting
                .first()
                .map_or_else(|| "working…".to_string(), |slow| format!("waiting: {slow}"));
            bar = bar.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(text),
            );
        }
        let labelled = density == ToolbarDensity::Full;
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
                    tool_button(
                        "step",
                        "Step",
                        Icon::Step,
                        "Step one frame (.)",
                        labelled,
                        loaded && !playing,
                        Tone::Plain,
                    )
                    .on_click(cx.listener(|view, _: &ClickEvent, _, _| view.step())),
                );
        }
        if density == ToolbarDensity::Compact {
            return bar.child(
                icon_button("toolbar-more", Icon::More, "More actions").on_click(cx.listener(
                    |view, _: &ClickEvent, _, _| {
                        view.menu = match view.menu {
                            Menu::Toolbar => Menu::Closed,
                            _ => Menu::Toolbar,
                        };
                    },
                )),
            );
        }
        if self.control && active {
            let reset_id = id.clone();
            bar = bar
                .child(
                    tool_button(
                        "reset",
                        "Reset",
                        Icon::Reset,
                        "Reset the guest",
                        labelled,
                        loaded,
                        Tone::Plain,
                    )
                    .on_click(cx.listener(
                        move |view, _: &ClickEvent, _, _| {
                            view.send(Command::Reset(reset_id.clone()))
                        },
                    )),
                )
                .child(
                    tool_button(
                        "load",
                        "Load",
                        Icon::Load,
                        "Load content",
                        labelled,
                        true,
                        Tone::Plain,
                    )
                    .on_click(cx.listener(|view, _: &ClickEvent, _, _| {
                        view.menu = match view.menu {
                            Menu::Load => Menu::Closed,
                            _ => Menu::Load,
                        };
                    })),
                );
        }
        if self.control {
            let stop_id = id.clone();
            bar = bar.child(
                tool_button(
                    "stop",
                    "Stop",
                    Icon::Stop,
                    "Stop session",
                    labelled,
                    true,
                    Tone::Danger,
                )
                .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                    view.send(Command::Stop(stop_id.clone()))
                })),
            );
        }
        let zoom_tip = format!(
            "Zoom: {} (click for {})",
            self.zoom.label(),
            self.zoom.next().label()
        );
        let layout_tip = format!(
            "Screens: {} (click for {})",
            self.layout.label(),
            self.layout.other().label()
        );
        let zoom = if labelled {
            button("zoom", self.zoom.label(), Tone::Plain, true)
        } else {
            with_tooltip(
                icon_tool("zoom", Icon::Zoom).child(
                    div()
                        .text_xs()
                        .font_family(theme::MONO)
                        .child(self.zoom.label()),
                ),
                zoom_tip,
            )
        };
        let screens = if labelled {
            button("layout", self.layout.label(), Tone::Plain, true)
        } else {
            with_tooltip(icon_tool("layout", self.layout.icon()), layout_tip)
        };
        bar.child(
            div()
                .flex_none()
                .w(px(1.0))
                .h(px(20.0))
                .mx_1()
                .bg(theme::border()),
        )
        .child(zoom.on_click(cx.listener(|view, _: &ClickEvent, _, _| {
            view.release_input();
            view.zoom = view.zoom.next();
        })))
        .child(screens.on_click(cx.listener(|view, _: &ClickEvent, _, _| {
            view.release_input();
            view.layout = view.layout.other();
        })))
    }

    /// Everything the compact toolbar leaves out.
    fn render_toolbar_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut menu = popover("toolbar-menu")
            .top(px(TOOLBAR_HEIGHT - 4.0))
            .right(px(16.0))
            .w(px(240.0))
            .on_mouse_down_out(cx.listener(|view, _, _, _| view.menu = Menu::Closed));
        if let Some(summary) = self.selected_summary() {
            let id = summary.info.id.clone();
            let loaded = summary.info.asset.is_some();
            if self.control && summary.telemetry.state.is_active() {
                let reset_id = id.clone();
                menu = menu
                    .child(menu_heading("SESSION"))
                    .child(
                        menu_item("menu-reset", "Reset", "")
                            .when(!loaded, |item| item.opacity(0.4))
                            .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                                view.menu = Menu::Closed;
                                if loaded {
                                    view.send(Command::Reset(reset_id.clone()));
                                }
                            })),
                    )
                    .child(menu_item("menu-load", "Load…", "").on_click(
                        cx.listener(|view, _: &ClickEvent, _, _| view.menu = Menu::Load),
                    ));
            }
            if self.control {
                menu = menu.child(
                    menu_item("menu-stop", "Stop", "")
                        .text_color(theme::error())
                        .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                            view.menu = Menu::Closed;
                            view.send(Command::Stop(id.clone()));
                        })),
                );
            }
            menu = menu.child(div().h(px(1.0)).my_1().bg(theme::border()));
        }
        menu = menu.child(menu_heading("ZOOM"));
        for zoom in [Zoom::Fit, Zoom::Native, Zoom::Double] {
            menu = menu.child(
                checkbox_item(
                    SharedString::from(format!("menu-zoom-{}", zoom.label())),
                    zoom.label(),
                    self.zoom == zoom,
                    true,
                )
                .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                    view.release_input();
                    view.zoom = zoom;
                    view.menu = Menu::Closed;
                })),
            );
        }
        menu = menu.child(menu_heading("SCREENS"));
        for layout in [ScreenLayout::Stacked, ScreenLayout::SideBySide] {
            menu = menu.child(
                checkbox_item(
                    SharedString::from(format!("menu-layout-{}", layout.label())),
                    layout.label(),
                    self.layout == layout,
                    true,
                )
                .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                    view.release_input();
                    view.layout = layout;
                    view.menu = Menu::Closed;
                })),
            );
        }
        menu
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

    fn render_stage(
        &self,
        plan: &Plan,
        gauge_height: f32,
        viewport_height: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
        if let Layout::Split { .. } = &self.workspace.layout {
            return self.render_split(plan, gauge_height, viewport_height, cx);
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
            let known = summary
                .telemetry
                .failure
                .as_ref()
                .is_some_and(|failure| failure["renderer"] == true);
            let session = summary.info.id.clone();
            return empty_state(
                "Worker crashed",
                format!(
                    "{} {} Logs and failure.json are kept in the session root. Stop the session \
                     to release its slot.",
                    if known {
                        "Known renderer error."
                    } else if summary.telemetry.failure.is_some() {
                        "Not a known renderer error."
                    } else {
                        "Cause unknown."
                    },
                    summary.telemetry.last_error
                ),
            )
            .child(
                div().mt_2().child(
                    button(
                        "copy-crash-report",
                        "Copy local crash report",
                        Tone::Plain,
                        true,
                    )
                    .on_click(cx.listener(
                        move |view, _: &ClickEvent, _, cx| {
                            view.copy_failure_report(session.clone(), cx);
                        },
                    )),
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
        let _ = window;
        let scale = self.screen_scale(plan, gauge_height, viewport_height);
        let screens = self.render_screens(&summary.info.id, self.layout, scale, cx);
        div()
            .id("stage")
            .flex()
            .flex_1()
            .items_center()
            .justify_center()
            .overflow_scroll()
            .child(screens)
            .into_any_element()
    }

    /// Two tiles along the resolved split axis, each with its own header, screens, touch
    /// mapping and images (lotus#65). Geometry comes from [`split_plan`].
    fn render_split(
        &self,
        plan: &Plan,
        gauge_height: f32,
        viewport_height: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Layout::Split {
            first,
            second,
            axis,
            ratio,
            screens,
        } = &self.workspace.layout
        else {
            return div().into_any_element();
        };
        let width = plan.main - 2.0 * CONTENT_PADDING;
        let height = viewport_height - TOOLBAR_HEIGHT - gauge_height - 2.0 * CONTENT_PADDING;
        let split = split_plan(
            width,
            height,
            *axis,
            *ratio,
            *screens,
            self.split_held.get(),
        );
        self.split_held.set(Some(split));
        let scale = split.tiles.map(|tile| match self.zoom {
            Zoom::Native => 1.0,
            Zoom::Double => 2.0,
            Zoom::Fit => tile.scale,
        });
        self.report_resolved(workspace::Resolved {
            revision: self.workspace.revision,
            axis: if split.vertical {
                Axis::Vertical
            } else {
                Axis::Horizontal
            },
            screens: split.tiles.map(|tile| match tile.screens {
                ScreenLayout::Stacked => workspace::ScreenLayout::Stacked,
                ScreenLayout::SideBySide => workspace::ScreenLayout::SideBySide,
            }),
            scale,
        });
        let tiles = [
            (Viewport::First, first, second, split.tiles[0]),
            (Viewport::Second, second, first, split.tiles[1]),
        ];
        div()
            .id("stage")
            .flex()
            .flex_1()
            .items_center()
            .justify_center()
            .overflow_scroll()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap(px(TILE_GAP))
                    .when(split.vertical, |row| row.flex_col())
                    .children(tiles.into_iter().map(|(viewport, session, other, tile)| {
                        self.render_tile(viewport, session, other, tile, *axis, cx)
                    })),
            )
            .into_any_element()
    }

    /// Tells the supervisor what the split resolved to when it changes. Render-time, so it
    /// sends directly; a full queue is retried on the next frame.
    fn report_resolved(&self, resolved: workspace::Resolved) {
        if self.resolved_sent.get() == Some(resolved) {
            return;
        }
        if let Some(controller) = &self.controller {
            if controller
                .commands
                .try_send(Command::WorkspaceResolved(resolved))
                .is_ok()
            {
                self.resolved_sent.set(Some(resolved));
            }
        }
    }

    fn render_tile(
        &self,
        viewport: Viewport,
        session: &str,
        other: &str,
        tile: TilePlan,
        axis: Axis,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focused = self.workspace.focused_viewport == viewport;
        let index = match viewport {
            Viewport::First => 0,
            Viewport::Second => 1,
        };
        let summary = self.sessions.iter().find(|s| s.info.id == session);
        let label = summary.map_or_else(
            || session_tree::short_id(session).to_string(),
            |summary| session_tree::session_label(&summary.info).to_string(),
        );
        let pads: Vec<String> = self
            .controllers
            .pads
            .iter()
            .filter(|pad| pad.session.as_deref() == Some(session))
            .map(|pad| pad.name.clone())
            .collect();
        let keep = other.to_string();
        let mut header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .h(px(TILE_HEADER))
            .px_2()
            .text_xs()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if focused {
                        theme::text()
                    } else {
                        theme::text_secondary()
                    })
                    .child(label),
            )
            .children(pads.into_iter().map(|name| {
                div()
                    .flex_none()
                    .max_w(px(140.0))
                    .truncate()
                    .px_1()
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(theme::border_strong())
                    .text_color(theme::text_secondary())
                    .child(name)
            }))
            .when(focused, |header| {
                header.child(
                    div()
                        .flex_none()
                        .text_color(theme::info())
                        .child("Keyboard"),
                )
            })
            .child(
                icon_button(
                    SharedString::from(format!("tile-screens-{index}")),
                    tile.screens.other().icon(),
                    "Switch this tile's screen arrangement",
                )
                .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    let screens = match tile.screens.other() {
                        ScreenLayout::Stacked => workspace::ScreenLayout::Stacked,
                        ScreenLayout::SideBySide => workspace::ScreenLayout::SideBySide,
                    };
                    view.send(Command::Workspace(Operation::SetScreenLayout {
                        viewport,
                        screens,
                    }));
                })),
            );
        if viewport == Viewport::First {
            let (next, label, description) = match axis {
                Axis::Auto => (
                    Axis::Horizontal,
                    "Auto",
                    "Split direction: automatic. Click for left and right",
                ),
                Axis::Horizontal => (
                    Axis::Vertical,
                    "Left | Right",
                    "Split direction: left and right. Click for top and bottom",
                ),
                Axis::Vertical => (
                    Axis::Auto,
                    "Top | Bottom",
                    "Split direction: top and bottom. Click for automatic",
                ),
            };
            header = header.child(with_tooltip(
                small_button("tile-axis", label, Tone::Plain, true).on_click(cx.listener(
                    move |view, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        view.send(Command::Workspace(Operation::SetAxis { axis: next }));
                    },
                )),
                description,
            ));
        }
        header = header.child(
            icon_button(
                SharedString::from(format!("untile-{index}")),
                Icon::Close,
                "Return to the sidebar; the session keeps running",
            )
            .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                view.send(Command::Workspace(Operation::Untile {
                    keep: Some(keep.clone()),
                }));
                // Outside a split the window shows the selection; select what stays.
                view.select(keep.clone());
            })),
        );
        let body = match summary {
            None => tile_message("Session ended"),
            Some(summary) if summary.telemetry.state == SessionState::Crashed => {
                tile_message("Worker crashed")
            }
            Some(summary) if summary.info.asset.is_none() => tile_message("Nothing loaded"),
            Some(_) => {
                let scale = match self.zoom {
                    Zoom::Native => 1.0,
                    Zoom::Double => 2.0,
                    Zoom::Fit => tile.scale,
                };
                div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .child(self.render_screens(session, tile.screens, scale, cx))
                    .into_any_element()
            }
        };
        div()
            .id(SharedString::from(format!("tile-{index}")))
            .flex()
            .flex_col()
            .flex_none()
            .w(px(tile.width.max(0.0)))
            .h(px(tile.height.max(0.0)))
            .rounded(px(6.0))
            .border_1()
            .border_color(if focused {
                theme::info()
            } else {
                theme::border()
            })
            .overflow_hidden()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |view, _: &MouseDownEvent, _, _| view.focus_viewport(viewport)),
            )
            .child(header)
            .child(body)
            .into_any_element()
    }

    /// Both screens of `session` at `scale`, with its own touch mapping.
    fn render_screens(
        &self,
        session: &str,
        layout: ScreenLayout,
        scale: f32,
        cx: &mut Context<Self>,
    ) -> Div {
        let tile = self.tiles.get(session);
        let mut top = screen_frame(400.0 * scale, 240.0 * scale).id("top-screen");
        if let Some(image) = tile.and_then(|tile| tile.images.first()) {
            top = top.child(gpui::img(image.clone()).size_full());
        }
        let rect = tile.map(|tile| tile.touch.clone()).unwrap_or_default();
        let (down, moved) = (session.to_string(), session.to_string());
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
                cx.listener(move |view, event: &MouseDownEvent, _, _| {
                    view.touch(&down, event.position.x.into(), event.position.y.into())
                }),
            )
            .on_mouse_move(cx.listener(move |view, event: &MouseMoveEvent, _, _| {
                if view.touching.is_some()
                    && event.dragging()
                    && view.touch_session.as_deref() == Some(moved.as_str())
                {
                    view.touch(&moved, event.position.x.into(), event.position.y.into());
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
        if let Some(image) = tile.and_then(|tile| tile.images.get(1)) {
            bottom = bottom.child(gpui::img(image.clone()).size_full());
        }
        div()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(SCREEN_GAP))
            .when(layout == ScreenLayout::Stacked, |screens| {
                screens.flex_col()
            })
            .child(top)
            .child(bottom)
    }

    /// Fit picks the largest scale that shows both screens without scrolling
    /// in the centre column left by the panes, toolbar and gauge strip.
    fn screen_scale(&self, plan: &Plan, gauge_height: f32, viewport_height: f32) -> f32 {
        match self.zoom {
            Zoom::Native => 1.0,
            Zoom::Double => 2.0,
            Zoom::Fit => {
                let width = plan.main - 2.0 * CONTENT_PADDING;
                let height =
                    viewport_height - TOOLBAR_HEIGHT - gauge_height - 2.0 * CONTENT_PADDING;
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
    /// Select an inspector view and make sure the panel shows it.
    fn show_panel(&mut self, panel: Panel) {
        self.menu = Menu::Closed;
        self.choose_inspection(panel, 0);
        if !self.preferences.values.inspector_open || self.recent != Pane::Inspector {
            self.open_pane(Pane::Inspector);
        }
    }

    /// The inspector rail: always visible at the window's right edge. Views
    /// (navigation) at the top, execution quick actions separated at the
    /// bottom. No Stop/Reset/trace commands here.
    fn render_inspector_rail(&self, plan: &Plan, cx: &mut Context<Self>) -> impl IntoElement {
        let open = plan.inspector.is_some();
        let squeezed = plan.squeezed == Some(Pane::Inspector);
        let mut views = div()
            .id("inspector-views")
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .py_2()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll();
        let mut group = "";
        for (index, panel) in Panel::RAIL.into_iter().enumerate() {
            if !group.is_empty() && group != panel.group() {
                views = views.child(div().w(px(20.0)).h(px(1.0)).my_1().bg(theme::border()));
            }
            group = panel.group();
            let current = open && self.inspection_panel == panel;
            views = views.child(
                rail_button(
                    SharedString::from(format!("rail-{}", panel as usize)),
                    panel.icon(),
                    current,
                    true,
                    format!("{} (Ctrl+{})", panel.label(), index + 1),
                )
                .on_click(cx.listener(move |view, _: &ClickEvent, _, _| view.show_panel(panel))),
            );
        }
        let overflow_current = open && Panel::OVERFLOW.contains(&self.inspection_panel);
        views = views.child(div().w(px(20.0)).h(px(1.0)).my_1().bg(theme::border()));
        views = views.child(
            rail_button(
                "rail-more",
                Icon::More,
                overflow_current,
                true,
                if overflow_current {
                    format!("More views (showing {})", self.inspection_panel.label())
                } else {
                    "More views: IPC, timeline, audio, settings, result".to_string()
                },
            )
            .on_click(cx.listener(|view, _: &ClickEvent, _, _| {
                view.menu = match view.menu {
                    Menu::InspectorMore => Menu::Closed,
                    _ => Menu::InspectorMore,
                };
            })),
        );
        let summary = self.selected_summary();
        let loaded =
            summary.is_some_and(|s| s.info.asset.is_some() && s.telemetry.state.is_active());
        let playing = summary.is_some_and(|s| s.telemetry.playing);
        let usable = self.control && loaded;
        let why = if !self.control {
            " — read-only host"
        } else if !loaded {
            " — nothing loaded"
        } else {
            ""
        };
        let actions = div()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .py_2()
            .border_t_1()
            .border_color(theme::border())
            .child(
                rail_button(
                    "rail-run",
                    if playing { Icon::Pause } else { Icon::Play },
                    false,
                    usable,
                    format!("{} (Space){why}", if playing { "Pause" } else { "Run" }),
                )
                .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                    if usable {
                        view.toggle_run();
                    }
                })),
            )
            .child(
                rail_button(
                    "rail-step",
                    Icon::Step,
                    false,
                    usable && !playing,
                    format!(
                        "Step one frame (.){}",
                        if why.is_empty() && playing {
                            " — pause first"
                        } else {
                            why
                        }
                    ),
                )
                .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                    if usable && !playing {
                        view.step();
                    }
                })),
            );
        div()
            .id("inspector-rail")
            .relative()
            .flex()
            .flex_col()
            .items_center()
            .flex_none()
            .w(px(layout::INSPECTOR_RAIL))
            .h_full()
            .bg(theme::panel())
            .border_l_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .flex_none()
                    .h(px(TOOLBAR_HEIGHT))
                    .w_full()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(
                        icon_button(
                            "inspector-toggle",
                            if open { Icon::ChevronRight } else { Icon::ChevronLeft },
                            if open {
                                "Collapse inspector (Ctrl+I)"
                            } else if squeezed {
                                "Expand inspector (Ctrl+I); the session sidebar folds to its rail at this width"
                            } else {
                                "Expand inspector (Ctrl+I)"
                            },
                        )
                        .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                            if open {
                                view.close_pane(Pane::Inspector);
                            } else {
                                view.open_pane(Pane::Inspector);
                            }
                        })),
                    ),
            )
            .child(views)
            .child(actions)
            .when(!open, |rail| rail.child(self.divider(Pane::Inspector, cx)))
            .when(self.menu == Menu::InspectorMore, |rail| {
                rail.child(self.render_more_views(cx))
            })
    }

    fn render_more_views(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut menu = popover("inspector-more")
            .top(px(TOOLBAR_HEIGHT + 8.0))
            .right(px(layout::INSPECTOR_RAIL - 4.0))
            .w(px(200.0))
            .on_mouse_down_out(cx.listener(|view, _, _, _| view.menu = Menu::Closed));
        let mut group = "";
        for panel in Panel::OVERFLOW {
            if group != panel.group() {
                group = panel.group();
                menu = menu.child(menu_heading(group));
            }
            let current = self.inspection_panel == panel;
            menu = menu.child(
                menu_item(
                    SharedString::from(format!("more-{}", panel as usize)),
                    panel.label(),
                    if current { "shown" } else { "" },
                )
                .when(current, |item| item.bg(theme::surface_active()))
                .on_click(cx.listener(move |view, _: &ClickEvent, _, _| view.show_panel(panel))),
            );
        }
        menu
    }

    /// Commands that belong to the selected view. `true` marks a command that
    /// replaces or rewinds guest state.
    fn panel_commands(panel: Panel) -> Vec<(&'static str, D, bool)> {
        // `draws` snapshots registers and shader state of the next 8 draws without resource
        // copies; `full` copies every draw's resources (lotus#24).
        let gpu = |enabled, capture| D::GpuConfigure {
            enabled,
            stop_events: vec![],
            capture: Some(capture),
            draw_range: None,
            max_draws: None,
            resources: None,
        };
        match panel {
            Panel::Cpu => vec![
                ("Step", D::Step { core: 0, count: 1 }, false),
                ("Over", D::StepOver { core: 0 }, false),
                ("Out", D::StepOut { core: 0 }, false),
                ("Back", D::ReverseStep { core: 0 }, true),
            ],
            Panel::States => vec![
                (
                    "Save desktop",
                    D::SaveState {
                        name: "desktop".into(),
                    },
                    false,
                ),
                (
                    "Enable rewind",
                    D::RewindConfigure {
                        enabled: true,
                        capacity: 64,
                        budget_mib: 128,
                        interval_frames: None,
                        compression_level: None,
                    },
                    false,
                ),
                ("Reverse stop", D::ReverseContinue, true),
            ],
            Panel::Gpu => vec![
                ("Count draws", gpu(true, GpuCapture::Counters), false),
                ("Capture 8 draws", gpu(true, GpuCapture::Draws), false),
                ("Full capture", gpu(true, GpuCapture::Full), false),
                ("Disable capture", gpu(false, GpuCapture::Counters), false),
                ("Record CiTrace", D::CitraceStart, false),
                ("Finish trace", D::CitraceStop, false),
                ("Replay trace", D::CitraceReplay, true),
            ],
            Panel::Ipc | Panel::Timeline => vec![
                ("Trace on", D::Trace { enabled: true }, false),
                ("Trace off", D::Trace { enabled: false }, false),
            ],
            Panel::Audio => vec![
                ("Focus audio", D::AudioFocus { enabled: true }, false),
                ("Mute", D::AudioFocus { enabled: false }, false),
                ("Record WAV", D::AudioCapture { enabled: true }, false),
                ("Finish WAV", D::AudioCapture { enabled: false }, false),
            ],
            _ => vec![],
        }
    }

    fn render_inspector(&self, width: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.inspection_panel;
        let mut panel = div()
            .id("inspector")
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(width))
            .h_full()
            .bg(theme::panel())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .h(px(TOOLBAR_HEIGHT))
                    .pl_4()
                    .pr_2()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(icon(current.icon(), 16.0, theme::text()))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .flex_none()
                            .child(current.label()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(section_label(current.group())),
                    )
                    .child(
                        icon_button(
                            "inspector-utility",
                            Icon::More,
                            "Trace import, clipboard command, keys",
                        )
                        .on_click(cx.listener(
                            |view, _: &ClickEvent, _, _| {
                                view.menu = match view.menu {
                                    Menu::InspectorUtility => Menu::Closed,
                                    _ => Menu::InspectorUtility,
                                };
                            },
                        )),
                    ),
            );
        let commands = Self::panel_commands(current);
        if !commands.is_empty() {
            let mut actions = div()
                .flex()
                .flex_none()
                .flex_wrap()
                .gap_1()
                .px_2()
                .py_2()
                .border_b_1()
                .border_color(theme::border());
            for (label, command, disruptive) in commands {
                let tone = if disruptive {
                    Tone::Danger
                } else {
                    Tone::Plain
                };
                actions = actions.child(
                    small_button(format!("debug-{label}"), label, tone, self.control).on_click(
                        cx.listener(move |view, _, _, _| {
                            if view.control {
                                view.debug_command(command.clone());
                            }
                        }),
                    ),
                );
            }
            panel = panel.child(actions);
        }
        panel = panel.child(self.divider(Pane::Inspector, cx));
        // Popovers paint last so the row list never covers them.
        let utility = (self.menu == Menu::InspectorUtility).then(|| self.render_utility_menu(cx));
        let view = self
            .inspection
            .as_ref()
            .filter(|(session, tab, _)| *session == self.selected && *tab == current);
        let Some((_, _, view)) = view else {
            return panel
                .child(
                    div()
                        .p_3()
                        .text_sm()
                        .text_color(theme::text_secondary())
                        .child("Select a session and inspector. Results are safe-point snapshots; unsupported backend operations show their reason."),
                )
                .children(utility);
        };
        panel = panel
            .child(
                div()
                    .px_2()
                    .pt_2()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .truncate()
                    .child(view.source.clone()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_2()
                    .pb_1()
                    .text_xs()
                    .child(format!("{} rows · {} lost", view.rows.len(), view.lost))
                    .when_some(view.next, |row, next| {
                        row.child(
                            small_button("inspect-next", "Next page", Tone::Plain, true).on_click(
                                cx.listener(move |view, _, _, _| {
                                    view.choose_inspection(current, next)
                                }),
                            ),
                        )
                    }),
            );
        if current == Panel::Result {
            if let Some(image) = &self.surface_image {
                panel = panel.child(
                    gpui::img(image.clone())
                        .w_full()
                        .h(px(180.0))
                        .object_fit(gpui::ObjectFit::Contain),
                );
            }
        }
        // Key column scales with the panel; the lane column goes first when narrow.
        let show_lane = width >= 360.0;
        let key_width = (width * 0.36).clamp(90.0, 220.0);
        let rows = view.rows.clone();
        let weak = cx.entity().downgrade();
        panel
            .child(
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
                                    .items_center()
                                    .overflow_hidden()
                                    .font_family(theme::MONO)
                                    .text_xs()
                                    .when(show_lane, |line| {
                                        line.child(
                                            div()
                                                .w(px(48.0))
                                                .flex_none()
                                                .text_color(theme::text_muted())
                                                .child(row.lane),
                                        )
                                    })
                                    .child(
                                        div()
                                            .w(px(key_width))
                                            .flex_none()
                                            .truncate()
                                            .child(row.key.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .child(row.value.clone()),
                                    ),
                                tooltip,
                            )
                            .when_some(action, |element, command| {
                                element
                                    .cursor_pointer()
                                    .hover(|style| style.bg(theme::surface_hover()))
                                    .on_click(move |_, _, cx| {
                                        let _ = weak.update(cx, |view, _| {
                                            view.debug_command(command.clone())
                                        });
                                    })
                            })
                        })
                        .collect()
                })
                .flex_1()
                .min_h_0(),
            )
            .children(utility)
    }

    /// Utilities that are not tied to the selected view.
    fn render_utility_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut keys = div().flex().flex_col().px_2().pb_1();
        for (key, action) in KEYS {
            keys = keys.child(
                div()
                    .flex()
                    .justify_between()
                    .gap_3()
                    .h(px(20.0))
                    .text_xs()
                    .child(div().font_family(theme::MONO).child(key))
                    .child(div().text_color(theme::text_muted()).child(action)),
            );
        }
        popover("inspector-utility-menu")
            .top(px(TOOLBAR_HEIGHT - 6.0))
            .right(px(8.0))
            .w(px(280.0))
            .on_mouse_down_out(cx.listener(|view, _, _, _| view.menu = Menu::Closed))
            .child(
                menu_item("open-trace", "Open JSONL trace…", "≤ 64 MiB").on_click(cx.listener(
                    |view, _, _, cx| {
                        view.menu = Menu::Closed;
                        view.open_trace(cx);
                    },
                )),
            )
            .child(
                menu_item("clipboard-command", "Run clipboard JSON", "debug command").on_click(
                    cx.listener(|view, _, _, cx| {
                        view.menu = Menu::Closed;
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
            )
            .child(div().h(px(1.0)).my_1().bg(theme::border()))
            .child(menu_heading("KEYBOARD"))
            .child(keys)
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
                    icon_button("dismiss-error", Icon::Close, "Dismiss").on_click(cx.listener(
                        |view, _: &ClickEvent, _, _| view.dismissed_error = view.error.clone(),
                    )),
                ),
        )
    }

    fn render_gauges(
        &self,
        mode: GaugeMode,
        main: f32,
        summary: &SessionSummary,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let telemetry = &summary.telemetry;
        match mode {
            GaugeMode::Cards => gauges::gauge_cards(telemetry).into_any_element(),
            GaugeMode::Compact { expanded } => {
                let pills = layout::pills(main);
                let [fps, speed, frame, rest @ ..] = gauges::gauges(telemetry);
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(px(layout::PILL_GAP))
                    .w(px(main))
                    .p(px(layout::PILL_PADDING))
                    .border_t_1()
                    .border_color(theme::border())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(layout::PILL_GAP))
                            .child(gauges::gauge_pills(
                                [fps, speed, frame],
                                pills.primary_width,
                                false,
                            ))
                            .child(
                                icon_button(
                                    "gauges-disclosure",
                                    if expanded {
                                        Icon::ChevronDown
                                    } else {
                                        Icon::ChevronUp
                                    },
                                    if expanded {
                                        "Show fewer gauges"
                                    } else {
                                        "Show all gauges: audio, renderer, worker, control"
                                    },
                                )
                                .on_click(cx.listener(
                                    |view, _: &ClickEvent, _, _| {
                                        view.gauges_expanded = !view.gauges_expanded;
                                    },
                                )),
                            ),
                    )
                    .when(expanded, |strip| {
                        strip.child(gauges::gauge_pills(rest, pills.secondary_width, true))
                    })
                    .into_any_element()
            }
        }
    }
}

impl Render for LotusWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(controller) = &self.controller {
            controller.ui.rendered();
        }
        let plan = self.plan(window);
        let viewport_height = f32::from(window.viewport_size().height);
        let density = layout::toolbar_density(plan.main);
        let mode = layout::gauge_mode(plan.main, viewport_height, self.gauges_expanded);
        let loaded = self
            .selected_summary()
            .filter(|summary| summary.info.asset.is_some())
            .cloned();
        let gauge_height = if loaded.is_some() {
            layout::gauge_height(mode, plan.main)
        } else {
            0.0
        };
        let stage = self.render_stage(&plan, gauge_height, viewport_height, window, cx);
        let gauges = loaded.map(|summary| self.render_gauges(mode, plan.main, &summary, cx));
        let main = div()
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(plan.main.max(0.0)))
            .min_w_0()
            .h_full()
            .child(self.render_toolbar(density, cx))
            .children(self.render_error(cx))
            .child(stage)
            .children(gauges)
            .when(self.menu == Menu::Load, |main| {
                main.child(self.render_load_menu(cx))
            })
            .when(self.menu == Menu::Toolbar, |main| {
                main.child(self.render_toolbar_menu(cx))
            });
        div()
            .id("lotus")
            .relative()
            .flex()
            .size_full()
            .overflow_hidden()
            .bg(theme::background())
            .text_color(theme::text())
            .font_family(theme::FONT)
            .text_sm()
            .track_focus(&self.focus)
            .when(self.drag.is_some(), |root| root.cursor_col_resize())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _, window, cx| window.focus(&view.focus, cx)),
            )
            .on_mouse_move(cx.listener(|view, event: &MouseMoveEvent, window, cx| {
                if view.drag.is_none() {
                    return;
                }
                // Ends on button release (inside or outside the window) or
                // deactivation, not on a move without the button: under mouse
                // capture such moves can come from another input source.
                view.drag_to(event.position.x.into(), window);
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|view, _: &MouseUpEvent, _, cx| {
                    view.end_drag();
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|view, _: &MouseUpEvent, _, cx| {
                    view.end_drag();
                    cx.notify();
                }),
            )
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                if event.keystroke.modifiers.control && event.keystroke.key == "o" {
                    let session = view
                        .selected_summary()
                        .filter(|summary| summary.telemetry.state.is_active())
                        .map(|summary| summary.info.id.clone());
                    view.open_file(session, cx);
                    return;
                }
                view.key_down(event, window, cx);
            }))
            .on_key_up(cx.listener(|view, event: &KeyUpEvent, _, _| {
                view.button(&event.keystroke.key, false)
            }))
            .child(self.render_sidebar(&plan, cx))
            .child(main)
            .when_some(plan.inspector, |root, width| {
                root.child(self.render_inspector(width, cx))
            })
            .child(self.render_inspector_rail(&plan, cx))
            // At the root so it paints over the centre column it overhangs.
            .when(self.menu == Menu::Preferences, |root| {
                let popover = self.render_preferences(cx);
                root.child(match plan.sessions {
                    Some(_) => popover.bottom(px(56.0)).left(px(8.0)),
                    None => popover.bottom(px(8.0)).left(px(layout::SESSION_RAIL + 4.0)),
                })
            })
    }
}

/// Largest scale at which both screens fit `width` x `height`, at least 0.5.
#[must_use]
/// Screens of one shown session and where its touch screen was last laid out.
#[derive(Default)]
struct TileView {
    images: Vec<Arc<RenderImage>>,
    touch: Arc<Mutex<Option<TouchTransform>>>,
}

/// Replaced frames leave the sprite atlas immediately so streaming stays bounded.
fn drop_images(images: Vec<Arc<RenderImage>>, cx: &mut Context<LotusWindow>) {
    for old in images {
        cx.drop_image(old, None);
    }
}

fn tile_message(text: &'static str) -> AnyElement {
    div()
        .flex()
        .flex_1()
        .items_center()
        .justify_center()
        .text_color(theme::text_muted())
        .child(text)
        .into_any_element()
}

/// Outer size, screen arrangement and fit scale of one tile.
#[derive(Debug, Clone, Copy, PartialEq)]
struct TilePlan {
    width: f32,
    height: f32,
    screens: ScreenLayout,
    scale: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct SplitPlan {
    /// First tile above the second rather than left of it.
    vertical: bool,
    tiles: [TilePlan; 2],
}

/// An `Auto` choice changes only once the alternative is this much larger, so a resize
/// or divider drag around the crossover does not flip the layout back and forth.
const AUTO_HYSTERESIS: f32 = 0.05;

/// `candidate` replaces `held` only when its score beats `held`'s by [`AUTO_HYSTERESIS`].
fn keep_held<T>(held: Option<(T, f32)>, best: (T, f32)) -> T {
    match held {
        Some((held, score)) if score * (1.0 + AUTO_HYSTERESIS) >= best.1 => held,
        _ => best.0,
    }
}

/// Tile sizes for a split of `width` x `height` (lotus#69). `ratio` is the first tile's
/// share of the split axis. An explicit axis or screen arrangement is kept. `Auto` takes
/// the option with the larger screen scale for the smaller tile, preferring left/right and
/// stacked screens on a tie, and keeps `held` (the previous plan) unless the other option
/// is [`AUTO_HYSTERESIS`] larger. An even split shows both consoles at one common scale;
/// a moved divider gives each tile its own fit. Sizes are never negative.
fn split_plan(
    width: f32,
    height: f32,
    axis: Axis,
    ratio: f32,
    screens: [workspace::ScreenLayout; 2],
    held: Option<SplitPlan>,
) -> SplitPlan {
    let (width, height) = (width.max(0.0), height.max(0.0));
    let plan = |vertical: bool| {
        let along = (if vertical { height } else { width } - TILE_GAP).max(0.0);
        let sizes = [along * ratio, along * (1.0 - ratio)];
        let mut tiles = [0, 1].map(|index| {
            let (w, h) = if vertical {
                (width, sizes[index])
            } else {
                (sizes[index], height)
            };
            let held = held
                .filter(|held| held.vertical == vertical)
                .map(|held| held.tiles[index].screens);
            tile_plan(w, h, screens[index], held)
        });
        if !vertical {
            // Same height on both sides keeps the row aligned.
            tiles[1].height = tiles[0].height;
        }
        if (ratio - 0.5).abs() < 1e-3 {
            let common = tiles[0].scale.min(tiles[1].scale);
            for tile in &mut tiles {
                tile.scale = common;
            }
        }
        SplitPlan { vertical, tiles }
    };
    let smaller = |plan: &SplitPlan| plan.tiles[0].scale.min(plan.tiles[1].scale);
    match axis {
        Axis::Horizontal => plan(false),
        Axis::Vertical => plan(true),
        Axis::Auto => {
            let (side, stacked) = (plan(false), plan(true));
            let best = if smaller(&stacked) > smaller(&side) {
                stacked
            } else {
                side
            };
            let held = held.map(|held| {
                let again = if held.vertical { stacked } else { side };
                (again, smaller(&again))
            });
            keep_held(held, (best, smaller(&best)))
        }
    }
}

fn tile_plan(
    width: f32,
    height: f32,
    screens: workspace::ScreenLayout,
    held: Option<ScreenLayout>,
) -> TilePlan {
    let inner = (
        (width - 2.0 * SCREEN_GAP).max(0.0),
        (height - TILE_HEADER - 2.0 * SCREEN_GAP).max(0.0),
    );
    let fit = |layout| TilePlan {
        width,
        height,
        screens: layout,
        scale: fit_scale(layout, inner.0, inner.1),
    };
    match screens {
        workspace::ScreenLayout::Stacked => fit(ScreenLayout::Stacked),
        workspace::ScreenLayout::SideBySide => fit(ScreenLayout::SideBySide),
        workspace::ScreenLayout::Auto => {
            let (stacked, side) = (fit(ScreenLayout::Stacked), fit(ScreenLayout::SideBySide));
            let best = if side.scale > stacked.scale {
                side
            } else {
                stacked
            };
            let held = held.map(|layout| {
                let again = fit(layout);
                (again, again.scale)
            });
            keep_held(held, (best, best.scale))
        }
    }
}

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

fn tone_colour(tone: Tone) -> Rgba {
    match tone {
        Tone::Primary => theme::background(),
        Tone::Plain => theme::text(),
        Tone::Danger => theme::error(),
    }
}

/// Bordered (or filled, for `Primary`) control; keyboard focusable.
fn control(id: impl Into<SharedString>, tone: Tone, enabled: bool, height: f32) -> Stateful<Div> {
    let base = div()
        .id(id.into())
        .tab_index(0)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap_1()
        .h(px(height))
        .rounded(px(6.0))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .text_color(tone_colour(tone))
        .focus_visible(|style| style.border_color(theme::info()));
    let base = match tone {
        Tone::Primary => base
            .bg(theme::text())
            .border_1()
            .border_color(theme::text()),
        Tone::Plain | Tone::Danger => base.border_1().border_color(theme::border()),
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

fn button(
    id: impl Into<SharedString>,
    label: &'static str,
    tone: Tone,
    enabled: bool,
) -> Stateful<Div> {
    control(id, tone, enabled, 30.0)
        .px_3()
        .text_sm()
        .child(label)
}

/// Inspector context command: shorter than toolbar buttons.
fn small_button(
    id: impl Into<SharedString>,
    label: &'static str,
    tone: Tone,
    enabled: bool,
) -> Stateful<Div> {
    control(id, tone, enabled, 26.0)
        .px_2()
        .text_xs()
        .child(label)
}

/// Square icon control for the condensed toolbar.
fn icon_tool(id: impl Into<SharedString>, glyph: Icon) -> Stateful<Div> {
    control(id, Tone::Plain, true, 30.0)
        .min_w(px(30.0))
        .px(px(7.0))
        .child(icon(glyph, 16.0, theme::text()))
}

/// Toolbar action: labelled at full width, otherwise an icon with a tooltip.
fn tool_button(
    id: &'static str,
    label: &'static str,
    glyph: Icon,
    description: &'static str,
    labelled: bool,
    enabled: bool,
    tone: Tone,
) -> Stateful<Div> {
    if labelled {
        return button(id, label, tone, enabled);
    }
    with_tooltip(
        control(id, tone, enabled, 30.0)
            .w(px(30.0))
            .child(icon(glyph, 16.0, tone_colour(tone))),
        description,
    )
}

/// Inspector rail entry. The current view gets a surface, a stronger icon and
/// a bar on the outer edge, so it does not depend on colour alone.
fn rail_button(
    id: impl Into<SharedString>,
    glyph: Icon,
    current: bool,
    enabled: bool,
    description: impl Into<SharedString>,
) -> Stateful<Div> {
    let button = div()
        .id(id.into())
        .tab_index(0)
        .relative()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(32.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(gpui::transparent_black())
        .focus_visible(|style| style.border_color(theme::info()))
        .child(icon(
            glyph,
            16.0,
            if current {
                theme::text()
            } else {
                theme::text_secondary()
            },
        ))
        .when(current, |button| {
            button.bg(theme::surface_active()).child(
                div()
                    .absolute()
                    .left(px(-5.0))
                    .top(px(7.0))
                    .h(px(16.0))
                    .w(px(3.0))
                    .rounded(px(1.0))
                    .bg(theme::text()),
            )
        })
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_hover()))
        })
        .when(!enabled, |button| button.opacity(0.4));
    with_tooltip(button, description)
}

/// Small text tooltip; GPUI tooltips are views.
struct Tooltip(SharedString);

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(360.0))
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

pub(crate) fn with_tooltip(element: Stateful<Div>, text: impl Into<SharedString>) -> Stateful<Div> {
    let text = text.into();
    element.tooltip(move |_, cx| cx.new(|_| Tooltip(text.clone())).into())
}

fn icon_button(
    id: impl Into<SharedString>,
    glyph: Icon,
    description: &'static str,
) -> Stateful<Div> {
    let button = div()
        .id(id.into())
        .tab_index(0)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(24.0))
        .rounded(px(5.0))
        .border_1()
        .border_color(gpui::transparent_black())
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_active()))
        .focus_visible(|style| style.border_color(theme::info()))
        .child(icon(glyph, 14.0, theme::text_secondary()));
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
        .tab_index(0)
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .h(px(28.0))
        .px_2()
        .rounded(px(5.0))
        .border_1()
        .border_color(gpui::transparent_black())
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_active()))
        .focus_visible(|style| style.border_color(theme::info()))
        .child(div().truncate().child(label.into()))
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(theme::text_muted())
                .child(detail.into()),
        )
}

/// Menu row with a check box drawn as a shape, so state is not colour alone.
fn checkbox_item(
    id: impl Into<SharedString>,
    label: &'static str,
    checked: bool,
    enabled: bool,
) -> Stateful<Div> {
    let mut item = div()
        .id(id.into())
        .tab_index(0)
        .flex()
        .items_center()
        .gap_2()
        .h(px(28.0))
        .px_2()
        .rounded(px(5.0))
        .border_1()
        .border_color(gpui::transparent_black())
        .focus_visible(|style| style.border_color(theme::info()))
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .size(px(14.0))
                .rounded(px(3.0))
                .border_1()
                .border_color(theme::border_strong())
                .when(checked, |box_| {
                    box_.bg(theme::text())
                        .child(div().size(px(6.0)).rounded(px(1.0)).bg(theme::background()))
                }),
        )
        .child(div().flex_1().truncate().child(label));
    if enabled {
        item = item
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_active()));
    } else {
        item = item.opacity(0.4);
    }
    item
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
    fn split_auto_picks_the_axis_with_larger_screens_and_keeps_explicit_choices() {
        let auto = [workspace::ScreenLayout::Auto; 2];
        let wide = split_plan(1800.0, 700.0, Axis::Auto, 0.5, auto, None);
        assert!(!wide.vertical, "{wide:?}");
        let tall = split_plan(700.0, 1600.0, Axis::Auto, 0.5, auto, None);
        assert!(tall.vertical, "{tall:?}");
        // An explicit axis is kept even where the other would be larger.
        assert!(!split_plan(700.0, 1600.0, Axis::Horizontal, 0.5, auto, None).vertical);
        let stacked = [workspace::ScreenLayout::Stacked; 2];
        let held = split_plan(1800.0, 300.0, Axis::Vertical, 0.5, stacked, None);
        assert!(held.vertical);
        assert!(held
            .tiles
            .iter()
            .all(|t| t.screens == ScreenLayout::Stacked));
        // The ratio splits the axis less the gap; sizes never go negative.
        let off = split_plan(1200.0, 700.0, Axis::Horizontal, 0.7, auto, None);
        let total = off.tiles[0].width + off.tiles[1].width + TILE_GAP;
        assert!((total - 1200.0).abs() < 1e-3, "{off:?}");
        assert!(off.tiles[0].width > off.tiles[1].width);
        assert!(off.tiles[0].scale >= off.tiles[1].scale);
        let tiny = split_plan(10.0, 10.0, Axis::Auto, 0.5, auto, None);
        assert!(tiny
            .tiles
            .iter()
            .all(|t| t.scale >= 0.5 && t.scale.is_finite()));
    }

    /// Content area for window sizes in logical pixels, after the 2 x 24 content padding
    /// and the 52 px toolbar, with panes collapsed and no gauge strip. GPUI lays out in
    /// logical pixels, so 3840 x 2160 at 200% is planned as 1920 x 1080.
    const WINDOWS: [(f32, f32); 6] = [
        (720.0 - 48.0, 560.0 - 52.0 - 48.0), // minimum window (main.rs)
        (900.0 - 48.0, 1500.0 - 100.0),      // portrait
        (1600.0 - 48.0, 800.0 - 100.0),      // landscape
        (1920.0 - 48.0, 1080.0 - 100.0),     // 4K at 200%
        (3440.0 - 48.0, 1440.0 - 100.0),     // ultrawide
        (2560.0 - 48.0, 1080.0 - 100.0),     // 21:9
    ];

    const FIXED: [workspace::ScreenLayout; 2] = [
        workspace::ScreenLayout::Stacked,
        workspace::ScreenLayout::SideBySide,
    ];

    fn screens_size(layout: ScreenLayout, scale: f32) -> (f32, f32) {
        match layout {
            ScreenLayout::Stacked => (400.0 * scale, 480.0 * scale + SCREEN_GAP),
            ScreenLayout::SideBySide => (720.0 * scale + SCREEN_GAP, 240.0 * scale),
        }
    }

    fn arrangement(plan: &SplitPlan) -> (bool, [ScreenLayout; 2]) {
        (plan.vertical, plan.tiles.map(|tile| tile.screens))
    }

    #[test]
    fn split_bounds_never_overlap_and_screens_fit_their_tiles() {
        let layouts = [[workspace::ScreenLayout::Auto; 2], [FIXED[1], FIXED[0]]];
        for (width, height) in WINDOWS {
            for axis in [Axis::Auto, Axis::Horizontal, Axis::Vertical] {
                for ratio in [0.2, 0.35, 0.5, 0.65, 0.8] {
                    for screens in layouts {
                        let plan = split_plan(width, height, axis, ratio, screens, None);
                        let case = format!("{width}x{height} {axis:?} {ratio} {plan:?}");
                        let along: f32 = if plan.vertical {
                            plan.tiles.iter().map(|t| t.height).sum()
                        } else {
                            plan.tiles.iter().map(|t| t.width).sum()
                        };
                        let room = if plan.vertical { height } else { width };
                        assert!(along + TILE_GAP <= room + 1e-3, "{case}");
                        for tile in plan.tiles {
                            assert!(tile.width >= 0.0 && tile.height >= 0.0, "{case}");
                            assert!(tile.scale.is_finite() && tile.scale >= 0.5, "{case}");
                            // Above the 0.5 floor, both screens fit inside the tile body.
                            if tile.scale > 0.5 {
                                let (w, h) = screens_size(tile.screens, tile.scale);
                                let body = tile.height - TILE_HEADER - 2.0 * SCREEN_GAP;
                                assert!(w <= tile.width - 2.0 * SCREEN_GAP + 1e-3, "{case}");
                                assert!(h <= body + 1e-3, "{case}");
                            }
                        }
                        if (ratio - 0.5).abs() < 1e-3 {
                            assert_eq!(plan.tiles[0].scale, plan.tiles[1].scale, "{case}");
                        }
                    }
                }
            }
        }
        let (width, height) = WINDOWS[0];
        let auto = [workspace::ScreenLayout::Auto; 2];
        let minimum = split_plan(width, height, Axis::Auto, 0.5, auto, None);
        assert!(minimum.tiles[0].scale > 0.5, "minimum window: {minimum:?}");
    }

    #[test]
    fn auto_never_picks_a_smaller_layout_than_the_alternatives() {
        let auto = [workspace::ScreenLayout::Auto; 2];
        for (width, height) in WINDOWS {
            let chosen = split_plan(width, height, Axis::Auto, 0.5, auto, None);
            for axis in [Axis::Horizontal, Axis::Vertical] {
                for first in FIXED {
                    for second in FIXED {
                        let other = split_plan(width, height, axis, 0.5, [first, second], None);
                        assert!(
                            chosen.tiles[0].scale >= other.tiles[0].scale - 1e-4,
                            "{width}x{height}: {chosen:?} vs {other:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn auto_does_not_oscillate_around_the_crossover() {
        let auto = [workspace::ScreenLayout::Auto; 2];
        let height = 900.0;
        let sweep = |widths: Vec<f32>, start: Option<SplitPlan>| {
            let mut held = start;
            let mut flips = Vec::new();
            for width in widths {
                let plan = split_plan(width, height, Axis::Auto, 0.5, auto, held);
                if held.is_some_and(|held| arrangement(&held) != arrangement(&plan)) {
                    flips.push(width);
                }
                held = Some(plan);
            }
            (flips, held)
        };
        let (rising, end) = sweep((700..=2600).map(|w| w as f32).collect(), None);
        let (falling, _) = sweep((700..=2600).rev().map(|w| w as f32).collect(), end);
        assert!(!rising.is_empty(), "the sweep crosses a threshold");
        assert_eq!(rising.len(), falling.len(), "{rising:?} {falling:?}");
        // Above the 0.5 floor (both options tie there and the held one stays), each
        // crossover flips once per direction, later going up than coming down.
        for (up, down) in rising.iter().zip(falling.iter().rev()) {
            assert!(up > down, "no hysteresis band: {rising:?} {falling:?}");
        }
        // Jitter of a few pixels around a crossover never flips.
        let jitter = (0..200).map(|step| rising[0] + if step % 2 == 0 { 3.0 } else { -3.0 });
        let (flips, _) = sweep(jitter.collect(), None);
        assert!(flips.len() <= 1, "{flips:?}");
        // An explicit axis ignores what Auto held.
        let held = split_plan(2600.0, height, Axis::Auto, 0.5, auto, None);
        for (axis, vertical) in [(Axis::Vertical, true), (Axis::Horizontal, false)] {
            let forced = split_plan(2600.0, height, axis, 0.5, auto, Some(held));
            assert_eq!(forced.vertical, vertical);
        }
    }

    #[test]
    fn registers_render_as_fixed_width_hex() {
        assert_eq!(hex_string(&serde_json::json!(0x1f)), "0000001f");
        assert_eq!(hex_string(&Value::Null), "—");
    }
}
