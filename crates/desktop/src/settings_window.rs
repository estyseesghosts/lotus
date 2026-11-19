//! The one Settings window (lotus#53): a separate, resizable, non-modal GPUI window over the
//! settings registry (`lotus-settings`, lotus#50) and the layered stores (lotus#51).
//!
//! - One per process. [`open`] focuses the existing window and switches its target; the
//!   gear, Ctrl+, and the card actions in the sidebar all call it.
//! - Targets are the saved global defaults and one card's saved overrides
//!   ([`Target`]). A card is edited whether or not its primary runs. Secondary sessions
//!   have no saved settings and are not a target.
//! - Rows come from the registry for the target's scope. Worker and input settings are read
//!   and written through `Supervisor::settings` and `Supervisor::set_setting`; desktop
//!   preferences through the main window's `desktop.json` store. Host settings are shown
//!   read-only (they live in the host JSON).
//! - Editing here never sends input to a guest: the window has its own focus, and the main
//!   window only maps keys while its screens are focused.

use std::sync::Arc;

#[cfg(not(feature = "silvestris-ui"))]
use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, App, AppContext, Bounds, ClickEvent, Context, FocusHandle, FontWeight,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render, SharedString, Styled,
    WeakEntity, Window, WindowHandle,
};
#[cfg(not(feature = "silvestris-ui"))]
use gpui::{Stateful, StatefulInteractiveElement};
use lotus_session::supervisor::{SettingsTarget, Supervisor};
use lotus_session::telemetry::Actor;
use lotus_settings::{Availability, Descriptor, Group, Kind, Owner, Registry, Scope};
use serde_json::Value;

#[cfg(not(feature = "silvestris-ui"))]
use crate::icons::{icon, Icon};
use crate::lotus_window::{with_tooltip, LotusWindow};
use crate::theme;
#[cfg(not(feature = "silvestris-ui"))]
use gpui::{TitlebarOptions, WindowBounds, WindowOptions};

#[cfg(feature = "silvestris-ui")]
mod shared;

/// `desktop.json` key holding this window's last bounds; window state, not shown as a row.
pub const BOUNDS_KEY: &str = "lotus.desktop.settings_bounds";
#[cfg(not(feature = "silvestris-ui"))]
const DEFAULT_SIZE: (f32, f32) = (860.0, 620.0);
const MIN_SIZE: (f32, f32) = (640.0, 420.0);

/// What the window edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Global,
    /// One card's saved overrides; its primary may be stopped.
    Card(String),
}

impl Target {
    fn scope(&self) -> Scope {
        match self {
            Target::Global => Scope::Global,
            Target::Card(_) => Scope::Primary,
        }
    }

    fn settings_target(&self) -> SettingsTarget {
        match self {
            Target::Global => SettingsTarget::Global,
            Target::Card(card) => SettingsTarget::Card { card: card.clone() },
        }
    }

    #[cfg(not(feature = "silvestris-ui"))]
    fn title(&self) -> String {
        match self {
            Target::Global => "Global".into(),
            Target::Card(card) => format!("Primary: {card}"),
        }
    }

    /// The `source` a value saved at this target reports.
    fn layer(&self) -> &'static str {
        match self {
            Target::Global => "global",
            Target::Card(_) => "card",
        }
    }
}

/// The open Settings window, if any.
struct Opened(WindowHandle<SettingsWindow>);

impl gpui::Global for Opened {}

/// Open the Settings window on `target`, or focus the open one and switch it there.
pub fn open(
    cx: &mut App,
    lotus: WeakEntity<LotusWindow>,
    host: Option<Arc<Supervisor>>,
    control: bool,
    target: Target,
) {
    if let Some(Opened(handle)) = cx.try_global::<Opened>() {
        let handle = *handle;
        let switched = handle.update(cx, |view, window, cx| {
            view.control = control;
            view.set_target(target.clone(), cx);
            window.activate_window();
        });
        if switched.is_ok() {
            return;
        }
    }
    let saved = lotus
        .upgrade()
        .and_then(|view| bounds_from(&view.read(cx).preference(BOUNDS_KEY)));
    #[cfg(not(feature = "silvestris-ui"))]
    let bounds = saved.unwrap_or_else(|| {
        Bounds::centered(None, gpui::size(px(DEFAULT_SIZE.0), px(DEFAULT_SIZE.1)), cx)
    });
    // HIG p. 244: preferences windows are not resizable; only the saved
    // origin is reused, and the pane sets the size (settings_window/shared.rs).
    #[cfg(feature = "silvestris-ui")]
    let options = {
        let size = gpui::size(px(silvestris::preferences::WINDOW_WIDTH), px(480.0));
        let bounds = saved
            .map(|b| Bounds::new(b.origin, size))
            .unwrap_or_else(|| Bounds::centered(None, size, cx));
        silvestris::chrome::fixed_window_options(shared::OVERVIEW_TITLE, bounds, cx)
    };
    #[cfg(not(feature = "silvestris-ui"))]
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("LOTUS Settings".into()),
            ..Default::default()
        }),
        window_min_size: Some(gpui::size(px(MIN_SIZE.0), px(MIN_SIZE.1))),
        ..Default::default()
    };
    let build = |window: &mut Window, cx: &mut App| {
        cx.new(|cx| {
            let view = SettingsWindow::new(lotus.clone(), host, control, target, window, cx);
            let remember = lotus.clone();
            window.on_window_should_close(cx, move |window, cx| {
                closing(&remember, window, cx);
                true
            });
            view
        })
    };
    #[cfg(feature = "silvestris-ui")]
    let opened = silvestris::chrome::open_window(options, cx, build);
    #[cfg(not(feature = "silvestris-ui"))]
    let opened = cx.open_window(options, build);
    match opened {
        Ok(handle) => {
            let _ = handle.update(cx, |view, window, cx| window.focus(&view.focus, cx));
            cx.set_global(Opened(handle));
        }
        Err(error) => tracing::warn!(%error, "cannot open the Settings window"),
    }
}

