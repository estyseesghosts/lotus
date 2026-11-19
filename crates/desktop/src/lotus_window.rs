//! The LOTUS window.
//!
//! Layout, left to right: the session sidebar (or its chip rail), the centre
//! column (toolbar, screens, gauges), inspector and rail. The opt-in shared shell
//! adds overlay/pin presentation and a width-dependent rail in single/split views.
//! The legacy renderer retains its docked single-view inspector. Panes resize
//! from dividers
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
    div, px, relative, AnyElement, AppContext, ClickEvent, Context, Div, DragMoveEvent,
    FocusHandle, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, KeyUpEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, PathPromptOptions,
    Render, RenderImage, Rgba, SharedString, Stateful, StatefulInteractiveElement, Styled,
    StyledImage, Window,
};
use image::{Frame, RgbaImage};
use lotus_session::supervisor::{HostConfig, SessionKind, SessionSummary};
use lotus_session::workspace::{self, Axis, Layout, Operation, Snapshot, Viewport};
use lotus_session::SessionState;
use serde_json::Value;

use crate::desktop_controller::{Command, DesktopController, McpLink, McpMode};
use crate::gauges;
use crate::icons::{icon, Icon};
use crate::inspection::{self, InspectionSnapshot, Panel, SessionTarget};
use crate::layout::{self, Drag, Pane, PaneState, Plan};
#[cfg(not(feature = "silvestris-ui"))]
use crate::layout::{GaugeMode, ToolbarDensity};
use crate::preferences::{self, Store};
use crate::screen_geometry::TouchTransform;
use crate::session_tree;
#[cfg(not(feature = "silvestris-ui"))]
use crate::session_tree::TreeNode;
use crate::theme;
use crate::tiling::{self, DividerDrag, SessionDrag, Source, Target};
use lotus_protocol::debug::{DebugCommand as D, GpuCapture};

#[cfg(feature = "silvestris-ui")]
mod shared_commands;
#[cfg(feature = "silvestris-ui")]
mod shared_navigation;
#[cfg(feature = "silvestris-ui")]
mod shared_shell;

const TOOLBAR_HEIGHT: f32 = 52.0;
// Silvestris measures its content below the frame's unified toolbar.
fn workspace_toolbar_height() -> f32 {
    if cfg!(feature = "silvestris-ui") {
        0.0
    } else {
        TOOLBAR_HEIGHT
    }
}
const SCREEN_GAP: f32 = 12.0;
const CONTENT_PADDING: f32 = 24.0;
/// Session name, controller and layout controls above each tile of a split.
const TILE_HEADER: f32 = 28.0;
/// Space between the two tiles of a split.
const TILE_GAP: f32 = 12.0;
/// Width of a tile's gauge column on the outer edge of a left/right split (lotus#70).
const GAUGE_COLUMN: f32 = 112.0;
/// Height of a tile's gauge row on the outer edge of a top/bottom split (lotus#70).
const GAUGE_ROW: f32 = layout::PILL_HEIGHT + 2.0 * layout::PILL_GAP;
/// Space between a tile and its gauges.
const GAUGE_GAP: f32 = 6.0;
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
    #[cfg(not(feature = "silvestris-ui"))]
    NewSession,
    #[cfg(not(feature = "silvestris-ui"))]
    Load,
    /// Toolbar actions that do not fit.
    #[cfg(not(feature = "silvestris-ui"))]
    Toolbar,
    /// Inspector views not on the rail (legacy UI; Silvestris uses a pop-up).
    #[cfg(not(feature = "silvestris-ui"))]
    InspectorMore,
    /// Trace import, clipboard command, keyboard reference.
    #[cfg(not(feature = "silvestris-ui"))]
    InspectorUtility,
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