/// Close the Settings window; the main window calls this when it closes.
pub fn close(cx: &mut App) {
    if let Some(Opened(handle)) = cx.try_global::<Opened>() {
        let handle = *handle;
        let _ = handle.update(cx, |view, window, cx| {
            closing(&view.lotus, window, cx);
            window.remove_window();
        });
    }
}

/// Every way the window closes (title bar, Esc, the main window closing) saves its bounds
/// and forgets the handle. `remove_window` skips `on_window_should_close`, so the Esc and
/// main-window paths call this themselves.
fn closing(lotus: &WeakEntity<LotusWindow>, window: &Window, cx: &mut App) {
    // "how a window should be opened after it has been closed" (gpui window.rs
    // `window_bounds`); `bounds()` drifts by the title bar on Windows.
    let bounds = window.window_bounds().get_bounds();
    let value = serde_json::json!([
        f32::from(bounds.origin.x),
        f32::from(bounds.origin.y),
        f32::from(bounds.size.width),
        f32::from(bounds.size.height),
    ]);
    let _ = lotus.update(cx, |view, cx| view.set_preference(BOUNDS_KEY, value, cx));
    cx.remove_global::<Opened>();
}

fn bounds_from(value: &Value) -> Option<Bounds<gpui::Pixels>> {
    let list = value.as_array()?;
    let [x, y, w, h] = [0, 1, 2, 3].map(|i| list.get(i).and_then(Value::as_f64));
    let (x, y, w, h) = (x? as f32, y? as f32, w? as f32, h? as f32);
    (w >= MIN_SIZE.0 && h >= MIN_SIZE.1)
        .then(|| Bounds::new(gpui::point(px(x), px(y)), gpui::size(px(w), px(h))))
}

pub struct SettingsWindow {
    lotus: WeakEntity<LotusWindow>,
    host: Option<Arc<Supervisor>>,
    control: bool,
    target: Target,
    group: Group,
    /// `Supervisor::settings` for `target`; `None` until loaded or without a host.
    report: Option<Value>,
    cards: Vec<String>,
    error: Option<String>,
    /// Bumped per target switch, so a late report for the previous target is dropped.
    generation: u64,
    focus: FocusHandle,
    #[cfg(feature = "silvestris-ui")]
    prefs: shared::PreferencesState,
}

impl SettingsWindow {
    fn new(
        lotus: WeakEntity<LotusWindow>,
        host: Option<Arc<Supervisor>>,
        control: bool,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Reload on any settings change in this process (the window's own edits, MCP,
        // presets: `Supervisor::settings_revision`, checked at the router's 250 ms) and on
        // activation, which picks up edits another process (`ctr-cli`) saved meanwhile.
        if let Some(host) = host.clone() {
            cx.spawn(async move |this, cx| {
                let mut seen = host.settings_revision();
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(250))
                        .await;
                    let now = host.settings_revision();
                    if now != seen {
                        seen = now;
                        if this.update(cx, |view, cx| view.refresh(cx)).is_err() {
                            break;
                        }
                    } else if this.upgrade().is_none() {
                        break;
                    }
                }
            })
            .detach();
        }
        cx.observe_window_activation(window, |view, window, cx| {
            if window.is_window_active() {
                view.refresh(cx);
            }
        })
        .detach();
        let mut view = Self {
            lotus,
            host,
            control,
            target,
            group: Group::General,
            report: None,
            cards: Vec::new(),
            error: None,
            generation: 0,
            focus: cx.focus_handle(),
            #[cfg(feature = "silvestris-ui")]
            prefs: shared::PreferencesState::default(),
        };
        view.refresh(cx);
        view
    }

    fn set_target(&mut self, target: Target, cx: &mut Context<Self>) {
        if self.target != target {
            self.target = target;
            #[cfg(feature = "silvestris-ui")]
            self.prefs.fields.clear();
            self.report = None;
            self.error = None;
        }
        self.refresh(cx);
    }

    /// Reload the target's report and the card list.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        let Some(host) = self.host.clone() else {
            cx.notify();
            return;
        };
        let target = self.target.settings_target();
        let loading = cx.background_spawn(async move {
            let report = host.settings(&target).await;
            let cards = host.cards();
            (report, cards)
        });
        cx.spawn(async move |this, cx| {
            let (report, cards) = loading.await;
            let _ = this.update(cx, |view, cx| {
                if view.generation != generation {
                    return;
                }
                match report {
                    Ok(report) => view.report = Some(report),
                    Err(error) => view.error = Some(error.to_string()),
                }
                if let Ok(cards) = cards {
                    view.cards = cards;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Save (`Some`) or remove (`None`) one value at the current target.
    fn write(&mut self, descriptor: &Descriptor, value: Option<Value>, cx: &mut Context<Self>) {
        if descriptor.owner == Owner::Desktop {
            self.error = None;
            if let Some(value) = value.or_else(|| Some(descriptor.default.clone())) {
                let key = descriptor.key.clone();
                let _ = self
                    .lotus
                    .update(cx, |view, cx| view.set_preference(&key, value, cx));
            }
            cx.notify();
            return;
        }
        let Some(host) = self.host.clone() else {
            return;
        };
        let ui_target = self.target.clone();
        let target = ui_target.settings_target();
        let key = descriptor.key.clone();
        let saving = cx.background_spawn(async move {
            host.set_setting(&target, &key, value, Actor::Desktop).await
        });
        cx.spawn(async move |this, cx| {
            let result = saving.await;
            let _ = this.update(cx, |view, cx| {
                if view.target == ui_target {
                    view.error = result.err().map(|error| error.to_string());
                    view.refresh(cx);
                }
            });
        })
        .detach();
    }

    /// Settings of `group` shown for the current target.
    fn rows(&self, group: Group) -> Vec<&'static Descriptor> {
        let scope = self.target.scope();
        Registry::builtin()
            .descriptors()
            .iter()
            .filter(|d| d.group == group && d.scopes.allows(scope) && d.key != BOUNDS_KEY)
            .filter(|d| !matches!(d.availability, Availability::Excluded { .. }))
            .collect()
    }

    /// Effective value and its source for one row.
    fn value(&self, descriptor: &Descriptor, cx: &App) -> (Value, String) {
        match descriptor.owner {
            Owner::Desktop => {
                let value = self
                    .lotus
                    .upgrade()
                    .map(|view| view.read(cx).preference(&descriptor.key))
                    .unwrap_or_else(|| descriptor.default.clone());
                let source = if value == descriptor.default {
                    "default"
                } else {
                    "desktop.json"
                };
                (value, source.into())
            }
            Owner::Host => (
                self.host
                    .as_ref()
                    .and_then(|host| host_value(host, &descriptor.key))
                    .unwrap_or_else(|| descriptor.default.clone()),
                "host.json".into(),
            ),
            Owner::Azahar | Owner::Input => {
                let entry = self
                    .report
                    .as_ref()
                    .map(|report| &report["settings"][&descriptor.key]);
                match entry {
                    Some(entry) if !entry.is_null() => (
                        entry["value"].clone(),
                        entry["source"].as_str().unwrap_or("default").to_string(),
                    ),
                    _ => (descriptor.default.clone(), "default".into()),
                }
            }
        }
    }

    fn editable(&self, descriptor: &Descriptor, source: &str) -> Result<(), &'static str> {
        match descriptor.owner {
            Owner::Desktop => Ok(()),
            Owner::Host => {
                Err("Set in the host configuration (host.json); read when the host starts.")
            }
            Owner::Azahar | Owner::Input => {
                if self.host.is_none() {
                    Err("No host configuration: start LOTUS with a host.json to change this.")
                } else if !self.control {
                    Err("This host is read-only (allow_control is off).")
                } else if source == "managed" {
                    Err("Managed by the host configuration profile.")
                } else {
                    Ok(())
                }
            }
        }
    }

    #[cfg(not(feature = "silvestris-ui"))]
    fn render_nav(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::palette(cx);
        let mut nav = div()
            .id("settings-nav")
            .flex()
            .flex_col()
            .flex_none()
            .w(px(210.0))
            .h_full()
            .overflow_y_scroll()
            .p_2()
            .gap(px(2.0))
            .bg(theme.panel)
            .border_r_1()
            .border_color(theme.border)
            .child(heading(theme, "SCOPE"));
        let targets = std::iter::once(Target::Global)
            .chain(self.cards.iter().cloned().map(Target::Card))
            .collect::<Vec<_>>();
        for target in targets {
            let selected = target == self.target;
            let label = target.title();
            let id = SharedString::from(format!("scope-{label}"));
            nav = nav.child(nav_item(theme, id, label, None, selected).on_click(
                cx.listener(move |view, _: &ClickEvent, _, cx| view.set_target(target.clone(), cx)),
            ));
        }
        nav = nav
            .child(div().h(px(8.0)))
            .child(heading(theme, "CATEGORY"));
        for group in Group::ALL {
            let count = self.rows(group).len();
            if count == 0 {
                continue;
            }
            let selected = group == self.group;
            nav = nav.child(
                nav_item(
                    theme,
                    SharedString::from(format!("group-{group:?}")),
                    group.label().to_string(),
                    Some(count.to_string()),
                    selected,
                )
                .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                    view.group = group;
                    cx.notify();
                })),
            );
        }
        nav
    }

    #[cfg(not(feature = "silvestris-ui"))]
    fn render_row(
        &self,
        descriptor: &'static Descriptor,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        let theme = theme::palette(cx);
        let (value, source) = self.value(descriptor, cx);
        let editable = self.editable(descriptor, &source);
        let overridden = match descriptor.owner {
            Owner::Desktop => value != descriptor.default,
            _ => source == self.target.layer(),
        };
        let key = descriptor.key.clone();
        let mut controls = div().flex().items_center().gap_1().flex_none();
        match (&descriptor.kind, editable) {
            (Kind::Boolean, Ok(())) => {
                let on = value.as_bool().unwrap_or(false);
                #[cfg(feature = "silvestris-ui")]
                let toggle = silvestris::controls::Checkbox::new(
                    SharedString::from(format!("toggle-{key}")),
                    descriptor.label.clone(),
                )
                .checked(on)
                .show_label(false)
                .on_change(cx.listener(
                    move |view, state: &silvestris::controls::CheckState, _, cx| {
                        view.write(
                            descriptor,
                            Some(Value::Bool(*state == silvestris::controls::CheckState::On)),
                            cx,
                        );
                    },
                ));
                #[cfg(not(feature = "silvestris-ui"))]
                let toggle = toggle(theme, format!("toggle-{key}"), on).on_click(cx.listener(
                    move |view, _: &ClickEvent, _, cx| {
                        view.write(descriptor, Some(Value::Bool(!on)), cx);
                    },
                ));
                controls = controls.child(toggle);
            }
            (kind, Ok(())) if choices(kind).is_some() => {
                let options = choices(kind).unwrap_or_default();
                let index = options.iter().position(|o| *o == value);
                for (step, glyph, label) in [
                    (-1isize, Icon::ChevronLeft, "Previous"),
                    (1, Icon::ChevronRight, "Next"),
                ] {
                    let options = options.clone();
                    controls = controls.child(
                        small_button(theme, format!("{label}-{key}"), glyph, label).on_click(
                            cx.listener(move |view, _: &ClickEvent, _, cx| {
                                let len = options.len() as isize;
                                let next = index.map_or(0, |i| (i as isize + step).rem_euclid(len));
                                view.write(descriptor, Some(options[next as usize].clone()), cx);
                            }),
                        ),
                    );
                }
                controls = controls.child(value_text(theme, display(&value)));
            }
            (kind, Ok(())) if stepper(kind).is_some() => {
                let (step, min, max, integer) = stepper(kind).unwrap_or((1.0, None, None, true));
                let current = value.as_f64().unwrap_or(0.0);
                for (sign, glyph, label) in [
                    (-1.0, Icon::ChevronDown, "Decrease"),
                    (1.0, Icon::ChevronUp, "Increase"),
                ] {
                    controls = controls.child(
                        small_button(theme, format!("{label}-{key}"), glyph, label).on_click(
                            cx.listener(move |view, _: &ClickEvent, _, cx| {
                                let mut next = current + sign * step;
                                next = (next / step).round() * step;
                                if let Some(min) = min {
                                    next = next.max(min);
                                }
                                if let Some(max) = max {
                                    next = next.min(max);
                                }
                                let next = if integer {
                                    Value::from(next.round() as i64)
                                } else {
                                    // Keep two significant decimals of the step.
                                    Value::from((next * 1e4).round() / 1e4)
                                };
                                view.write(descriptor, Some(next), cx);
                            }),
                        ),
                    );
                }
                controls = controls.child(value_text(theme, display(&value)));
            }
            (_, Ok(())) => {
                controls = controls.child(with_tooltip(
                    div()
                        .id(SharedString::from(format!("text-{key}")))
                        .child(value_text(theme, display(&value))),
                    "Text and list values are edited with ctr-cli or MCP for now (#54).",
                ));
            }
            (_, Err(reason)) => {
                #[cfg(feature = "silvestris-ui")]
                let value = if descriptor.kind == Kind::Boolean {
                    silvestris::controls::Checkbox::new(
                        SharedString::from(format!("toggle-{key}")),
                        descriptor.label.clone(),
                    )
                    .checked(value.as_bool().unwrap_or(false))
                    .show_label(false)
                    .disabled(true)
                    .into_any_element()
                } else {
                    div()
                        .opacity(0.6)
                        .child(value_text(theme, display(&value)))
                        .into_any_element()
                };
                #[cfg(not(feature = "silvestris-ui"))]
                let value = div()
                    .opacity(0.6)
                    .child(value_text(theme, display(&value)))
                    .into_any_element();
                controls = controls.child(with_tooltip(
                    div()
                        .id(SharedString::from(format!("locked-{key}")))
                        .child(value),
                    reason,
                ));
            }
        }
        if overridden && editable.is_ok() {
            #[cfg(feature = "silvestris-ui")]
            let reset = silvestris::controls::Button::new(
                SharedString::from(format!("reset-{key}")),
                reset_label(&self.target, descriptor),
            )
            .size(silvestris::controls::ControlSize::Small)
            .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                view.write(descriptor, None, cx);
            }))
            .into_any_element();
            #[cfg(not(feature = "silvestris-ui"))]
            let reset = small_button(
                theme,
                format!("reset-{key}"),
                Icon::Reset,
                reset_label(&self.target, descriptor),
            )
            .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                view.write(descriptor, None, cx);
            }))
            .into_any_element();
            controls = controls.child(reset);
        }
        div()
            .id(SharedString::from(format!("row-{key}")))
            .flex()
            .items_center()
            .gap_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().truncate().child(descriptor.label.clone()))
                            .child(source_badge(theme, &source, overridden)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.text_muted)
                            .child(descriptor.description.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.text_muted)
                            .opacity(0.7)
                            .child(descriptor.key.clone()),
                    ),
            )
            .child(controls)
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(feature = "silvestris-ui")]
        return self.render_preferences(_window, cx);
        #[cfg(not(feature = "silvestris-ui"))]
        self.render_legacy(cx)
    }
}