#[cfg(not(feature = "silvestris-ui"))]
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
    #[cfg(feature = "silvestris-ui")]
    shared_plan: Cell<Option<silvestris::core::layout::ShellPlan>>,
    #[cfg(feature = "silvestris-ui")]
    preference_save: Option<gpui::Task<()>>,
    #[cfg(feature = "silvestris-ui")]
    shared_sources: gpui::Entity<silvestris::controls::SourceList>,
    #[cfg(feature = "silvestris-ui")]
    shared_commands: shared_commands::SharedCommands,
    #[cfg(feature = "silvestris-ui")]
    navigation: crate::navigation::Navigation,
    #[cfg(feature = "silvestris-ui")]
    source_branches: std::collections::BTreeSet<String>,
    #[cfg(feature = "silvestris-ui")]
    source_context: (u64, bool, String),
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
    /// Session the inspector targets, as published by the controller (lotus#71).
    inspected: String,
    /// Tile session whose floating inspector is open in a split; `None` when closed.
    inspect_tile: Option<String>,
    /// Width of that floating inspector. Not saved: the single-view inspector width in
    /// preferences is never changed by a split.
    overlay_width: f32,
    overlay_drag: bool,
    /// Where the session being dragged would land, with its snap preview as fractions of
    /// the stage (lotus#68). Cleared whenever no drag is active.
    drop_target: Option<(Target, (f32, f32, f32, f32))>,
    /// Split ratio while the divider is dragged; committed on release.
    ratio_drag: Option<f32>,
    /// Last split plan, so `Auto` holds its choice near a crossover (lotus#69).
    split_held: Cell<Option<SplitPlan>>,
    /// Last resolved layout handed to the supervisor, to report only changes.
    resolved_sent: Cell<Option<workspace::Resolved>>,
    registers: Option<(SessionTarget, Value)>,
    inspection: Option<InspectionSnapshot>,
    inspection_panel: Panel,
    surface_image: Option<Arc<RenderImage>>,
    surface_target: Option<SessionTarget>,
    error: String,
    dismissed_error: String,
    mcp: McpLink,
    pending: usize,
    waiting: Vec<String>,
    artifacts: Option<lotus_session::artifacts::Usage>,
    /// Connected controllers and their assignments (lotus#62), refreshed with each update.
    controllers: crate::input_routing::Controllers,
    focus: FocusHandle,
    /// Keyboard keys held (GPUI names), for [`Self::keyboard_target`], and pointer touch,
    /// for the tile it started on; the router maps keys through that session's input layout
    /// (lotus#63) and composes them with its controllers (lotus#61).
    keys: std::collections::BTreeSet<String>,
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
    #[cfg(all(test, feature = "silvestris-ui"))]
    pub(crate) fn test_preview(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut view = Self::new(None, McpMode::Off, window, cx);
        view.preferences = Store::load(None);
        view.inspection_panel = Panel::Cpu;
        view.recent = Pane::Sessions;
        view
    }
    pub fn new(
        config: Option<HostConfig>,
        mcp: McpMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        // The Settings window belongs to this one; it never outlives it (lotus#53).
        #[cfg(feature = "silvestris-ui")]
        let close_owner = cx.weak_entity();
        window.on_window_should_close(cx, move |_, cx| {
            #[cfg(feature = "silvestris-ui")]
            let _ = close_owner.read_with(cx, |view, _| view.preferences.save());
            crate::settings_window::close(cx);
            true
        });
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
        cx.observe_window_visibility(window, |view, visibility, _, _| {
            if let Some(controller) = &view.controller {
                controller.set_preview_visible(visibility.is_visible());
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
        #[allow(unused_mut)]
        let mut preferences = Store::load(preferences::default_path());
        #[cfg(feature = "silvestris-ui")]
        {
            use silvestris::ActiveTheme;
            let active = cx.theme().id;
            preferences.ui.theme_family = active.family;
            preferences.ui.appearance = active.appearance;
        }
        if let Some(controller) = &controller {
            controller.set_preview_visible(window.is_visible());
            #[cfg(feature = "silvestris-ui")]
            {
                *controller.inspection_selection.lock().unwrap() = (
                    Panel::from_id(&preferences.ui.inspector_panel).unwrap_or(Panel::Cpu),
                    0,
                );
            }
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
        #[cfg(feature = "silvestris-ui")]
        let shared_sources = Self::make_shared_sources(cx);
        #[cfg(feature = "silvestris-ui")]
        let shared_commands = shared_commands::SharedCommands::new(window, cx);
        Self {
            #[cfg(feature = "silvestris-ui")]
            shared_plan: Cell::new(None),
            #[cfg(feature = "silvestris-ui")]
            preference_save: None,
            #[cfg(feature = "silvestris-ui")]
            shared_sources,
            #[cfg(feature = "silvestris-ui")]
            shared_commands,
            #[cfg(feature = "silvestris-ui")]
            navigation: crate::navigation::Navigation::default(),
            #[cfg(feature = "silvestris-ui")]
            source_branches: std::collections::BTreeSet::new(),
            #[cfg(feature = "silvestris-ui")]
            source_context: (0, false, String::new()),
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
            inspected: String::new(),
            inspect_tile: None,
            overlay_width: layout::INSPECTOR_DEFAULT,
            overlay_drag: false,
            drop_target: None,
            ratio_drag: None,
            split_held: Cell::new(None),
            resolved_sent: Cell::new(None),
            registers: None,
            inspection: None,
            inspection_panel: {
                #[cfg(feature = "silvestris-ui")]
                {
                    Panel::from_id(&preferences.ui.inspector_panel).unwrap_or(Panel::Cpu)
                }
                #[cfg(not(feature = "silvestris-ui"))]
                {
                    Panel::Cpu
                }
            },
            surface_image: None,
            surface_target: None,
            error: String::new(),
            dismissed_error: String::new(),
            mcp: McpLink::Off,
            pending: 0,
            waiting: Vec::new(),
            artifacts: None,
            controllers: crate::input_routing::Controllers::default(),
            focus,
            keys: std::collections::BTreeSet::new(),
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
        #[cfg(feature = "silvestris-ui")]
        if pane == Pane::Inspector {
            if state.open {
                self.release_input();
            }
            if !state.open && self.inspect_tile.take().is_some() {
                self.send(Command::Inspect(None));
            }
        }
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
        #[cfg(feature = "silvestris-ui")]
        {
            self.shared_layout(window)
        }
        #[cfg(not(feature = "silvestris-ui"))]
        {
            // A split has no inspector rail; each tile opens its own floating inspector.
            let inspector = (!self.split()).then(|| self.pane(Pane::Inspector));
            layout::plan_with(
                f32::from(window.viewport_size().width),
                self.pane(Pane::Sessions),
                inspector,
                self.recent,
            )
        }
    }

    fn split(&self) -> bool {
        matches!(self.workspace.layout, Layout::Split { .. })
    }

    /// Open (or, if already open for it, close) the floating inspector of a tile's
    /// session. Never selects it, so audio focus and the gauges' selection stay put.
    fn toggle_tile_inspector(&mut self, session: String) {
        let same = self.inspect_tile.as_deref() == Some(session.as_str())
            && (!cfg!(feature = "silvestris-ui") || self.preferences.values.inspector_open);
        let next = (!same).then_some(session);
        self.inspect_tile.clone_from(&next);
        self.send(Command::Inspect(next));
        #[cfg(feature = "silvestris-ui")]
        {
            self.set_pane(
                Pane::Inspector,
                PaneState {
                    open: self.inspect_tile.is_some(),
                    ..self.pane(Pane::Inspector)
                },
            );
            self.preferences.save();
        }
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

    /// Toggle the saved preference in Silvestris, including when space hides a pane.
    fn toggle_pane(&mut self, pane: Pane, _window: &Window) {
        #[cfg(feature = "silvestris-ui")]
        let shown = self.pane(pane).open;
        #[cfg(not(feature = "silvestris-ui"))]
        let shown = {
            let plan = self.plan(_window);
            match pane {
                Pane::Sessions => plan.sessions.is_some(),
                Pane::Inspector => plan.inspector.is_some(),
            }
        };
        if shown {
            self.close_pane(pane);
        } else {
            self.open_pane(pane);
        }
    }

    /// Resize the floating inspector from its inner edge; never saved.
    fn drag_overlay(&mut self, x: f32, window: &Window) {
        let plan = self.plan(window);
        let viewport = f32::from(window.viewport_size().width);
        let width = if self.overlay_left() {
            x - plan.sessions_width()
        } else {
            viewport - x
        };
        let room = (plan.main - layout::INSPECTOR_MIN).max(layout::INSPECTOR_MIN);
        self.overlay_width = width.clamp(layout::INSPECTOR_MIN, layout::INSPECTOR_MAX.min(room));
    }

    /// The floating inspector covers the side of its own tile: left for the first tile of
    /// a left/right split, right otherwise.
    fn overlay_left(&self) -> bool {
        match &self.workspace.layout {
            Layout::Split { first, .. } => {
                self.inspect_tile.as_deref() == Some(first.as_str())
                    && self.split_held.get().is_some_and(|plan| !plan.vertical)
            }
            _ => false,
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
        self.overlay_drag = false;
        if self.drag.take().is_some() {
            self.preferences.save();
        }
    }

    /// A pane edge that resizes on drag. Lives inside the pane so the centre
    /// column's hitboxes (including the touch screen) never overlap it.
    fn divider(&self, pane: Pane, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::palette(cx);
        let active = self.drag.is_some_and(|drag| drag.pane == pane);
        let line = div()
            .absolute()
            .top_0()
            .bottom_0()
            .w(px(if active { 2.0 } else { 1.0 }))
            .when(pane == Pane::Sessions, |line| line.right_0())
            .when(pane == Pane::Inspector, |line| line.left_0())
            .bg(if active { theme.info } else { theme.border });
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
            .hover(|style| style.bg(theme.surface_active))
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
        let latest = controller.latest.clone();
        let preview_visible = controller.preview_visible();
        ui.poll_started();
        let controllers = controller.input().controllers();
        if controllers != self.controllers {
            self.controllers = controllers;
        }
        let target = self.keyboard_target();
        let (previews, selected, surface) = {
            let mut update = ui.lock_update(&latest);
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
            self.set_inspection(update.inspection.clone(), cx);
            self.workspace.clone_from(&update.workspace);
            if update.inspected != self.inspected {
                self.inspected.clone_from(&update.inspected);
                if let Some(old) = self.surface_image.take() {
                    cx.drop_image(old, None);
                }
            }
            // The controller clears the tile once it leaves the split or stops; a request
            // still in the queue keeps the local value until it is applied.
            if update.inspect_tile.is_none() && self.inspect_tile.is_some() {
                let requested = self.inspect_tile.as_ref();
                let shown = self.workspace.layout.visible();
                if !matches!(self.workspace.layout, Layout::Split { .. })
                    || !requested.is_some_and(|tile| shown.contains(&tile.as_str()))
                {
                    self.inspect_tile = None;
                }
            }
            if let Some(controller) = &self.controller {
                let open = if cfg!(feature = "silvestris-ui") {
                    self.preferences.values.inspector_open
                } else if matches!(self.workspace.layout, Layout::Split { .. }) {
                    self.inspect_tile.is_some()
                } else {
                    self.preferences.values.inspector_open
                };
                controller.inspector.store(open, Ordering::Release);
            }
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
            // Let any in-flight preview finish, but defer image creation until it can be
            // shown. A paused worker may have sent its final frame just before hiding.
            let previews = if preview_visible {
                std::mem::take(&mut update.previews)
            } else {
                BTreeMap::new()
            };
            (previews, update.selected.clone(), update.debug_image.take())
        };
        if selected != self.selected {
            self.selected = selected;
        }
        // Keyboard follows the selection outside a split and the focused tile inside one;
        // what it held is released when it moves.
        if self.keyboard_target() != target {
            self.release_input();
        }
        self.retain_current_inspection(cx);
        if let Some(surface) =
            surface.filter(|surface| surface.target.is_current(&self.inspected, &self.sessions))
        {
            if let Some(old) = self.surface_image.take() {
                cx.drop_image(old, None);
            }
            if let Some(pixels) = RgbaImage::from_raw(surface.width, surface.height, surface.bgra) {
                self.surface_target = Some(surface.target);
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
        for summary in &self.sessions {
            let Some(tile) = self.tiles.get_mut(&summary.info.id) else {
                continue;
            };
            if summary.telemetry.state == SessionState::Crashed
                || tile.epoch < summary.telemetry.epoch
            {
                drop_images(std::mem::take(&mut tile.images), cx);
                tile.epoch = summary.telemetry.epoch;
            }
        }
        for (session, preview) in previews {
            let Some(tile) = self.tiles.get_mut(&session) else {
                continue;
            };
            let Some(frame) = preview.frames.first() else {
                continue;
            };
            let epoch = frame.lease.epoch;
            if epoch < tile.epoch
                || self.sessions.iter().any(|summary| {
                    summary.info.id == session && summary.telemetry.state == SessionState::Crashed
                })
            {
                continue;
            }
            tile.epoch = epoch;
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
        #[cfg(feature = "silvestris-ui")]
        self.sync_shared_sources(cx);
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

    /// The session keyboard input drives: the focused tile of a split or the single view,
    /// which an MCP `single` may set apart from the selection (#112); the selection only
    /// while nothing is shown. Selecting in the sidebar never moves it off a tile.
    fn keyboard_target(&self) -> Option<String> {
        let id = self
            .workspace
            .focused_session()
            .unwrap_or(self.selected.as_str());
        self.sessions
            .iter()
            .any(|s| s.info.id == id)
            .then(|| id.to_string())
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
        input.keyboard_keys(keyboard.as_deref(), self.keys.clone());
        input.pointer(pointer.as_deref(), self.touching);
    }

    fn release_input(&mut self) {
        self.keys.clear();
        self.touching = None;
        self.publish_input();
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = &event.keystroke.modifiers;
        let key = event.keystroke.key.as_str();
        if modifiers.control && modifiers.shift && self.split() && self.split_shortcut(key) {
            return;
        }
        if modifiers.control {
            match key {
                "b" => self.toggle_pane(Pane::Sessions, window),
                "i" if self.split() && !cfg!(feature = "silvestris-ui") => {
                    if let Some(session) = self.keyboard_target() {
                        self.toggle_tile_inspector(session);
                    }
                }
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
                // Cancels a drag-and-drop or divider drag without committing it.
                if cx.stop_active_drag(window) {
                    self.drop_target = None;
                    self.ratio_drag = None;
                    cx.notify();
                }
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
        // Keys handled here are `lotus_input::layout::RESERVED_KEYS`, which bindings may
        // not use; a held repeat of one must not reach the session either.
        match key {
            "space" if !event.is_held => self.toggle_run(),
            "." if !event.is_held => self.step(),
            key if lotus_input::layout::reserved_key(key).is_some() => {}
            key => self.button(key, true),
        }
    }

    /// A game key went down or up. Which 3DS input it means is the target session's layout.
    fn button(&mut self, key: &str, down: bool) {
        let changed = if down {
            self.keys.insert(key.to_string())
        } else {
            self.keys.remove(key)
        };
        if changed {
            self.publish_input();
        }
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
        let theme = theme::palette(cx);
        let Some(width) = plan.sessions else {
            #[cfg(not(feature = "silvestris-ui"))]
            return self.render_rail(plan, cx);
            #[cfg(feature = "silvestris-ui")]
            return gpui::Empty.into_any_element();
        };
        #[cfg(feature = "silvestris-ui")]
        let list = div()
            .flex_1()
            .min_h_0()
            .child(self.shared_sources.clone())
            .into_any_element();
        #[cfg(not(feature = "silvestris-ui"))]
        let list = {
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
                        .text_color(theme.text_muted)
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
            list.into_any_element()
        };
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(width))
            .h_full()
            .bg(theme.panel)
            .when(!cfg!(feature = "silvestris-ui"), |sidebar| {
                sidebar.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .h(px(TOOLBAR_HEIGHT))
                        .px_4()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(div().size(px(10.0)).rounded(px(2.0)).bg(theme.text))
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("LOTUS"),
                                ),
                        )
                        .child(
                            icon_button(
                                theme,
                                "collapse",
                                Icon::ChevronLeft,
                                "Collapse sidebar (Ctrl+B)",
                            )
                            .on_click(cx.listener(
                                |view, _: &ClickEvent, _, _| {
                                    view.close_pane(Pane::Sessions);
                                },
                            )),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .pl_4()
                    .pr_2()
                    .pt_3()
                    .pb_1()
                    .child(section_label(theme, "SESSIONS"))
                    .when(self.control, |row| {
                        row.child(
                            icon_button(theme, "new-session", Icon::Plus, "New session").on_click(
                                cx.listener(|view, event: &ClickEvent, window, cx| {
                                    #[cfg(feature = "silvestris-ui")]
                                    view.open_new_session_menu(event, window, cx);
                                    #[cfg(not(feature = "silvestris-ui"))]
                                    {
                                        let _ = (event, window, cx);
                                        view.menu = match view.menu {
                                            Menu::NewSession => Menu::Closed,
                                            _ => Menu::NewSession,
                                        };
                                    }
                                }),
                            ),
                        )
                    }),
            )
            .child(list)
            .child(self.render_controllers(cx))
            .when(!cfg!(feature = "silvestris-ui"), |sidebar| {
                sidebar.child(self.render_sidebar_footer(cx))
            })
            .when(!cfg!(feature = "silvestris-ui"), |sidebar| {
                sidebar.child(self.divider(Pane::Sessions, cx))
            })
            .children({
                #[cfg(feature = "silvestris-ui")]
                let menu: Option<gpui::Empty> = None;
                #[cfg(not(feature = "silvestris-ui"))]
                let menu = (self.menu == Menu::NewSession).then(|| self.render_new_menu(cx));
                menu
            })
            .into_any_element()
    }

    #[cfg(not(feature = "silvestris-ui"))]
    fn render_rail(&self, plan: &Plan, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme::palette(cx);
        let squeezed = plan.squeezed == Some(Pane::Sessions);
        let mut rail = div()
            .relative()
            .flex()
            .flex_col()
            .items_center()
            .flex_none()
            .w(px(layout::SESSION_RAIL))
            .h_full()
            .bg(theme.panel)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(TOOLBAR_HEIGHT))
                    .w_full()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        icon_button(
                            theme,
                            "expand",
                            Icon::ChevronRight,
                            if squeezed {
                                "Expand sidebar (Ctrl+B); resizing the window may be needed"
                            } else {
                                "Expand sidebar (Ctrl+B)"
                            },
                        )
                        .on_click(cx.listener(
                            |view, _: &ClickEvent, _, _| {
                                view.open_pane(Pane::Sessions);
                            },
                        )),
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
                            theme.border_strong
                        } else {
                            theme.border
                        })
                        .when(selected, |chip| chip.bg(theme.surface_active))
                        .when(depth == Depth::Nested, |chip| chip.border_dashed())
                        .hover(|style| style.bg(theme.surface_hover))
                        .focus_visible(|style| style.border_color(theme.info))
                        .cursor_pointer()
                        .text_xs()
                        .text_color(theme.text_secondary)
                        .child(initial.to_string())
                        .child(
                            div()
                                .absolute()
                                .top(px(-2.0))
                                .right(px(-2.0))
                                .size(px(7.0))
                                .rounded_full()
                                .bg(state_colour(theme, &summary)),
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
                    .border_color(theme.border)
                    .child(self.preferences_button(cx)),
            )
            .child(self.divider(Pane::Sessions, cx));
        rail.into_any_element()
    }

    fn preferences_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::palette(cx);
        icon_button(theme, "preferences", Icon::Gear, "Settings (Ctrl+,)").on_click(cx.listener(
            |view, _: &ClickEvent, _, cx| {
                view.open_settings(crate::settings_window::Target::Global, cx)
            },
        ))
    }

    /// Open (or focus) the one Settings window on `target` (lotus#53).
    fn open_settings(&mut self, target: crate::settings_window::Target, cx: &mut Context<Self>) {
        self.menu = Menu::Closed;
        let host = self.controller.as_ref().map(|c| c.host().clone());
        let lotus = cx.weak_entity();
        let control = self.control;
        cx.defer(move |cx| crate::settings_window::open(cx, lotus, host, control, target));
    }

    /// A `lotus.desktop.*` value from `desktop.json` (default when unset or unknown).
    pub(crate) fn preference(&self, key: &str) -> serde_json::Value {
        let field = key.strip_prefix("lotus.desktop.").unwrap_or(key);
        serde_json::to_value(&self.preferences.values)
            .ok()
            .and_then(|values| values.get(field).cloned())
            .unwrap_or_default()
    }

    /// Change one `lotus.desktop.*` value after checking it against the registry, save
    /// `desktop.json` and apply it. Rich Presence stays off while the application ID is
    /// unusable, as the popover it replaces did.
    pub(crate) fn set_preference(
        &mut self,
        key: &str,
        value: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let Some(descriptor) = lotus_settings::Registry::builtin().get(key) else {
            return;
        };
        if descriptor.validate(&value).is_err() {
            return;
        }
        if key == "lotus.desktop.discord_presence"
            && value == serde_json::Value::Bool(true)
            && crate::presence::application_id(&self.preferences.values).is_none()
        {
            return;
        }
        let field = key.strip_prefix("lotus.desktop.").unwrap_or(key);
        let Ok(serde_json::Value::Object(mut values)) =
            serde_json::to_value(&self.preferences.values)
        else {
            return;
        };
        values.insert(field.to_string(), value);
        let Ok(updated) = serde_json::from_value(serde_json::Value::Object(values)) else {
            return;
        };
        self.preferences.values = updated;
        #[cfg(feature = "silvestris-ui")]
        if key.starts_with("lotus.desktop.inspector_") {
            self.set_pane(Pane::Inspector, self.pane(Pane::Inspector));
        }
        self.preferences.save();
        self.publish_presence();
        cx.notify();
    }

    /// Rich Presence connection state for the Settings window: colour and text.
    pub(crate) fn presence_line(&self, theme: theme::Palette) -> Option<(Rgba, String)> {
        if !self.preferences.values.discord_presence {
            return None;
        }
        let status = self
            .controller
            .as_ref()
            .map(|controller| controller.presence.status())
            .unwrap_or_default();
        Some(match status {
            crate::presence::Status::Off => (theme.text_muted, "Starting…".to_string()),
            crate::presence::Status::Waiting(_) => {
                (theme.warning, "Discord not reachable; retrying".to_string())
            }
            crate::presence::Status::Shown(None) => {
                (theme.success, "Connected · nothing loaded".to_string())
            }
            crate::presence::Status::Shown(Some(activity)) => {
                (theme.success, format!("Connected · {}", activity.details))
            }
        })
    }

    /// Whether `desktop.json` is saved (a location exists and the file was readable).
    pub(crate) fn preferences_saved(&self) -> bool {
        self.preferences.path.is_some()
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

    /// Split orientation and ratio as drawn, for drop zones and the divider (lotus#68).
    fn drawn_split(&self) -> (bool, f32) {
        let vertical = self.split_held.get().is_some_and(|plan| plan.vertical);
        let ratio = match &self.workspace.layout {
            Layout::Split { ratio, .. } => self.ratio_drag.unwrap_or(*ratio),
            _ => workspace::RATIO_DEFAULT,
        };
        (vertical, ratio)
    }

    fn session_drag(&self, session: &str, source: Source) -> SessionDrag {
        SessionDrag {
            session: session.to_string(),
            label: format!(
                "{} {}",
                self.session_name(session),
                session_tree::short_id(session)
            ),
            source,
            revision: self.workspace.revision,
        }
    }

    /// Commit a dropped session at the target its preview showed. The operation carries
    /// the revision the drag started on, so a layout changed meanwhile refuses it.
    fn drop_session(&mut self, drag: &SessionDrag) {
        let Some((target, _)) = self.drop_target.take() else {
            return;
        };
        // Ended while it was being dragged.
        if !self.sessions.iter().any(|s| s.info.id == drag.session) {
            return;
        }
        let (vertical, _) = self.drawn_split();
        let Some(operation) = tiling::operation(drag, target, &self.workspace.layout, vertical)
        else {
            return;
        };
        // Outside a split the window shows the selection; select what stays.
        match &operation {
            Operation::Untile {
                keep: Some(session),
            }
            | Operation::Single { session } => self.select(session.clone()),
            _ => {}
        }
        self.send(Command::WorkspaceAt(operation, drag.revision));
    }

    /// Track a session drag over the stage: the target under the pointer and its preview.
    fn drag_over_stage(&mut self, event: &DragMoveEvent<SessionDrag>, cx: &mut Context<Self>) {
        let (bounds, at) = (event.bounds, event.event.position);
        let target = if bounds.contains(&at) {
            let (vertical, ratio) = self.drawn_split();
            let (x, y) = (
                f32::from(at.x - bounds.origin.x),
                f32::from(at.y - bounds.origin.y),
            );
            let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            let size = if vertical { h } else { w };
            let split = tiling::split_point(size, CONTENT_PADDING, TILE_GAP, ratio) / size.max(1.0);
            tiling::stage_target(
                event.drag(cx),
                &self.workspace.layout,
                (x, y, w, h),
                vertical,
                ratio,
                (CONTENT_PADDING, TILE_GAP),
            )
            .map(|target| (target, tiling::preview(target, vertical, split)))
        } else if self
            .drop_target
            .is_some_and(|(target, _)| target == Target::Sidebar)
        {
            return;
        } else {
            None
        };
        if self.drop_target != target {
            self.drop_target = target;
            cx.notify();
        }
    }

    /// Track a session drag over the sidebar: a tile dropped there returns to it.
    fn drag_over_sidebar(&mut self, event: &DragMoveEvent<SessionDrag>, cx: &mut Context<Self>) {
        let inside = event.bounds.contains(&event.event.position);
        let target = if inside {
            let (vertical, _) = self.drawn_split();
            tiling::operation(
                event.drag(cx),
                Target::Sidebar,
                &self.workspace.layout,
                vertical,
            )
            .map(|_| (Target::Sidebar, (0.0, 0.0, 1.0, 1.0)))
        } else if self
            .drop_target
            .is_some_and(|(target, _)| target == Target::Sidebar)
        {
            None
        } else {
            return;
        };
        if self.drop_target != target {
            self.drop_target = target;
            cx.notify();
        }
    }

    /// Keyboard equivalents of the drag actions in a split (lotus#68): Ctrl+Shift+H/V
    /// left/right or top/bottom, R swap, W return the unfocused tile to the sidebar,
    /// Left/Up and Right/Down move the divider by 5%, D equalizes it. Shifted symbols are
    /// avoided: GPUI on Windows reports Ctrl+Shift+, as "<" without shift.
    fn split_shortcut(&mut self, key: &str) -> bool {
        let Layout::Split {
            first,
            second,
            ratio,
            ..
        } = &self.workspace.layout
        else {
            return false;
        };
        let keep = match self.workspace.focused_viewport {
            Viewport::First => first.clone(),
            Viewport::Second => second.clone(),
        };
        let step = |delta: f32| Operation::SetRatio {
            ratio: (ratio + delta).clamp(workspace::RATIO_MIN, workspace::RATIO_MAX),
        };
        let operation = match key.to_ascii_lowercase().as_str() {
            "h" => Operation::SetAxis {
                axis: Axis::Horizontal,
            },
            "v" => Operation::SetAxis {
                axis: Axis::Vertical,
            },
            "r" => Operation::Swap,
            "w" => {
                self.select(keep.clone());
                Operation::Untile { keep: Some(keep) }
            }
            "left" | "up" => step(-0.05),
            "right" | "down" => step(0.05),
            "d" => Operation::SetRatio {
                ratio: workspace::RATIO_DEFAULT,
            },
            _ => return false,
        };
        self.send(Command::Workspace(operation));
        true
    }

    /// The stage as a drop target for sessions and the divider, with the snap preview.
    fn render_drop_stage(&self, stage: AnyElement, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::palette(cx);
        let preview = self
            .drop_target
            .filter(|(target, _)| *target != Target::Sidebar)
            .map(|(_, rect)| rect);
        div()
            .id("drop-stage")
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(stage)
            .on_drag_move(
                cx.listener(|view, event: &DragMoveEvent<SessionDrag>, _, cx| {
                    view.drag_over_stage(event, cx)
                }),
            )
            .on_drag_move(
                cx.listener(|view, event: &DragMoveEvent<DividerDrag>, _, cx| {
                    let (vertical, _) = view.drawn_split();
                    let (bounds, at) = (event.bounds, event.event.position);
                    let (pos, size) = if vertical {
                        (at.y - bounds.origin.y, bounds.size.height)
                    } else {
                        (at.x - bounds.origin.x, bounds.size.width)
                    };
                    let ratio = tiling::ratio_at(
                        f32::from(pos),
                        f32::from(size),
                        CONTENT_PADDING,
                        TILE_GAP,
                    );
                    if view.ratio_drag != Some(ratio) {
                        view.ratio_drag = Some(ratio);
                        cx.notify();
                    }
                }),
            )
            .on_drop(cx.listener(|view, drag: &SessionDrag, _, cx| {
                view.drop_session(drag);
                cx.notify();
            }))
            .children(preview.map(|(x, y, w, h)| {
                div()
                    .absolute()
                    .left(relative(x))
                    .top(relative(y))
                    .w(relative(w))
                    .h(relative(h))
                    .p_2()
                    .child(
                        div()
                            .size_full()
                            .rounded(px(8.0))
                            .border_2()
                            .border_color(theme.info)
                            .bg(theme.info.opacity(0.12)),
                    )
            }))
    }

    /// The sidebar as a drop target: a tile dragged here leaves the split, still running.
    fn render_drop_sidebar(&self, sidebar: AnyElement, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::palette(cx);
        let active = self
            .drop_target
            .is_some_and(|(target, _)| target == Target::Sidebar);
        div()
            .id("drop-sidebar")
            .relative()
            .flex()
            .flex_none()
            .h_full()
            .child(sidebar)
            .on_drag_move(
                cx.listener(|view, event: &DragMoveEvent<SessionDrag>, _, cx| {
                    view.drag_over_sidebar(event, cx)
                }),
            )
            .on_drop(cx.listener(|view, drag: &SessionDrag, _, cx| {
                view.drop_session(drag);
                cx.notify();
            }))
            .when(active, |sidebar| {
                sidebar.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(8.0))
                        .border_2()
                        .border_color(theme.info)
                        .bg(theme.info.opacity(0.12))
                        .text_color(theme.text)
                        .child("Return to sidebar (keeps running)"),
                )
            })
    }

    #[cfg(not(feature = "silvestris-ui"))]
    fn session_row(
        &self,
        summary: &SessionSummary,
        depth: Depth,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = theme::palette(cx);
        let id = summary.info.id.clone();
        let selected = id == self.selected;
        let group = SharedString::from(format!("row-{id}"));
        let is_primary = matches!(summary.info.kind, SessionKind::Primary { .. });
        let fork = match (&summary.info.kind, &summary.info.asset) {
            (SessionKind::Primary { card }, asset) => Some((card.clone(), asset.clone())),
            _ => None,
        };
        let label = session_tree::session_label(&summary.info);
        // Running, loaded sessions can be dragged onto the stage (lotus#68).
        let draggable = summary.info.asset.is_some() && summary.telemetry.state.is_active();
        let drag = self.session_drag(&id, Source::Row);
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
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .child(label),
            );
        if !is_primary {
            name = name.child(
                div()
                    .flex_none()
                    .font_family(theme.mono)
                    .text_xs()
                    .text_color(theme.text_muted)
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
                    theme,
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
        if let SessionKind::Primary { card } = &summary.info.kind {
            let card = card.clone();
            actions = actions.child(
                icon_button(
                    theme,
                    SharedString::from(format!("card-settings-{id}")),
                    Icon::Gear,
                    "Card settings",
                )
                .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    view.open_settings(crate::settings_window::Target::Card(card.clone()), cx);
                })),
            );
        }
        if let Some(tile) = self.tile_with(summary) {
            actions = actions.child(
                icon_button(
                    theme,
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
                    theme,
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
            .when(selected, |row| row.bg(theme.surface_active))
            .when(!selected, |row| {
                row.hover(|style| style.bg(theme.surface_hover))
            })
            .when(depth == Depth::Nested, |row| {
                row.child(
                    div()
                        .absolute()
                        .left(px(16.0))
                        .top_0()
                        .bottom_0()
                        .w(px(1.0))
                        .bg(theme.border_strong),
                )
            })
            .child(state_dot(theme, summary, is_primary))
            .child(name)
            .child(
                div()
                    .flex_none()
                    .font_family(theme.mono)
                    .text_xs()
                    .text_color(theme.text_muted)
                    .group_hover(group, |style| style.invisible())
                    .child(row_meta(summary)),
            )
            .child(div().absolute().right(px(4.0)).child(actions))
            .when(draggable, |row| {
                row.on_drag(drag, |drag: &SessionDrag, _, _, cx| {
                    cx.new(|_| DragGhost(drag.label.clone().into()))
                })
            })
            .on_click(cx.listener(move |view, _: &ClickEvent, _, _| view.select(id.clone())))
    }

    #[cfg(not(feature = "silvestris-ui"))]
    fn offline_card_row(&self, card: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::palette(cx);
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
                    .border_color(theme.border_strong),
            )
            .child(
                div()
                    .flex_1()
                    .truncate()
                    .text_sm()
                    .text_color(theme.text_muted)
                    .child(card.to_string()),
            )
            .child({
                let card = card.to_string();
                icon_button(
                    theme,
                    SharedString::from(format!("card-settings-{card}")),
                    Icon::Gear,
                    "Card settings",
                )
                .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    view.open_settings(crate::settings_window::Target::Card(card.clone()), cx);
                }))
            })
            .when(self.control, |row| {
                row.child(
                    icon_button(
                        theme,
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

    #[cfg(not(feature = "silvestris-ui"))]
    fn render_new_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::palette(cx);
        let mounted: Vec<&str> = self
            .sessions
            .iter()
            .filter_map(|summary| match &summary.info.kind {
                SessionKind::Primary { card } => Some(card.as_str()),
                SessionKind::Secondary { .. } => None,
            })
            .collect();
        let mut menu = popover(theme, "new-session-menu")
            .top(px(TOOLBAR_HEIGHT + 34.0))
            .left(px(8.0))
            .right(px(8.0))
            .on_mouse_down_out(cx.listener(|view, _, _, _| view.menu = Menu::Closed))
            .child(menu_heading(theme, "PRIMARY · PERSISTENT SD"));
        for card in self.cards.iter().filter(|p| !mounted.contains(&p.as_str())) {
            let name = card.clone();
            menu = menu.child(
                menu_item(
                    theme,
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
                menu_item(theme, "new-card", "New card", fresh.clone()).on_click(cx.listener(
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
            .child(div().h(px(1.0)).my_1().bg(theme.border))
            .child(menu_heading(theme, "SECONDARY · DISCARDED ON STOP"))
            .child(
                menu_item(theme, "new-empty", "Empty", "blank SD").on_click(cx.listener(
                    |view, _: &ClickEvent, _, _| {
                        view.create(SessionKind::Secondary { parent: None }, None);
                    },
                )),
            );
        for card in &self.cards {
            let parent = card.clone();
            menu = menu.child(
                menu_item(
                    theme,
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
        let theme = theme::palette(cx);
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
            .border_color(theme.border)
            .text_xs()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(section_label(theme, "CONTROLLERS"))
                    .when(!listing.pads.is_empty(), |row| {
                        let label = if listing.identifying {
                            "Press a button…"
                        } else {
                            "Identify"
                        };
                        row.child(compact_tooltip(
                            small_button(theme, "identify-controller", label, Tone::Plain, true)
                                .on_click(cx.listener(|view, _: &ClickEvent, _, _| {
                                    if let Some(controller) = &view.controller {
                                        controller.input().identify();
                                    }
                                })),
                            "Highlight the next controller that presses a button",
                        ))
                    }),
            );
        if listing.pads.is_empty() {
            section = section.child(
                div()
                    .text_color(theme.text_muted)
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
                    theme.warning
                } else if pad.session.is_some() {
                    theme.success
                } else {
                    theme.border_strong
                }))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(div().truncate().child(format!("{} · {device}", pad.name)))
                        .child(div().truncate().text_color(theme.text_muted).child(target)),
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
                row = row.child(compact_tooltip(
                    small_button(theme, format!("assign-{device}"), label, Tone::Plain, true)
                        .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                            view.assign_controller(device, Some(session.clone()), replace);
                        })),
                    tip,
                ));
            }
            if pad.session.is_some() && self.control {
                row = row.child(
                    small_button(
                        theme,
                        format!("unassign-{device}"),
                        "Unassign",
                        Tone::Plain,
                        true,
                    )
                    .on_click(cx.listener(
                        move |view, _: &ClickEvent, _, _| {
                            view.assign_controller(device, None, false);
                        },
                    )),
                );
            }
            section = section.child(row);
        }
        for entry in &listing.remembered {
            section = section.child(div().truncate().text_color(theme.text_muted).child(format!(
                "{} disconnected; was on {}",
                entry.name,
                self.session_name(&entry.session)
            )));
        }
        section
    }

    fn render_sidebar_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::palette(cx);
        let (colour, text) = match self.mcp {
            McpLink::Attached => (theme.success, "MCP attached"),
            McpLink::Closed => (theme.warning, "MCP disconnected"),
            McpLink::Off => (theme.border_strong, "MCP off"),
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
            .border_color(theme.border)
            .text_xs()
            .text_color(theme.text_muted)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().size(px(6.0)).rounded_full().bg(colour))
                    .child(div().flex_1().truncate().child(text))
                    .child(div().font_family(theme.mono).child(slots))
                    .child(self.preferences_button(cx)),
            )
            .when_some(self.artifacts.as_ref(), |footer, usage| {
                // Quota and retention at a glance; details per session in lotus_status.
                let pinned: usize = usage.sessions.values().map(|s| s.pinned).sum();
                footer.child(div().font_family(theme.mono).truncate().child(format!(
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
    #[cfg(not(feature = "silvestris-ui"))]
    fn render_toolbar(&self, density: ToolbarDensity, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::palette(cx);
        let mut bar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .h(px(TOOLBAR_HEIGHT))
            .px_4()
            .border_b_1()
            .border_color(theme.border);
        let Some(summary) = self.selected_summary() else {
            return bar.child(
                div()
                    .text_sm()
                    .text_color(theme.text_muted)
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
                    theme.info
                } else {
                    theme.warning
                },
            )));
        }
        if density == ToolbarDensity::Full {
            bar = bar.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(theme.text_muted)
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
                    .text_color(theme.text_muted)
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
                        theme,
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
                        theme,
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
                icon_button(theme, "toolbar-more", Icon::More, "More actions").on_click(
                    cx.listener(|view, _: &ClickEvent, _, _| {
                        view.menu = match view.menu {
                            Menu::Toolbar => Menu::Closed,
                            _ => Menu::Toolbar,
                        };
                    }),
                ),
            );
        }
        if self.control && active {
            let reset_id = id.clone();
            bar = bar
                .child(
                    tool_button(
                        theme,
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
                        theme,
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
                    theme,
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
            button(theme, "zoom", self.zoom.label(), Tone::Plain, true)
        } else {
            with_tooltip(
                icon_tool(theme, "zoom", Icon::Zoom).child(
                    div()
                        .text_xs()
                        .font_family(theme.mono)
                        .child(self.zoom.label()),
                ),
                zoom_tip,
            )
        };
        let screens = if labelled {
            button(theme, "layout", self.layout.label(), Tone::Plain, true)
        } else {
            with_tooltip(icon_tool(theme, "layout", self.layout.icon()), layout_tip)
        };
        bar.child(
            div()
                .flex_none()
                .w(px(1.0))
                .h(px(20.0))
                .mx_1()
                .bg(theme.border),
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
    #[cfg(not(feature = "silvestris-ui"))]
    fn render_toolbar_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::palette(cx);
        let mut menu = popover(theme, "toolbar-menu")
            .top(px((workspace_toolbar_height() - 4.0).max(4.0)))
            .right(px(16.0))
            .w(px(240.0))
            .on_mouse_down_out(cx.listener(|view, _, _, _| view.menu = Menu::Closed));
        if let Some(summary) = self.selected_summary() {
            let id = summary.info.id.clone();
            let loaded = summary.info.asset.is_some();
            if self.control && summary.telemetry.state.is_active() {
                let reset_id = id.clone();
                menu = menu
                    .child(menu_heading(theme, "SESSION"))
                    .child(
                        menu_item(theme, "menu-reset", "Reset", "")
                            .when(!loaded, |item| item.opacity(0.4))
                            .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                                view.menu = Menu::Closed;
                                if loaded {
                                    view.send(Command::Reset(reset_id.clone()));
                                }
                            })),
                    )
                    .child(menu_item(theme, "menu-load", "Load…", "").on_click(
                        cx.listener(|view, _: &ClickEvent, _, _| view.menu = Menu::Load),
                    ));
            }
            if self.control {
                menu = menu.child(
                    menu_item(theme, "menu-stop", "Stop", "")
                        .text_color(theme.error)
                        .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                            view.menu = Menu::Closed;
                            view.send(Command::Stop(id.clone()));
                        })),
                );
            }
            menu = menu.child(div().h(px(1.0)).my_1().bg(theme.border));
        }
        menu = menu.child(menu_heading(theme, "ZOOM"));
        for zoom in [Zoom::Fit, Zoom::Native, Zoom::Double] {
            menu = menu.child(
                checkbox_item(
                    theme,
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
        menu = menu.child(menu_heading(theme, "SCREENS"));
        for layout in [ScreenLayout::Stacked, ScreenLayout::SideBySide] {
            menu = menu.child(
                checkbox_item(
                    theme,
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

    #[cfg(not(feature = "silvestris-ui"))]
    fn render_load_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::palette(cx);
        let session = self.selected.clone();
        popover(theme, "load-menu")
            .top(px((workspace_toolbar_height() - 4.0).max(4.0)))
            .right(px(16.0))
            .w(px(280.0))
            .on_mouse_down_out(cx.listener(|view, _, _, _| view.menu = Menu::Closed))
            .child(menu_heading(theme, "APPROVED CONTENT"))
            .child(self.asset_list(Some(session.clone()), cx))
            .child(div().h(px(1.0)).my_1().bg(theme.border))
            .child(
                menu_item(theme, "open-file", "Open file…", "Ctrl+O").on_click(cx.listener(
                    move |view, _: &ClickEvent, _, cx| view.open_file(Some(session.clone()), cx),
                )),
            )
    }

    #[cfg(not(feature = "silvestris-ui"))]
    fn asset_list(&self, session: Option<String>, cx: &mut Context<Self>) -> Div {
        let theme = theme::palette(cx);
        let mut list = div().flex().flex_col();
        if self.assets.is_empty() {
            list = list.child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(theme.text_muted)
                    .child("None configured for this host."),
            );
        }
        for asset in &self.assets {
            let handle = asset.clone();
            let session = session.clone();
            list = list.child(
                menu_item(
                    theme,
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
        let theme = theme::palette(cx);
        if self.controller.is_none() {
            #[cfg(feature = "silvestris-ui")]
            return self.render_shared_disconnected(cx);
            #[cfg(not(feature = "silvestris-ui"))]
            return empty_state(
                theme,
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
            let mut state = empty_state(theme,
                "No sessions",
                "A primary session keeps a persistent SD card. A secondary is a disposable copy, discarded when it stops.".to_string(),
            );
            if self.control {
                let fresh = session_tree::next_card_name(&self.cards);
                let primary = self.cards.first().cloned().unwrap_or(fresh);
                state = state.child(
                    div()
                        .flex()
                        .gap_2()
                        .mt_2()
                        .child(
                            workspace_button(
                                theme,
                                "empty-primary",
                                "New primary",
                                Tone::Primary,
                                true,
                            )
                            .on_click(cx.listener(
                                move |view, _: &ClickEvent, _, _| {
                                    view.create(
                                        SessionKind::Primary {
                                            card: primary.clone(),
                                        },
                                        None,
                                    );
                                },
                            )),
                        )
                        .child(
                            workspace_button(
                                theme,
                                "empty-secondary",
                                "New secondary",
                                Tone::Plain,
                                true,
                            )
                            .on_click(cx.listener(
                                |view, _: &ClickEvent, _, _| {
                                    view.create(SessionKind::Secondary { parent: None }, None);
                                },
                            )),
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
                theme,
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
                    workspace_button(
                        theme,
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
            #[cfg(not(feature = "silvestris-ui"))]
            let session = summary.info.id.clone();
            let mut state = empty_state(
                theme,
                "Nothing loaded",
                "Choose approved content, or open a .3dsx file.".to_string(),
            );
            #[cfg(feature = "silvestris-ui")]
            if self.control {
                state = state.child(self.render_empty_content_actions(cx));
            }
            #[cfg(not(feature = "silvestris-ui"))]
            if self.control {
                state = state.child(
                    div()
                        .w(px(320.0))
                        .mt_2()
                        .p_1()
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(theme.border)
                        .child(self.asset_list(Some(session.clone()), cx))
                        .child(div().h(px(1.0)).my_1().bg(theme.border))
                        .child(
                            menu_item(theme, "idle-open-file", "Open file…", "Ctrl+O").on_click(
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
        let theme = theme::palette(cx);
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
        let height =
            viewport_height - workspace_toolbar_height() - gauge_height - 2.0 * CONTENT_PADDING;
        let ratio = self.ratio_drag.unwrap_or(*ratio);
        let split = split_plan(width, height, *axis, ratio, *screens, self.split_held.get());
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
                    .when(split.vertical, |row| row.flex_col())
                    .children(tiles.into_iter().enumerate().flat_map(
                        |(index, (viewport, session, other, tile))| {
                            let gauges = self.render_tile_gauges(
                                theme,
                                viewport,
                                session,
                                split.vertical,
                                tile,
                                #[cfg(feature = "silvestris-ui")]
                                cx,
                            );
                            let tile = self.render_tile(viewport, session, other, tile, *axis, cx);
                            // Gauges sit on the outer edge: before the first tile, after the second.
                            let (lead, trail) = match viewport {
                                Viewport::First => (gauges, tile),
                                Viewport::Second => (tile, gauges),
                            };
                            let slot = div()
                                .flex()
                                .flex_none()
                                .gap(px(GAUGE_GAP))
                                .when(split.vertical, |slot| slot.flex_col())
                                .child(lead)
                                .child(trail)
                                .into_any_element();
                            // The gap between the tiles is the divider (lotus#68).
                            let divider =
                                (index == 0).then(|| self.render_split_divider(split.vertical, cx));
                            std::iter::once(slot).chain(divider)
                        },
                    )),
            )
            .into_any_element()
    }

    /// Drag to resize the split; double-click returns it to an even split.
    fn render_split_divider(&self, vertical: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme::palette(cx);
        div()
            .id("split-divider")
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded(px(4.0))
            .when(vertical, |divider| {
                divider.h(px(TILE_GAP)).cursor_row_resize()
            })
            .when(!vertical, |divider| {
                divider.w(px(TILE_GAP)).cursor_col_resize()
            })
            .hover(|style| style.bg(theme.surface_hover))
            .child(
                div()
                    .rounded(px(1.0))
                    .bg(theme.border_strong)
                    .when(vertical, |grip| grip.w(px(32.0)).h(px(2.0)))
                    .when(!vertical, |grip| grip.w(px(2.0)).h(px(32.0))),
            )
            .on_drag(DividerDrag, |_: &DividerDrag, _, _, cx| {
                cx.new(|_| DragGhost(SharedString::default()))
            })
            .on_click(cx.listener(|view, event: &ClickEvent, _, _| {
                if event.click_count() >= 2 {
                    view.send(Command::Workspace(Operation::SetRatio {
                        ratio: workspace::RATIO_DEFAULT,
                    }));
                }
            }))
            .into_any_element()
    }

    /// Floating inspector for one tile's session (lotus#71): the same panels and debug
    /// commands as the docked inspector, aimed at [`Self::inspected`], over that tile's side
    /// of the stage without shrinking either tile. Views, run/pause/step and close in its
    /// header; resizable from its inner edge.
    #[cfg(not(feature = "silvestris-ui"))]
    fn render_tile_inspector(
        &self,
        plan: &Plan,
        session: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = theme::palette(cx);
        let width = self.overlay_width.clamp(
            layout::INSPECTOR_MIN,
            (plan.main - 48.0).max(layout::INSPECTOR_MIN),
        );
        let left = self.overlay_left();
        let summary = self.sessions.iter().find(|s| s.info.id == session);
        // Tiles can share a name; the short id is what the session list shows beside it.
        let label = format!(
            "{} {}",
            self.session_name(session),
            session_tree::short_id(session)
        );
        // Data is shown only once the controller targets this session.
        let ready = self.inspected == session;
        let loaded =
            summary.is_some_and(|s| s.info.asset.is_some() && s.telemetry.state.is_active());
        let playing = summary.is_some_and(|s| s.telemetry.playing);
        let usable = self.control && loaded && ready;
        let (run, step) = (session.to_string(), session.to_string());
        let close = session.to_string();
        let mut views = div()
            .id("overlay-views")
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .overflow_x_scroll()
            .border_b_1()
            .border_color(theme.border);
        for panel in Panel::RAIL.into_iter().chain(Panel::OVERFLOW) {
            views = views.child(
                rail_button(
                    theme,
                    SharedString::from(format!("overlay-view-{}", panel.label())),
                    panel.icon(),
                    panel == self.inspection_panel,
                    true,
                    panel.label(),
                )
                .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                    view.choose_inspection(panel, 0);
                })),
            );
        }
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .h(px(TOOLBAR_HEIGHT))
            .pl_3()
            .pr_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .child(format!("Inspecting {label}")),
            )
            .child(
                rail_button(
                    theme,
                    "overlay-run",
                    if playing { Icon::Pause } else { Icon::Play },
                    false,
                    usable,
                    format!("{} {label}", if playing { "Pause" } else { "Run" }),
                )
                .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                    if usable {
                        view.send(if playing {
                            Command::Pause(run.clone())
                        } else {
                            Command::Play(run.clone())
                        });
                    }
                })),
            )
            .child(
                rail_button(
                    theme,
                    "overlay-step",
                    Icon::Step,
                    false,
                    usable && !playing,
                    format!("Step {label} one frame"),
                )
                .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                    if usable && !playing {
                        view.send(Command::Step(step.clone()));
                    }
                })),
            )
            .child(
                icon_button(
                    theme,
                    "overlay-close",
                    Icon::Close,
                    "Close inspector (Ctrl+I)",
                )
                .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                    if view.inspect_tile.as_deref() == Some(close.as_str()) {
                        view.toggle_tile_inspector(close.clone());
                    }
                })),
            );
        let edge = div()
            .id("overlay-divider")
            .absolute()
            .top_0()
            .bottom_0()
            .w(px(DIVIDER_WIDTH))
            .when(left, |edge| edge.right_0())
            .when(!left, |edge| edge.left_0())
            .cursor_col_resize()
            .hover(|style| style.bg(theme.surface_active))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    view.release_input();
                    view.overlay_drag = true;
                }),
            );
        let body = if ready {
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .child(self.render_inspector(width, false, cx))
                .into_any_element()
        } else {
            tile_message(theme, "Switching inspector…")
        };
        div()
            .id("tile-inspector")
            .absolute()
            // Below the stage toolbar so the selected session's controls stay reachable.
            .top(px(TOOLBAR_HEIGHT))
            .bottom_0()
            .when(left, |overlay| overlay.left(px(plan.sessions_width())))
            .when(!left, |overlay| overlay.right_0())
            .w(px(width))
            .flex()
            .flex_col()
            .bg(theme.panel)
            .border_1()
            .border_color(theme.border_strong)
            .shadow_lg()
            .occlude()
            .child(header)
            .child(views)
            .child(body)
            .child(edge)
            .into_any_element()
    }

    /// One tile's own gauges (lotus#70): state, FPS, speed, frame (the remaining telemetry
    /// in its tooltip) and the controllers assigned to that session. A column beside a
    /// left/right tile, a row above or below a top/bottom tile.
    fn render_tile_gauges(
        &self,
        theme: theme::Palette,
        viewport: Viewport,
        session: &str,
        vertical: bool,
        tile: TilePlan,
        #[cfg(feature = "silvestris-ui")] cx: &mut Context<Self>,
    ) -> AnyElement {
        let summary = self.sessions.iter().find(|s| s.info.id == session);
        let mut items = Vec::new();
        match summary {
            None => items.push(gauges::Gauge {
                label: "STATE",
                value: "ended".into(),
                colour: theme.text_muted,
                detail: "the session is no longer registered".into(),
            }),
            Some(summary) => {
                items.push(gauges::Gauge {
                    label: "STATE",
                    value: state_name(summary).into(),
                    colour: state_colour(theme, summary),
                    detail: session_tree::session_label(&summary.info).to_string(),
                });
                let [fps, speed, frame, rest @ ..] = gauges::gauges(theme, &summary.telemetry);
                let more = rest
                    .iter()
                    .map(|gauge| format!("{}: {} · {}", gauge.label, gauge.value, gauge.detail))
                    .collect::<Vec<_>>()
                    .join("\n");
                items.extend([
                    fps,
                    speed,
                    gauges::Gauge {
                        detail: format!("{}\n{more}", frame.detail),
                        ..frame
                    },
                ]);
            }
        }
        let paired = pairing(&self.controllers, session);
        if paired.is_empty() {
            items.push(gauges::Gauge {
                label: "PAD",
                value: "none".into(),
                colour: theme.text_muted,
                detail: "No controller is assigned to this session".into(),
            });
        }
        let target = self.session_name(session);
        for pad in paired {
            items.push(gauges::Gauge {
                label: "PAD",
                value: pad.name.clone(),
                colour: if pad.connected {
                    theme.success
                } else {
                    theme.warning
                },
                detail: match &pad.device {
                    Some(device) => format!("{} · {device} → {target}", pad.name),
                    None => format!(
                        "{} → {target}: disconnected; the assignment returns when it reconnects",
                        pad.name
                    ),
                },
            });
        }
        let index = match viewport {
            Viewport::First => 0,
            Viewport::Second => 1,
        };
        #[cfg(feature = "silvestris-ui")]
        let strip = self.render_tile_status(items, vertical, tile, cx);
        #[cfg(not(feature = "silvestris-ui"))]
        let count = items.len() as f32;
        #[cfg(not(feature = "silvestris-ui"))]
        let strip = if vertical {
            let width = ((tile.width - (count - 1.0) * layout::PILL_GAP) / count).max(0.0);
            gauges::gauge_pills(theme, items, width, false)
                .w(px(tile.width.max(0.0)))
                .h(px(GAUGE_ROW))
                .items_center()
        } else {
            gauges::gauge_pills(theme, items, GAUGE_COLUMN, false)
                .flex_col()
                .w(px(GAUGE_COLUMN))
                .h(px(tile.height.max(0.0)))
        };
        div()
            .id(SharedString::from(format!("tile-gauges-{index}")))
            .flex_none()
            .overflow_hidden()
            .child(strip)
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
        let theme = theme::palette(cx);
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
        let keep = other.to_string();
        let mut header = div()
            .id(SharedString::from(format!("tile-header-{index}")))
            .on_drag(
                self.session_drag(session, Source::Tile),
                |drag: &SessionDrag, _, _, cx| cx.new(|_| DragGhost(drag.label.clone().into())),
            )
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
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .child(label.clone()),
            )
            .when(focused, |header| {
                header.child(div().flex_none().text_color(theme.info).child("Keyboard"))
            })
            .child(
                icon_button(
                    theme,
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
            header = header.child(compact_tooltip(
                small_button(theme, "tile-axis", label, Tone::Plain, true).on_click(cx.listener(
                    move |view, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        view.send(Command::Workspace(Operation::SetAxis { axis: next }));
                    },
                )),
                description,
            ));
        }
        let inspecting = self.inspect_tile.as_deref() == Some(session);
        let target = session.to_string();
        let description = format!(
            "{} {label} (Ctrl+I on the focused tile)",
            if inspecting {
                "Close inspector for"
            } else {
                "Inspect"
            },
        );
        #[cfg(feature = "silvestris-ui")]
        let inspect = icon_button(
            theme,
            SharedString::from(format!("inspect-{index}")),
            Icon::Cpu,
            description,
        )
        .selected(inspecting);
        #[cfg(not(feature = "silvestris-ui"))]
        let inspect = rail_button(
            theme,
            SharedString::from(format!("inspect-{index}")),
            Icon::Cpu,
            inspecting,
            true,
            description,
        )
        .size(px(24.0));
        header = header.child(
            inspect.on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                view.toggle_tile_inspector(target.clone());
            })),
        );
        header = header.child(
            icon_button(
                theme,
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
            None => tile_message(theme, "Session ended"),
            Some(summary) if summary.telemetry.state == SessionState::Crashed => {
                tile_message(theme, "Worker crashed")
            }
            Some(summary) if summary.info.asset.is_none() => tile_message(theme, "Nothing loaded"),
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
            .border_color(if focused { theme.info } else { theme.border })
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
        let theme = theme::palette(cx);
        let tile = self.tiles.get(session);
        let mut top = screen_frame(theme, 400.0 * scale, 240.0 * scale).id("top-screen");
        if let Some(image) = tile.and_then(|tile| tile.images.first()) {
            top = top.child(gpui::img(image.clone()).size_full());
        }
        let rect = tile.map(|tile| tile.touch.clone()).unwrap_or_default();
        let (down, moved) = (session.to_string(), session.to_string());
        let mut bottom = screen_frame(theme, 320.0 * scale, 240.0 * scale)
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
                let height = viewport_height
                    - workspace_toolbar_height()
                    - gauge_height
                    - 2.0 * CONTENT_PADDING;
                fit_scale(self.layout, width, height)
            }
        }
    }

    fn choose_inspection(&mut self, panel: Panel, offset: u32) {
        self.inspection_panel = panel;
        #[cfg(feature = "silvestris-ui")]
        if self.preferences.ui.inspector_panel != panel.id() {
            self.preferences.ui.inspector_panel = panel.id().into();
            self.preferences.save();
        }
        if let Some(controller) = &self.controller {
            *controller.inspection_selection.lock().unwrap() = (panel, offset);
        }
    }
    fn set_inspection(&mut self, snapshot: Option<InspectionSnapshot>, cx: &mut Context<Self>) {
        // Polls clone the same immutable Arc. A new result must not inherit a
        // previous command's image, even when both belong to the same epoch.
        let old = self.inspection.as_ref().map(|s| Arc::as_ptr(&s.2));
        let new = snapshot.as_ref().map(|s| Arc::as_ptr(&s.2));
        if old != new {
            self.surface_target = None;
            if let Some(image) = self.surface_image.take() {
                cx.drop_image(image, None);
            }
        }
        self.inspection = snapshot;
    }

    fn retain_current_inspection(&mut self, cx: &mut Context<Self>) {
        self.registers = self
            .registers
            .take()
            .filter(|(target, _)| target.is_current(&self.inspected, &self.sessions));
        self.inspection = self.inspection.take().filter(|snapshot| {
            inspection::snapshot_current(snapshot, &self.inspected, &self.sessions)
        });
        let local_trace = self
            .inspection
            .as_ref()
            .is_some_and(|snapshot| snapshot.0.is_none());
        if local_trace
            || self
                .surface_target
                .as_ref()
                .is_none_or(|target| !target.is_current(&self.inspected, &self.sessions))
        {
            self.surface_target = None;
            if let Some(old) = self.surface_image.take() {
                cx.drop_image(old, None);
            }
        }
    }

    fn inspection_target(&self) -> Option<SessionTarget> {
        #[cfg(feature = "silvestris-ui")]
        if self.shared_inspector_target() != Some(self.inspected.as_str()) {
            return None;
        }
        self.sessions
            .iter()
            .find(|s| s.info.id == self.inspected)
            .map(SessionTarget::from_summary)
    }

    fn debug_command(&mut self, target: &Option<SessionTarget>, command: D) {
        let Some(target) = target.as_ref().filter(|target| {
            self.control
                && target.resolve(&self.sessions).is_some()
                && self.inspection_target().as_ref() == Some(*target)
        }) else {
            return;
        };
        let target = target.clone();
        self.choose_inspection(Panel::Result, 0);
        self.send(Command::DebugAt { target, command });
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
        if self.split() {
            // The focused tile's floating inspector; the docked pane state stays as saved.
            if self.inspect_tile.is_none() {
                if let Some(session) = self.keyboard_target() {
                    self.toggle_tile_inspector(session);
                }
            }
            #[cfg(feature = "silvestris-ui")]
            self.open_pane(Pane::Inspector);
            return;
        }
        if !self.preferences.values.inspector_open || self.recent != Pane::Inspector {
            self.open_pane(Pane::Inspector);
        }
    }

    /// The inspector rail: always visible at the window's right edge. Views
    /// (navigation) at the top, execution quick actions separated at the
    /// bottom. No Stop/Reset/trace commands here.
    #[cfg(not(feature = "silvestris-ui"))]
    fn render_inspector_rail(&self, plan: &Plan, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::palette(cx);
        let open = if cfg!(feature = "silvestris-ui") {
            self.preferences.values.inspector_open
        } else {
            plan.inspector.is_some()
        };
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
                views = views.child(div().w(px(20.0)).h(px(1.0)).my_1().bg(theme.border));
            }
            group = panel.group();
            let current = open && self.inspection_panel == panel;
            views = views.child(
                rail_button(
                    theme,
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
        views = views.child(div().w(px(20.0)).h(px(1.0)).my_1().bg(theme.border));
        views = views.child(
            rail_button(
                theme,
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
            .border_color(theme.border)
            .child(
                rail_button(
                    theme,
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
                    theme,
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
            .bg(theme.panel)
            .border_l_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .flex_none()
                    .h(px(TOOLBAR_HEIGHT))
                    .w_full()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        icon_button(theme,
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
            .when(!open && !cfg!(feature = "silvestris-ui"), |rail| rail.child(self.divider(Pane::Inspector, cx)))
            .when(self.menu == Menu::InspectorMore, |rail| {
                rail.child(self.render_more_views(cx))
            })
    }

    #[cfg(not(feature = "silvestris-ui"))]
    fn render_more_views(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::palette(cx);
        let mut menu = popover(theme, "inspector-more")
            .top(px(TOOLBAR_HEIGHT + 8.0))
            .right(px(layout::INSPECTOR_RAIL - 4.0))
            .w(px(200.0))
            .on_mouse_down_out(cx.listener(|view, _, _, _| view.menu = Menu::Closed));
        let mut group = "";
        for panel in Panel::OVERFLOW {
            if group != panel.group() {
                group = panel.group();
                menu = menu.child(menu_heading(theme, group));
            }
            let current = self.inspection_panel == panel;
            menu = menu.child(
                menu_item(
                    theme,
                    SharedString::from(format!("more-{}", panel as usize)),
                    panel.label(),
                    if current { "shown" } else { "" },
                )
                .when(current, |item| item.bg(theme.surface_active))
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

    /// Legacy: icon, view name and group. Silvestris: a pop-up choosing the
    /// view, replacing the rail (HIG p. 341, pop-up instead of tabs).
    fn render_inspector_heading(&self, current: Panel, cx: &mut Context<Self>) -> AnyElement {
        #[cfg(feature = "silvestris-ui")]
        {
            let items = Panel::MENU
                .iter()
                .map(|p| SharedString::from(p.label()))
                .collect();
            let selected = Panel::MENU.iter().position(|p| *p == current);
            div()
                .flex_1()
                .min_w_0()
                .child(
                    silvestris::controls::PopUpButton::new(
                        "inspector-view",
                        "Inspector view",
                        items,
                    )
                    .selected(selected)
                    .size(silvestris::controls::ControlSize::Small)
                    .min_width(140.0)
                    .on_choose(cx.listener(|view, index: &usize, _, cx| {
                        if let Some(panel) = Panel::MENU.get(*index) {
                            view.show_panel(*panel);
                            cx.notify();
                        }
                    })),
                )
                .into_any_element()
        }
        #[cfg(not(feature = "silvestris-ui"))]
        {
            let theme = theme::palette(cx);
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap_2()
                .child(icon(current.icon(), 16.0, theme.text))
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
                        .child(section_label(theme, current.group())),
                )
                .into_any_element()
        }
    }

    fn render_inspector(
        &self,
        width: f32,
        docked: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = theme::palette(cx);
        let target = self.inspection_target();
        let current = self.inspection_panel;
        let mut panel = div()
            .id("inspector")
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(width))
            .h_full()
            .bg(theme.panel)
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
                    .border_color(theme.border)
                    .child(self.render_inspector_heading(current, cx))
                    .child(
                        icon_button(
                            theme,
                            "inspector-utility",
                            Icon::More,
                            "Trace import, clipboard command, keys",
                        )
                        .on_click(cx.listener(
                            |view, event: &ClickEvent, window, cx| {
                                #[cfg(feature = "silvestris-ui")]
                                view.open_inspector_utilities(event, window, cx);
                                #[cfg(not(feature = "silvestris-ui"))]
                                {
                                    let _ = (event, window, cx);
                                    view.menu = match view.menu {
                                        Menu::InspectorUtility => Menu::Closed,
                                        _ => Menu::InspectorUtility,
                                    };
                                }
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
                .border_color(theme.border);
            for (label, command, disruptive) in commands {
                let target = target.clone();
                let tone = if disruptive {
                    Tone::Danger
                } else {
                    Tone::Plain
                };
                actions = actions.child(
                    small_button(theme, format!("debug-{label}"), label, tone, self.control)
                        .on_click(cx.listener(move |view, _, _, _| {
                            if view.control {
                                view.debug_command(&target, command.clone());
                            }
                        })),
                );
            }
            panel = panel.child(actions);
        }
        if docked {
            panel = panel.child(self.divider(Pane::Inspector, cx));
        }
        // Popovers paint last so the row list never covers them.
        #[cfg(feature = "silvestris-ui")]
        let utility: Option<gpui::Empty> = None;
        #[cfg(not(feature = "silvestris-ui"))]
        let utility = (self.menu == Menu::InspectorUtility).then(|| self.render_utility_menu(cx));
        let view = self.inspection.as_ref().filter(|snapshot| {
            snapshot.1 == current
                && inspection::snapshot_current(snapshot, &self.inspected, &self.sessions)
        });
        let Some((_, _, view)) = view else {
            return panel
                .child(
                    div()
                        .p_3()
                        .text_sm()
                        .text_color(theme.text_secondary)
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
                    .text_color(theme.text_muted)
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
                            small_button(theme, "inspect-next", "Next page", Tone::Plain, true)
                                .on_click(cx.listener(move |view, _, _, _| {
                                    view.choose_inspection(current, next)
                                })),
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
        #[cfg(not(feature = "silvestris-ui"))]
        use gpui::uniform_list as inspection_list;
        #[cfg(feature = "silvestris-ui")]
        use silvestris::controls::table::virtual_table as inspection_list;
        panel
            .child(
                inspection_list("inspection-rows", rows.len(), move |range, _, cx| {
                    range
                        .map(|index| {
                            let row = &rows[index];
                            let weak = weak.clone();
                            let target = target.clone();
                            let action = row.action.clone();
                            let tooltip = format!("{}\n{}", row.key, row.value);
                            #[cfg(feature = "silvestris-ui")]
                            let line = silvestris::controls::table::code_row(
                                ("inspection-row", index),
                                28.0,
                                cx,
                            );
                            #[cfg(not(feature = "silvestris-ui"))]
                            let line = {
                                let _ = cx;
                                div()
                                    .id(("inspection-row", index))
                                    .flex()
                                    .h(px(28.0))
                                    .px_2()
                                    .gap_2()
                                    .items_center()
                                    .overflow_hidden()
                                    .font_family(theme.mono)
                                    .text_xs()
                            };
                            with_tooltip(
                                line.when(show_lane, |line| {
                                    line.child(
                                        div()
                                            .w(px(48.0))
                                            .flex_none()
                                            .text_color(theme.text_muted)
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
                                    div().flex_1().min_w_0().truncate().child(row.value.clone()),
                                ),
                                tooltip,
                            )
                            .when_some(action, |element, command| {
                                element
                                    .cursor_pointer()
                                    .hover(|style| style.bg(theme.surface_hover))
                                    .on_click(move |_, _, cx| {
                                        let _ = weak.update(cx, |view, _| {
                                            view.debug_command(&target, command.clone())
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
    #[cfg(not(feature = "silvestris-ui"))]
    fn render_utility_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::palette(cx);
        let target = self.inspection_target();
        let mut keys = div().flex().flex_col().px_2().pb_1();
        for (key, action) in KEYS {
            keys = keys.child(
                div()
                    .flex()
                    .justify_between()
                    .gap_3()
                    .h(px(20.0))
                    .text_xs()
                    .child(div().font_family(theme.mono).child(key))
                    .child(div().text_color(theme.text_muted).child(action)),
            );
        }
        popover(theme, "inspector-utility-menu")
            .top(px(TOOLBAR_HEIGHT - 6.0))
            .right(px(8.0))
            .w(px(280.0))
            .on_mouse_down_out(cx.listener(|view, _, _, _| view.menu = Menu::Closed))
            .child(
                menu_item(theme, "open-trace", "Open JSONL trace…", "≤ 64 MiB").on_click(
                    cx.listener(|view, _, _, cx| {
                        view.menu = Menu::Closed;
                        view.open_trace(cx);
                    }),
                ),
            )
            .child(
                menu_item(
                    theme,
                    "clipboard-command",
                    "Run clipboard JSON",
                    "debug command",
                )
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.menu = Menu::Closed;
                    view.run_clipboard_command(&target, cx);
                })),
            )
            .child(div().h(px(1.0)).my_1().bg(theme.border))
            .child(menu_heading(theme, "KEYBOARD"))
            .child(keys)
    }

    fn run_clipboard_command(&mut self, target: &Option<SessionTarget>, cx: &mut gpui::App) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        if text.len() > 65536 {
            self.error = "Command exceeds 64 KiB".into();
            return;
        }
        match serde_json::from_str::<D>(&text) {
            Ok(command) => match command.validate() {
                Ok(()) => self.debug_command(target, command),
                Err(error) => self.error = error.into(),
            },
            Err(error) => self.error = error.to_string(),
        }
    }

    fn render_error(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = theme::palette(cx);
        if self.error.is_empty() || self.error == self.dismissed_error {
            return None;
        }
        let dismiss = icon_button(theme, "dismiss-error", Icon::Close, "Dismiss").on_click(
            cx.listener(|view, _: &ClickEvent, _, cx| {
                view.dismissed_error = view.error.clone();
                cx.notify();
            }),
        );
        #[cfg(feature = "silvestris-ui")]
        return Some(
            silvestris::controls::notice::error_banner(self.error.clone(), dismiss, cx)
                .into_any_element(),
        );
        #[cfg(not(feature = "silvestris-ui"))]
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
                .border_color(theme.error)
                .text_sm()
                .child(div().size(px(6.0)).rounded_full().bg(theme.error))
                .child(div().flex_1().child(self.error.clone()))
                .child(dismiss)
                .into_any_element(),
        )
    }

    #[cfg(not(feature = "silvestris-ui"))]
    fn render_gauges(
        &self,
        mode: GaugeMode,
        main: f32,
        summary: &SessionSummary,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = theme::palette(cx);
        let telemetry = &summary.telemetry;
        match mode {
            GaugeMode::Cards => gauges::gauge_cards(theme, telemetry).into_any_element(),
            GaugeMode::Compact { expanded } => {
                let pills = layout::pills(main);
                let [fps, speed, frame, rest @ ..] = gauges::gauges(theme, telemetry);
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(px(layout::PILL_GAP))
                    .w(px(main))
                    .p(px(layout::PILL_PADDING))
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(layout::PILL_GAP))
                            .child(gauges::gauge_pills(
                                theme,
                                [fps, speed, frame],
                                pills.primary_width,
                                false,
                            ))
                            .child(
                                icon_button(
                                    theme,
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
                        strip.child(gauges::gauge_pills(
                            theme,
                            rest,
                            pills.secondary_width,
                            true,
                        ))
                    })
                    .into_any_element()
            }
        }
    }
}

impl LotusWindow {
    fn render_workspace(
        &mut self,
        plan: &Plan,
        viewport_height: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        #[cfg(not(feature = "silvestris-ui"))]
        let mode = layout::gauge_mode(plan.main, viewport_height, self.gauges_expanded);
        // A split shows each console's gauges beside its own tile instead (lotus#70).
        let split = matches!(self.workspace.layout, Layout::Split { .. });
        let loaded = self
            .selected_summary()
            .filter(|summary| summary.info.asset.is_some() && !split)
            .cloned();
        #[cfg(not(feature = "silvestris-ui"))]
        let gauge_height = if loaded.is_some() {
            layout::gauge_height(mode, plan.main)
        } else {
            0.0
        };
        #[cfg(feature = "silvestris-ui")]
        let gauge_height = if loaded.is_some() && self.gauges_expanded {
            shared_shell::TELEMETRY_DETAILS_HEIGHT
        } else {
            0.0
        };
        let stage = self.render_stage(plan, gauge_height, viewport_height, window, cx);
        #[cfg(not(feature = "silvestris-ui"))]
        let gauges = loaded.map(|summary| self.render_gauges(mode, plan.main, &summary, cx));
        #[cfg(feature = "silvestris-ui")]
        let gauges = loaded
            .filter(|_| self.gauges_expanded)
            .map(|summary| self.render_telemetry_details(&summary, cx));
        #[cfg(not(feature = "silvestris-ui"))]
        let (legacy_toolbar, legacy_load, legacy_actions) = (
            Some(self.render_toolbar(layout::toolbar_density(plan.main), cx)),
            (self.menu == Menu::Load).then(|| self.render_load_menu(cx)),
            (self.menu == Menu::Toolbar).then(|| self.render_toolbar_menu(cx)),
        );
        #[cfg(feature = "silvestris-ui")]
        let (legacy_toolbar, legacy_load, legacy_actions): (Option<gpui::Empty>, _, _) =
            (None, None::<gpui::Empty>, None::<gpui::Empty>);
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(plan.main.max(0.0)))
            .min_w_0()
            .h_full()
            .children(legacy_toolbar)
            .children(self.render_error(cx))
            .child(self.render_drop_stage(stage, cx))
            .children(gauges)
            .children(legacy_load)
            .children(legacy_actions)
            .into_any_element()
    }
}

impl Render for LotusWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::palette(cx);
        if let Some(controller) = &self.controller {
            controller.ui.rendered();
        }
        if !cx.has_active_drag() {
            // A drag released outside every drop target, or cancelled.
            self.drop_target = None;
            self.ratio_drag = None;
        }
        #[cfg(not(feature = "silvestris-ui"))]
        let content = {
            let plan = self.plan(_window);
            let height = f32::from(_window.viewport_size().height);
            let main = self.render_workspace(&plan, height, _window, cx);
            let sidebar = self.render_sidebar(&plan, cx);
            div()
                .flex()
                .size_full()
                .child(self.render_drop_sidebar(sidebar, cx))
                .child(main)
                .when_some(plan.inspector, |root, width| {
                    root.child(self.render_inspector(width, true, cx))
                })
                .when(plan.inspector_rail, |root| {
                    root.child(self.render_inspector_rail(&plan, cx))
                })
                .children(
                    self.inspect_tile
                        .clone()
                        .filter(|_| self.split())
                        .map(|session| self.render_tile_inspector(&plan, &session, cx)),
                )
                .into_any_element()
        };
        #[cfg(feature = "silvestris-ui")]
        self.sync_shared_commands(_window, cx);
        #[cfg(feature = "silvestris-ui")]
        let content = self.render_shared_window(_window, cx);
        #[cfg(feature = "silvestris-ui")]
        let menu_overlay = Some(gpui::deferred(self.shared_commands.menu.clone()).with_priority(1));
        #[cfg(not(feature = "silvestris-ui"))]
        let menu_overlay: Option<gpui::Empty> = None;
        #[cfg(feature = "silvestris-ui")]
        let confirmation = self
            .render_confirmation(cx)
            .map(|sheet| gpui::deferred(sheet).with_priority(2));
        #[cfg(not(feature = "silvestris-ui"))]
        let confirmation: Option<gpui::Empty> = None;
        let root = div()
            .id("lotus")
            .relative()
            .flex()
            .size_full()
            .overflow_hidden()
            .bg(theme.window)
            .text_color(theme.text)
            .font_family(theme.font)
            .text_size(px(theme.ui_size))
            .track_focus(&self.focus)
            .when(self.drag.is_some(), |root| root.cursor_col_resize())
            .on_drop(cx.listener(|view, _: &DividerDrag, _, _| {
                if let (Some(ratio), true) = (view.ratio_drag.take(), view.split()) {
                    view.send(Command::Workspace(Operation::SetRatio { ratio }));
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _, window, cx| window.focus(&view.focus, cx)),
            )
            .on_mouse_move(cx.listener(|view, event: &MouseMoveEvent, window, cx| {
                if view.overlay_drag {
                    view.drag_overlay(event.position.x.into(), window);
                    cx.notify();
                    return;
                }
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
                if event.keystroke.modifiers.control && event.keystroke.key == "," {
                    view.open_settings(crate::settings_window::Target::Global, cx);
                    return;
                }
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
            .child(content)
            // Silvestris menus.md: mount once as a priority-1 deferred overlay.
            .children(menu_overlay)
            .children(confirmation);
        // Silvestris commands.md: registry shortcuts dispatch at this responder
        // boundary, ahead of the root's key_down (and so guest input).
        #[cfg(feature = "silvestris-ui")]
        return silvestris::command::CommandScope::new(
            "lotus-commands",
            shared_commands::COMMAND_CONTEXT,
            self.shared_commands.registry.clone(),
        )
        .child(root)
        .into_any_element();
        #[cfg(not(feature = "silvestris-ui"))]
        root.into_any_element()
    }
}

/// Largest scale at which both screens fit `width` x `height`, at least 0.5.
#[must_use]
/// Screens of one shown session and where its touch screen was last laid out.
#[derive(Default)]
struct TileView {
    images: Vec<Arc<RenderImage>>,
    epoch: u64,
    touch: Arc<Mutex<Option<TouchTransform>>>,
}

/// Replaced frames leave the sprite atlas immediately so streaming stays bounded.
fn drop_images(images: Vec<Arc<RenderImage>>, cx: &mut Context<LotusWindow>) {
    for old in images {
        cx.drop_image(old, None);
    }
}

#[cfg(feature = "silvestris-ui")]
fn tile_message(_: theme::Palette, text: &'static str) -> AnyElement {
    silvestris::controls::Notice::new(text, "").into_any_element()
}

#[cfg(not(feature = "silvestris-ui"))]
fn tile_message(theme: theme::Palette, text: &'static str) -> AnyElement {
    div()
        .flex()
        .flex_1()
        .items_center()
        .justify_center()
        .text_color(theme.text_muted)
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

#[cfg(test)]
impl SplitPlan {
    /// Space each tile's gauges take on its outer edge, gap included: left of the first
    /// and right of the second tile, or above the first and below the second.
    fn gauge_edge(&self) -> f32 {
        gauge_edge(self.vertical)
    }
}

fn gauge_edge(vertical: bool) -> f32 {
    GAUGE_GAP + if vertical { GAUGE_ROW } else { GAUGE_COLUMN }
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
        // Each slot gives its outer edge to that console's gauges (lotus#70).
        let sizes = [along * ratio, along * (1.0 - ratio)]
            .map(|slot| (slot - gauge_edge(vertical)).max(0.0));
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

/// A controller shown in a tile's gauges.
#[derive(Debug, Clone, PartialEq)]
struct Paired {
    name: String,
    /// `None` when disconnected: the router remembers the assignment (lotus#62).
    device: Option<String>,
    connected: bool,
}

/// Controllers assigned to `session`, from the router's assignment state only: never
/// inferred from pad order, tile order or keyboard focus.
fn pairing(controllers: &crate::input_routing::Controllers, session: &str) -> Vec<Paired> {
    let connected = controllers
        .pads
        .iter()
        .filter(|pad| pad.session.as_deref() == Some(session))
        .map(|pad| Paired {
            name: pad.name.clone(),
            device: Some(pad.id.to_string()),
            connected: true,
        });
    let remembered = controllers
        .remembered
        .iter()
        .filter(|pad| pad.session == session)
        .map(|pad| Paired {
            name: pad.name.clone(),
            device: None,
            connected: false,
        });
    connected.chain(remembered).collect()
}

fn state_name(summary: &SessionSummary) -> &'static str {
    session_tree::session_status(summary)
}

fn state_colour(theme: theme::Palette, summary: &SessionSummary) -> Rgba {
    match state_name(summary) {
        "running" => theme.success,
        "paused" => theme.warning,
        "crashed" => theme.error,
        _ => theme.text_muted,
    }
}

/// Square dot for primaries, round for secondaries: shape, not only colour.
fn state_dot(theme: theme::Palette, summary: &SessionSummary, primary: bool) -> impl IntoElement {
    div()
        .flex_none()
        .size(px(8.0))
        .rounded(px(if primary { 2.0 } else { 4.0 }))
        .bg(state_colour(theme, summary))
}

#[cfg(not(feature = "silvestris-ui"))]
fn row_meta(summary: &SessionSummary) -> String {
    session_tree::session_detail(summary)
}

fn section_label(theme: theme::Palette, text: &'static str) -> impl IntoElement {
    div()
        .text_size(px(10.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.text_muted)
        .child(text)
}

#[cfg(not(feature = "silvestris-ui"))]
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

#[cfg(not(feature = "silvestris-ui"))]
fn tone_colour(theme: theme::Palette, tone: Tone) -> Rgba {
    match tone {
        Tone::Primary => theme.primary_text,
        Tone::Plain => theme.text,
        Tone::Danger => theme.error,
    }
}

/// Bordered (or filled, for `Primary`) control; keyboard focusable.
#[cfg(not(feature = "silvestris-ui"))]
fn control(
    theme: theme::Palette,
    id: impl Into<SharedString>,
    tone: Tone,
    enabled: bool,
    height: f32,
) -> Stateful<Div> {
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
        .text_color(tone_colour(theme, tone))
        .focus_visible(|style| style.border_color(theme.info));
    let base = match tone {
        Tone::Primary => base
            .bg(theme.primary_fill)
            .border_1()
            .border_color(theme.primary_fill),
        Tone::Plain | Tone::Danger => base.border_1().border_color(theme.border),
    };
    if enabled {
        base.cursor_pointer().hover(move |style| match tone {
            Tone::Primary => style.opacity(0.88),
            Tone::Plain | Tone::Danger => style
                .bg(theme.surface_hover)
                .border_color(theme.border_strong),
        })
    } else {
        base.opacity(0.4)
    }
}

#[cfg(feature = "silvestris-ui")]
fn workspace_button(
    theme: theme::Palette,
    id: impl Into<SharedString>,
    label: &'static str,
    tone: Tone,
    enabled: bool,
) -> CompactButton {
    small_button(theme, id, label, tone, enabled).size(silvestris::controls::ControlSize::Regular)
}

#[cfg(not(feature = "silvestris-ui"))]
use button as workspace_button;

#[cfg(not(feature = "silvestris-ui"))]
fn button(
    theme: theme::Palette,
    id: impl Into<SharedString>,
    label: &'static str,
    tone: Tone,
    enabled: bool,
) -> Stateful<Div> {
    control(theme, id, tone, enabled, 30.0)
        .px_3()
        .text_sm()
        .child(label)
}

/// Inspector context command: shorter than toolbar buttons.
#[cfg(feature = "silvestris-ui")]
type CompactButton = silvestris::controls::Button;
#[cfg(not(feature = "silvestris-ui"))]
type CompactButton = Stateful<Div>;

fn compact_tooltip(button: CompactButton, text: impl Into<SharedString>) -> CompactButton {
    #[cfg(feature = "silvestris-ui")]
    return button.tooltip(text);
    #[cfg(not(feature = "silvestris-ui"))]
    with_tooltip(button, text)
}

#[cfg(feature = "silvestris-ui")]
fn small_button(
    _: theme::Palette,
    id: impl Into<SharedString>,
    label: &'static str,
    tone: Tone,
    enabled: bool,
) -> CompactButton {
    CompactButton::new(id.into(), label)
        .size(silvestris::controls::ControlSize::Small)
        .default_button(matches!(tone, Tone::Primary))
        .destructive(matches!(tone, Tone::Danger))
        .disabled(!enabled)
}

#[cfg(not(feature = "silvestris-ui"))]
fn small_button(
    theme: theme::Palette,
    id: impl Into<SharedString>,
    label: &'static str,
    tone: Tone,
    enabled: bool,
) -> Stateful<Div> {
    control(theme, id, tone, enabled, 26.0)
        .px_2()
        .text_xs()
        .child(label)
}

/// Square icon control for the condensed toolbar.
#[cfg(not(feature = "silvestris-ui"))]
fn icon_tool(theme: theme::Palette, id: impl Into<SharedString>, glyph: Icon) -> Stateful<Div> {
    control(theme, id, Tone::Plain, true, 30.0)
        .min_w(px(30.0))
        .px(px(7.0))
        .child(icon(glyph, 16.0, theme.text))
}

/// Toolbar action: labelled at full width, otherwise an icon with a tooltip.
#[allow(
    clippy::too_many_arguments,
    reason = "Explicit palette added to the legacy toolbar composer; shared command migration is tracked in #133."
)]
#[cfg(not(feature = "silvestris-ui"))]
fn tool_button(
    theme: theme::Palette,
    id: &'static str,
    label: &'static str,
    glyph: Icon,
    description: &'static str,
    labelled: bool,
    enabled: bool,
    tone: Tone,
) -> Stateful<Div> {
    if labelled {
        return button(theme, id, label, tone, enabled);
    }
    with_tooltip(
        control(theme, id, tone, enabled, 30.0)
            .w(px(30.0))
            .child(icon(glyph, 16.0, tone_colour(theme, tone))),
        description,
    )
}

/// Inspector rail entry. The current view gets a surface, a stronger icon and
/// a bar on the outer edge, so it does not depend on colour alone.
#[cfg(not(feature = "silvestris-ui"))]
fn rail_button(
    theme: theme::Palette,
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
        .focus_visible(|style| style.border_color(theme.info))
        .child(icon(
            glyph,
            16.0,
            if current {
                theme.text
            } else {
                theme.text_secondary
            },
        ))
        .when(current, |button| {
            button.bg(theme.surface_active).child(
                div()
                    .absolute()
                    .left(px(-5.0))
                    .top(px(7.0))
                    .h(px(16.0))
                    .w(px(3.0))
                    .rounded(px(1.0))
                    .bg(theme.text),
            )
        })
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(|style| style.bg(theme.surface_hover))
        })
        .when(!enabled, |button| button.opacity(0.4));
    with_tooltip(button, description)
}

/// What follows the pointer during a drag: the session's name, or nothing for the divider.
struct DragGhost(SharedString);

impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::palette(cx);
        if self.0.is_empty() {
            return div();
        }
        div()
            .px_2()
            .py_1()
            .rounded(px(4.0))
            .bg(theme.panel)
            .border_1()
            .border_color(theme.info)
            .font_family(theme.font)
            .text_xs()
            .text_color(theme.text)
            .child(self.0.clone())
    }
}

/// Small text tooltip; GPUI tooltips are views.
#[cfg(not(feature = "silvestris-ui"))]
struct Tooltip(SharedString);

#[cfg(not(feature = "silvestris-ui"))]
impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::palette(cx);
        div()
            .max_w(px(360.0))
            .px_2()
            .py_1()
            .rounded(px(4.0))
            .bg(theme.panel)
            .border_1()
            .border_color(theme.border_strong)
            .font_family(theme.font)
            .text_xs()
            .text_color(theme.text)
            .child(self.0.clone())
    }
}

#[cfg(feature = "silvestris-ui")]
pub(crate) use silvestris::controls::tooltip::attach as with_tooltip;

#[cfg(not(feature = "silvestris-ui"))]
pub(crate) fn with_tooltip(element: Stateful<Div>, text: impl Into<SharedString>) -> Stateful<Div> {
    let text = text.into();
    element.tooltip(move |_, cx| cx.new(|_| Tooltip(text.clone())).into())
}

#[cfg(feature = "silvestris-ui")]
fn icon_button(
    _: theme::Palette,
    id: impl Into<SharedString>,
    glyph: Icon,
    description: impl Into<SharedString>,
) -> CompactButton {
    let description = description.into();
    CompactButton::new(id.into(), description.clone())
        .style(silvestris::controls::ButtonStyle::Gradient)
        .icon(move |ink| icon(glyph, 12.0, ink))
        .tooltip(description)
}

#[cfg(not(feature = "silvestris-ui"))]
fn icon_button(
    theme: theme::Palette,
    id: impl Into<SharedString>,
    glyph: Icon,
    description: impl Into<SharedString>,
) -> Stateful<Div> {
    let description: SharedString = description.into();
    let button = div()
        .id(id.into())
        .role(gpui::Role::Button)
        .aria_label(description.clone())
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
        .hover(|style| style.bg(theme.surface_active))
        .focus_visible(|style| style.border_color(theme.info))
        .child(icon(glyph, 14.0, theme.text_secondary));
    with_tooltip(button, description)
}

#[cfg(not(feature = "silvestris-ui"))]
fn popover(theme: theme::Palette, id: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .absolute()
        .flex()
        .flex_col()
        .p_1()
        .rounded(px(8.0))
        .bg(theme.panel)
        .border_1()
        .border_color(theme.border_strong)
        .shadow_lg()
        .occlude()
}

#[cfg(not(feature = "silvestris-ui"))]
fn menu_heading(theme: theme::Palette, text: &'static str) -> impl IntoElement {
    div().px_2().pt_2().pb_1().child(section_label(theme, text))
}

#[cfg(not(feature = "silvestris-ui"))]
fn menu_item(
    theme: theme::Palette,
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
        .hover(|style| style.bg(theme.surface_active))
        .focus_visible(|style| style.border_color(theme.info))
        .child(div().truncate().child(label.into()))
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(theme.text_muted)
                .child(detail.into()),
        )
}

/// Menu row with a check box drawn as a shape, so state is not colour alone.
#[cfg(not(feature = "silvestris-ui"))]
fn checkbox_item(
    theme: theme::Palette,
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
        .focus_visible(|style| style.border_color(theme.info))
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .size(px(14.0))
                .rounded(px(3.0))
                .border_1()
                .border_color(theme.border_strong)
                .when(checked, |box_| {
                    box_.bg(theme.text)
                        .child(div().size(px(6.0)).rounded(px(1.0)).bg(theme.window))
                }),
        )
        .child(div().flex_1().truncate().child(label));
    if enabled {
        item = item
            .cursor_pointer()
            .hover(|style| style.bg(theme.surface_active));
    } else {
        item = item.opacity(0.4);
    }
    item
}

#[cfg(feature = "silvestris-ui")]
fn empty_state(
    _: theme::Palette,
    title: &'static str,
    body: String,
) -> silvestris::controls::Notice {
    silvestris::controls::Notice::new(title, body)
}

#[cfg(not(feature = "silvestris-ui"))]
fn empty_state(theme: theme::Palette, title: &'static str, body: String) -> Div {
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
                .text_color(theme.text_muted)
                .child(body),
        )
}

fn screen_frame(theme: theme::Palette, width: f32, height: f32) -> Div {
    div()
        .relative()
        .flex_none()
        .w(px(width))
        .h(px(height))
        .bg(theme.canvas)
        .border_1()
        .border_color(theme.border)
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
        let total = off.tiles[0].width + off.tiles[1].width + 2.0 * off.gauge_edge() + TILE_GAP;
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
                        let gauges = 2.0 * plan.gauge_edge();
                        assert!(along + gauges + TILE_GAP <= room + 1e-3, "{case}");
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
    fn tile_pairing_comes_from_assignments_only() {
        use lotus_input::router::{PadStatus, Remembered};
        use lotus_input::DeviceId;
        let pad = |id, name: &str, session: Option<&str>| PadStatus {
            id: DeviceId(id),
            name: name.into(),
            model_uuid: String::new(),
            session: session.map(str::to_string),
        };
        let controllers = crate::input_routing::Controllers {
            // Listed out of tile order; one unassigned pad.
            pads: vec![
                pad(2, "DualSense", Some("b")),
                pad(7, "Spare", None),
                pad(1, "Xbox Wireless", Some("a")),
            ],
            remembered: vec![Remembered {
                name: "Pro Controller".into(),
                model_uuid: String::new(),
                session: "b".into(),
            }],
            ..Default::default()
        };
        let a = pairing(&controllers, "a");
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].name, "Xbox Wireless");
        assert_eq!(a[0].device.as_deref(), Some("pad-1"));
        let b = pairing(&controllers, "b");
        assert_eq!(
            b.iter()
                .map(|p| (p.name.as_str(), p.connected))
                .collect::<Vec<_>>(),
            [("DualSense", true), ("Pro Controller", false)]
        );
        assert!(pairing(&controllers, "c").is_empty());
    }

    #[test]
    fn split_reserves_each_tiles_outer_gauge_edge() {
        let auto = [workspace::ScreenLayout::Auto; 2];
        let side = split_plan(1600.0, 800.0, Axis::Horizontal, 0.5, auto, None);
        let slot = (1600.0 - TILE_GAP) / 2.0;
        assert!((side.tiles[0].width - (slot - GAUGE_COLUMN - GAUGE_GAP)).abs() < 1e-3);
        assert!(
            (side.tiles[0].height - 800.0).abs() < 1e-3,
            "row keeps full height"
        );
        let stacked = split_plan(1600.0, 1200.0, Axis::Vertical, 0.5, auto, None);
        let slot = (1200.0 - TILE_GAP) / 2.0;
        assert!((stacked.tiles[1].height - (slot - GAUGE_ROW - GAUGE_GAP)).abs() < 1e-3);
        assert!((stacked.tiles[1].width - 1600.0).abs() < 1e-3);
    }

    #[test]
    fn registers_render_as_fixed_width_hex() {
        assert_eq!(hex_string(&serde_json::json!(0x1f)), "0000001f");
        assert_eq!(hex_string(&Value::Null), "—");
    }
}