impl SettingsWindow {
    #[cfg(not(feature = "silvestris-ui"))]
    fn render_legacy(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = theme::palette(cx);
        if self.rows(self.group).is_empty() {
            if let Some(group) = Group::ALL.into_iter().find(|g| !self.rows(*g).is_empty()) {
                self.group = group;
            }
        }
        let rows: Vec<&'static Descriptor> = self.rows(self.group);
        let mut list = div()
            .id("settings-rows")
            .flex()
            .flex_col()
            .flex_1()
            .overflow_y_scroll()
            .px_4()
            .pb_4();
        for descriptor in rows {
            list = list.child(self.render_row(descriptor, cx));
        }
        let note = match &self.target {
            Target::Global => "Defaults for every card and new secondary session. Worker \
                settings apply to sessions started afterwards; input layouts apply at once."
                .to_string(),
            Target::Card(card) => format!(
                "Saved overrides of card {card}; unset values inherit Global. Worker \
                 settings apply when its primary next starts; input layouts apply at once."
            ),
        };
        let body = div()
            .id("settings-root")
            .track_focus(&self.focus)
            .key_context("Settings")
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    closing(&view.lotus, window, cx);
                    window.remove_window();
                }
            }))
            .size_full()
            .flex()
            .bg(theme.window)
            .text_color(theme.text)
            .text_size(px(theme.ui_size))
            .when(cfg!(feature = "silvestris-ui"), |root| root.font_family(theme.font))
            .child(self.render_nav(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .px_4()
                            .pt_4()
                            .pb_2()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(format!("{} · {}", self.group.label(), self.target.title())),
                            )
                            .child(div().text_xs().text_color(theme.text_muted).child(note))
                            .when_some(self.error.clone(), |header, error| {
                                header.child(
                                    div().text_xs().text_color(theme.error).child(error),
                                )
                            })
                            .when_some(
                                (self.group == Group::Discord)
                                    .then(|| self.lotus.read_with(cx, |l, _| l.presence_line(theme)).ok().flatten())
                                    .flatten(),
                                |header, (colour, text)| {
                                    header.child(div().text_xs().text_color(colour).child(text))
                                },
                            )
                            .when(
                                !self.lotus.read_with(cx, |l, _| l.preferences_saved()).unwrap_or(true),
                                |header| {
                                    header.child(div().text_xs().text_color(theme.warning).child(
                                        "desktop.json has no location or could not be read: preference changes are not saved and last until Lotus closes.",
                                    ))
                                },
                            )
                            .when(self.host.is_none(), |header| {
                                header.child(div().text_xs().text_color(theme.warning).child(
                                    "No host configuration: only desktop preferences can be changed.",
                                ))
                            }),
                    )
                    .child(list),
            );
        body.into_any_element()
    }
}

/// A Lotus host setting's current value from the running host, if it reports one.
fn host_value(host: &Supervisor, key: &str) -> Option<Value> {
    let config = serde_json::to_value(host.config()).ok()?;
    let mut value = &config;
    for part in key.strip_prefix("lotus.host.")?.split('.') {
        value = value.get(part)?;
    }
    Some(value.clone())
}

/// Allowed values of a choice setting.
fn choices(kind: &Kind) -> Option<Vec<Value>> {
    match kind {
        Kind::Integer {
            choices: Some(list),
            ..
        } => Some(list.iter().map(|c| Value::from(*c)).collect()),
        Kind::String {
            choices: Some(list),
            ..
        } => Some(list.iter().map(|c| Value::from(c.clone())).collect()),
        _ => None,
    }
}

/// Step, bounds and integer-ness of a numeric setting.
fn stepper(kind: &Kind) -> Option<(f64, Option<f64>, Option<f64>, bool)> {
    match kind {
        Kind::Integer {
            min,
            max,
            choices: None,
            ..
        } => Some((1.0, min.map(|v| v as f64), max.map(|v| v as f64), true)),
        Kind::Float { min, max } => {
            let step = match (min, max) {
                (Some(low), Some(high)) => nice_step((high - low) / 100.0),
                _ => 0.1,
            };
            Some((step, *min, *max, false))
        }
        _ => None,
    }
}

/// `raw` rounded up to 1, 2 or 5 times a power of ten.
fn nice_step(raw: f64) -> f64 {
    if raw <= 0.0 || !raw.is_finite() {
        return 0.1;
    }
    let power = 10f64.powf(raw.log10().floor());
    let mantissa = raw / power;
    let nice = if mantissa <= 1.0 {
        1.0
    } else if mantissa <= 2.0 {
        2.0
    } else if mantissa <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * power
}

fn display(value: &Value) -> String {
    match value {
        Value::Bool(true) => "On".into(),
        Value::Bool(false) => "Off".into(),
        Value::Null => "—".into(),
        Value::String(text) if text.is_empty() => "(empty)".into(),
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        other => {
            let text = other.to_string();
            if text.len() > 48 {
                format!("{}…", &text[..text.floor_char_boundary(47)])
            } else {
                text
            }
        }
    }
}

fn reset_label(target: &Target, descriptor: &Descriptor) -> &'static str {
    match (target, descriptor.owner) {
        (_, Owner::Desktop) => "Reset to default",
        (Target::Global, _) => "Reset to default",
        (Target::Card(_), _) => "Reset to Global",
    }
}

#[cfg(not(feature = "silvestris-ui"))]
fn heading(theme: theme::Palette, text: &'static str) -> impl IntoElement {
    div()
        .px_2()
        .pt_1()
        .pb_1()
        .text_size(px(10.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.text_muted)
        .child(text)
}

#[cfg(not(feature = "silvestris-ui"))]
fn nav_item(
    theme: theme::Palette,
    id: SharedString,
    label: String,
    detail: Option<String>,
    selected: bool,
) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .tab_index(0)
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .h(px(28.0))
        .px_2()
        .rounded(px(5.0))
        .border_1()
        .border_color(gpui::transparent_black())
        .cursor_pointer()
        .hover(|style| style.bg(theme.surface_active))
        .focus_visible(|style| style.border_color(theme.info))
        .when(selected, |item| item.bg(theme.surface_active))
        .child(div().truncate().child(label))
        .when_some(detail, |item, detail| {
            item.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.text_muted)
                    .child(detail),
            )
        })
}

#[cfg(not(feature = "silvestris-ui"))]
fn small_button(
    theme: theme::Palette,
    id: impl Into<SharedString>,
    glyph: Icon,
    description: impl Into<SharedString>,
) -> Stateful<gpui::Div> {
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
        .border_color(theme.border)
        .cursor_pointer()
        .hover(|style| style.bg(theme.surface_active))
        .focus_visible(|style| style.border_color(theme.info))
        .child(icon(glyph, 12.0, theme.text_secondary));
    with_tooltip(button, description)
}

/// A switch drawn as a shape with its state in text, so it is not colour alone.
#[cfg(not(feature = "silvestris-ui"))]
fn toggle(theme: theme::Palette, id: String, on: bool) -> Stateful<gpui::Div> {
    div()
        .id(SharedString::from(id))
        .tab_index(0)
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .h(px(24.0))
        .rounded(px(5.0))
        .border_1()
        .border_color(theme.border)
        .cursor_pointer()
        .hover(|style| style.bg(theme.surface_active))
        .focus_visible(|style| style.border_color(theme.info))
        .child(
            div()
                .flex()
                .items_center()
                .w(px(26.0))
                .h(px(14.0))
                .p(px(2.0))
                .rounded_full()
                .bg(if on { theme.text } else { theme.border_strong })
                .when(on, |track| track.justify_end())
                .child(div().size(px(10.0)).rounded_full().bg(theme.window)),
        )
        .child(
            div()
                .w(px(24.0))
                .text_xs()
                .child(if on { "On" } else { "Off" }),
        )
}

#[cfg(not(feature = "silvestris-ui"))]
fn value_text(theme: theme::Palette, text: String) -> impl IntoElement {
    div()
        .min_w(px(56.0))
        .max_w(px(220.0))
        .truncate()
        .text_xs()
        .font_family(if cfg!(feature = "silvestris-ui") {
            theme.mono
        } else {
            "monospace"
        })
        .text_color(theme.text_secondary)
        .child(text)
}

#[cfg(not(feature = "silvestris-ui"))]
fn source_badge(theme: theme::Palette, source: &str, overridden: bool) -> impl IntoElement {
    let label = match source {
        "default" => "default".to_string(),
        "global" => "global".to_string(),
        "card" => "card".to_string(),
        "managed" => "managed".to_string(),
        other => other.to_string(),
    };
    div()
        .flex_none()
        .px(px(5.0))
        .rounded(px(4.0))
        .border_1()
        .border_color(if overridden { theme.info } else { theme.border })
        .text_size(px(10.0))
        .text_color(if overridden {
            theme.info
        } else {
            theme.text_muted
        })
        .child(if overridden {
            format!("set here · {label}")
        } else {
            format!("inherited · {label}")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "silvestris-ui")]
    #[test]
    fn shared_checkbox_writes_only_its_setting_and_read_only_boolean_cannot_activate() {
        use gpui::{Modifiers, TestAppContext, VisualTestContext};
        use silvestris::{ThemeId, ThemePreference};
        struct Row {
            settings: gpui::Entity<SettingsWindow>,
            descriptor: &'static Descriptor,
        }
        impl Render for Row {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let row = self.settings.update(cx, |settings, cx| {
                    settings.form_row(self.descriptor, window, cx)
                });
                silvestris::preferences::Form::new(200.0).row(row)
            }
        }
        fn draw(cx: &mut VisualTestContext) {
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.draw(cx).clear(cx);
            });
        }
        for theme in ThemeId::ALL {
            let mut app = TestAppContext::single();
            app.update(|cx| {
                silvestris::test_support::initialize(ThemePreference::explicit(theme), cx)
            });
            let (main, _) = app.add_window_view(LotusWindow::test_preview);
            let key = "lotus.desktop.sessions_open";
            let descriptor = Registry::builtin()
                .descriptors()
                .iter()
                .find(|d| d.key == key)
                .unwrap();
            let (row, cx) = app.add_window_view(|window, cx| Row {
                settings: cx.new(|cx| {
                    SettingsWindow::new(main.downgrade(), None, false, Target::Global, window, cx)
                }),
                descriptor,
            });
            draw(cx);
            let checkbox = cx
                .debug_bounds("toggle-lotus.desktop.sessions_open")
                .unwrap();
            cx.simulate_click(checkbox.center(), Modifiers::default());
            draw(cx);
            cx.update(|_, cx| {
                assert_eq!(main.read(cx).preference(key), Value::Bool(false));
                assert_eq!(
                    main.read(cx).preference("lotus.desktop.inspector_open"),
                    Value::Bool(false)
                );
            });
            cx.simulate_keystrokes("space");
            cx.simulate_event(gpui::KeyUpEvent {
                keystroke: gpui::Keystroke::parse("space").unwrap(),
            });
            draw(cx);
            cx.update(|_, cx| assert_eq!(main.read(cx).preference(key), Value::Bool(true)));
            let locked = Registry::builtin()
                .descriptors()
                .iter()
                .find(|d| d.key == "use_hw_shader")
                .unwrap();
            cx.update(|_, cx| {
                row.update(cx, |row, cx| {
                    row.descriptor = locked;
                    cx.notify();
                })
            });
            draw(cx);
            let checkbox = cx.debug_bounds("toggle-use_hw_shader").unwrap();
            cx.simulate_click(checkbox.center(), Modifiers::default());
            cx.simulate_keystrokes("space");
            cx.simulate_event(gpui::KeyUpEvent {
                keystroke: gpui::Keystroke::parse("space").unwrap(),
            });
            draw(cx);
            cx.update(|window, cx| {
                assert!(window.focused(cx).is_none());
                assert!(row.read(cx).settings.read(cx).error.is_none());
                assert_eq!(main.read(cx).preference(key), Value::Bool(true));
            });
        }
    }

    #[cfg(feature = "silvestris-ui")]
    #[test]
    fn appearance_controls_switch_both_dimensions_in_open_main_and_settings_windows() {
        use gpui::{size, Modifiers, TestAppContext, VisualTestContext};
        use silvestris::{ActiveTheme, ThemeId, ThemePreference};
        fn draw(cx: &mut VisualTestContext) {
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.draw(cx).clear(cx);
            });
        }
        let mut app = TestAppContext::single();
        app.update(|cx| {
            silvestris::test_support::initialize(ThemePreference::explicit(ThemeId::GEIST_DARK), cx)
        });
        let (main, main_cx) = app.add_window_view(LotusWindow::test_preview);
        main_cx.simulate_resize(size(px(1280.), px(880.)));
        draw(main_cx);
        let main_handle = main_cx.update(|window, _| window.window_handle());
        let (_, cx) = app.add_window_view(|window, cx| {
            SettingsWindow::new(main.downgrade(), None, false, Target::Global, window, cx)
        });
        draw(cx);
        // Opens on the overview (HIG p. 244); its General icon shows the pane.
        let general = cx
            .debug_bounds("preference-pane-general")
            .expect("overview icon");
        cx.simulate_click(general.center(), Modifiers::default());
        draw(cx);
        let mut main_cx = VisualTestContext::from_window(main_handle, &cx.cx);
        let workspace = main_cx.debug_bounds("shell-workspace").unwrap();
        for (selector, expected) in [
            ("segment-aqua", ThemeId::AQUA_DARK),
            ("segment-light", ThemeId::AQUA_LIGHT),
            ("segment-geist", ThemeId::GEIST_LIGHT),
            ("segment-dark", ThemeId::GEIST_DARK),
        ] {
            let segment = cx
                .debug_bounds(selector)
                .expect("appearance choice mounted");
            cx.simulate_click(segment.center(), Modifiers::default());
            let focus = cx.update(|window, cx| window.focused(cx));
            draw(cx);
            draw(&mut main_cx);
            cx.update(|window, cx| {
                assert_eq!(cx.theme().id, expected);
                assert_eq!(main.read(cx).ui_theme(), expected);
                assert_eq!(theme::palette(cx), theme::from_theme(cx.theme()));
                assert_eq!(
                    window.focused(cx),
                    focus,
                    "switch preserves the choice responder"
                );
            });
            let bounds = main_cx.debug_bounds("shell-workspace").unwrap();
            let metrics = silvestris::core::theme(expected).metrics;
            assert_eq!(
                (bounds.left(), bounds.size.width),
                (workspace.left(), workspace.size.width)
            );
            assert_eq!(
                bounds.top(),
                px(metrics.titlebar_height + metrics.toolbar_height)
            );
            assert_eq!(bounds.bottom(), px(880. - metrics.bottom_bar_height));
            assert!(cx.debug_bounds("appearance-style").is_some());
        }
    }

    #[test]
    fn steps_are_round_and_bounded() {
        assert_eq!(nice_step(0.0099), 0.01);
        assert_eq!(nice_step(0.019), 0.02);
        assert_eq!(nice_step(0.03), 0.05);
        assert_eq!(nice_step(3.0), 5.0);
        let (step, min, max, integer) = stepper(&Kind::Float {
            min: Some(0.0),
            max: Some(0.99),
        })
        .unwrap();
        assert_eq!(
            (step, min, max, integer),
            (0.01, Some(0.0), Some(0.99), false)
        );
    }

    #[test]
    fn every_target_scope_has_rows_and_hides_window_state() {
        let registry = Registry::builtin();
        assert!(registry.get(BOUNDS_KEY).is_some(), "bounds key registered");
        for scope in [Scope::Global, Scope::Primary] {
            let shown = registry
                .descriptors()
                .iter()
                .filter(|d| d.scopes.allows(scope) && d.key != BOUNDS_KEY)
                .count();
            assert!(shown > 0);
        }
        // A card never offers host or desktop settings: they are not per card.
        assert!(registry
            .for_scope(Scope::Primary)
            .all(|d| matches!(d.owner, Owner::Azahar | Owner::Input)));
    }

    #[test]
    fn saved_bounds_need_the_minimum_size() {
        assert!(bounds_from(&serde_json::json!([10.0, 20.0, 900.0, 640.0])).is_some());
        assert!(bounds_from(&serde_json::json!([10.0, 20.0, 100.0, 640.0])).is_none());
        assert!(bounds_from(&serde_json::json!(null)).is_none());
    }
}
