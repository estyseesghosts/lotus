//! #138: Settings as a System Preferences-style window (HIG pp. 243-244,
//! 347-350; silvestris docs/preferences.md). Overview of category icons,
//! one pane per registry Group, registry subgroups as the pane's tabs.
//! Global/card scope is a pop-up in the toolbar's search position, because it
//! changes every pane's values rather than selecting a pane.
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use gpui::{AnyElement, Entity, Focusable, ImageFormat, Pixels};
use silvestris::controls::{
    Button, Checkbox, CheckState, ControlSize, PopUpButton, Segment, SegmentedControl, Stepper,
    TextField, TextFieldEvent,
};
use silvestris::preferences::{
    self as prefs, Form, FormRow, Navigation, PreferenceCategory, PreferencePane,
    PreferencesNavigator, PreferencesOverview, PreferencesToolbar, TabView,
};
use silvestris::ActiveTheme;

use super::*;

/// Overview title (the 10.6 overview window is titled with the app's name).
pub(super) const OVERVIEW_TITLE: &str = "Lotus Settings";
const LABEL_WIDTH: f32 = 200.0;
/// More subgroups than this become headings instead of tabs (a 668 px window
/// fits about five 10.6 tabs).
const MAX_TABS: usize = 5;

macro_rules! icon {
    ($name:literal) => {{
        static IMAGE: LazyLock<Arc<gpui::Image>> = LazyLock::new(|| {
            Arc::new(gpui::Image::from_bytes(
                ImageFormat::Svg,
                include_bytes!(concat!("../../assets/prefs/", $name, ".svg")).to_vec(),
            ))
        });
        IMAGE.clone()
    }};
}

/// Share of the display's visible area the window may take in either
/// direction (user decision, 2026-11-19); longer content scrolls.
const MAX_SCREEN_FRACTION: f32 = 0.66;

/// Resize the window by `delta` to fit the shown view, as 10.6 System
/// Preferences resizes per pane (HIG p. 244; M: prefs.pane.geometry). Width
/// never drops below the 668 px preferences width and grows only for a pane
/// whose tab strip needs it. Both sides are capped at `MAX_SCREEN_FRACTION`
/// of the display, independent of where the window sits, so moving the
/// window never changes its size. Returns true once no resize is needed.
fn fit_size(delta: gpui::Size<Pixels>, window: &mut Window, cx: &mut App) -> bool {
    let viewport = window.viewport_size();
    let mut width = (viewport.width + delta.width).max(px(prefs::WINDOW_WIDTH));
    let mut height = viewport.height + delta.height;
    if let Some(display) = window.display(cx) {
        let outer = window.bounds();
        let visible = display.visible_bounds().size;
        let chrome = outer.size - viewport;
        width = width.min(visible.width * MAX_SCREEN_FRACTION - chrome.width).max(px(prefs::WINDOW_WIDTH));
        height = height.min(visible.height * MAX_SCREEN_FRACTION - chrome.height);
    }
    if (width - viewport.width).abs() < px(1.0) && (height - viewport.height).abs() < px(1.0) {
        return true;
    }
    window.defer(cx, move |window, _| window.resize(gpui::size(width, height)));
    false
}

/// Apply `fit_size` only while a navigation or tab switch is pending.
fn fit_pending(this: &gpui::WeakEntity<SettingsWindow>, delta: gpui::Size<Pixels>, window: &mut Window, cx: &mut App) {
    let Ok(true) = this.read_with(cx, |view, _| view.prefs.fit_pending) else { return };
    if fit_size(delta, window, cx) {
        let _ = this.update(cx, |view, _| view.prefs.fit_pending = false);
    }
}

pub(super) fn group_id(group: Group) -> &'static str {
    match group {
        Group::General => "general",
        Group::SessionsStorage => "sessions",
        Group::Emulation => "emulation",
        Group::GraphicsDisplay => "graphics",
        Group::AudioDevices => "audio",
        Group::Controls => "controls",
        Group::DebuggingTracing => "debugging",
        Group::McpAutomation => "automation",
        Group::Discord => "presence",
        Group::Advanced => "advanced",
    }
}

fn group_from(id: &str) -> Option<Group> {
    Group::ALL.into_iter().find(|g| group_id(*g) == id)
}

fn group_icon(group: Group) -> Arc<gpui::Image> {
    match group {
        Group::General => icon!("general"),
        Group::SessionsStorage => icon!("sessions"),
        Group::Emulation => icon!("emulation"),
        Group::GraphicsDisplay => icon!("graphics"),
        Group::AudioDevices => icon!("audio"),
        Group::Controls => icon!("controls"),
        Group::DebuggingTracing => icon!("debugging"),
        Group::McpAutomation => icon!("automation"),
        Group::Discord => icon!("presence"),
        Group::Advanced => icon!("advanced"),
    }
}

/// Overview bands, like System Preferences' Personal/Hardware/… rows.
const BANDS: [(&str, &[Group]); 3] = [
    ("Lotus", &[Group::General, Group::SessionsStorage, Group::Discord]),
    (
        "Emulation",
        &[Group::Emulation, Group::GraphicsDisplay, Group::AudioDevices, Group::Controls],
    ),
    ("Development", &[Group::DebuggingTracing, Group::McpAutomation, Group::Advanced]),
];

#[derive(Default)]
pub(super) struct PreferencesState {
    pub navigator: PreferencesNavigator,
    tabs: HashMap<Group, &'static str>,
    fields: HashMap<String, Entity<TextField>>,
    sized_for: Option<Option<SharedString>>,
    /// Set when the pane or tab changes; cleared once the window fits, so
    /// the window is resized only on navigation, never every frame.
    fit_pending: bool,
}

impl SettingsWindow {
    pub(super) fn categories(&self) -> Vec<PreferenceCategory> {
        BANDS
            .iter()
            .filter_map(|(label, groups)| {
                let panes: Vec<_> = groups
                    .iter()
                    .filter(|g| !self.rows(**g).is_empty())
                    .map(|g| {
                        let image = group_icon(*g);
                        PreferencePane::new(group_id(*g), g.label())
                            .icon(move |_, _| gpui::img(image.clone()).size(px(prefs::ICON_SIZE)))
                    })
                    .collect();
                (!panes.is_empty()).then(|| PreferenceCategory::new(*label, panes))
            })
            .collect()
    }

    pub(super) fn navigate(&mut self, to: Navigation, window: &mut Window, cx: &mut Context<Self>) {
        let nav = &mut self.prefs.navigator;
        match to {
            Navigation::Back => nav.go_back(),
            Navigation::Forward => nav.go_forward(),
            Navigation::ShowAll => nav.go(None),
        };
        self.after_navigation(window, cx);
    }

    pub(super) fn show_group(&mut self, group: Group, window: &mut Window, cx: &mut Context<Self>) {
        self.prefs.navigator.go(Some(group_id(group).into()));
        self.after_navigation(window, cx);
    }

    fn after_navigation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(group) = self.prefs.navigator.current().and_then(|id| group_from(id)) {
            self.group = group;
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// HIG p. 244: the title names the pane. The height follows the shown
    /// view (`fit_size`).
    fn fit_window(&mut self, window: &mut Window) {
        let current = self.prefs.navigator.current().cloned();
        if self.prefs.sized_for.as_ref() == Some(&current) {
            return;
        }
        self.prefs.sized_for = Some(current.clone());
        self.prefs.fit_pending = true;
        let categories = self.categories();
        window.set_window_title(&self.prefs.navigator.title(&categories, OVERVIEW_TITLE));
    }

    fn scope_popup(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let targets: Vec<Target> = std::iter::once(Target::Global)
            .chain(self.cards.iter().cloned().map(Target::Card))
            .collect();
        let items = targets
            .iter()
            .map(|t| match t {
                Target::Global => SharedString::from("All Cards (Global)"),
                Target::Card(card) => SharedString::from(format!("Card {card}")),
            })
            .collect();
        let selected = targets.iter().position(|t| *t == self.target);
        let this = cx.entity().downgrade();
        div()
            .flex()
            .items_center()
            .gap(px(prefs::CONTROL_GAP))
            .text_size(px(cx.theme().type_.size_small))
            .child("Settings for:")
            .child(
                PopUpButton::new("settings-scope", "Settings for", items)
                    .selected(selected)
                    .min_width(170.0)
                    .on_choose(move |index, _, cx| {
                        if let Some(target) = targets.get(*index).cloned() {
                            let _ = this.update(cx, |view, cx| view.set_target(target, cx));
                        }
                    }),
            )
    }

    pub(super) fn render_preferences(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // A scope with fewer groups may remove the shown pane.
        let categories = self.categories();
        self.prefs.navigator.retain(|id| prefs::find(&categories, id).is_some());
        self.fit_window(window);
        let this = cx.entity().downgrade();
        let toolbar = PreferencesToolbar::new(&self.prefs.navigator)
            .on_navigate(move |to, window, cx| {
                let _ = this.update(cx, |view, cx| view.navigate(to, window, cx));
            })
            .search(self.scope_popup(cx));
        let content = match self.prefs.navigator.current().and_then(|id| group_from(id)) {
            None => {
                let this = cx.entity().downgrade();
                let overview = PreferencesOverview::new(categories).on_select(move |id, window, cx| {
                    if let Some(group) = group_from(id) {
                        let _ = this.update(cx, |view, cx| view.show_group(group, window, cx));
                    }
                });
                let this = cx.entity().downgrade();
                prefs::fit_content(overview, move |needed, window, cx| {
                    let viewport = window.viewport_size();
                    let delta = gpui::size(px(prefs::WINDOW_WIDTH) - viewport.width, needed - viewport.height);
                    fit_pending(&this, delta, window, cx);
                })
                .into_any_element()
            }
            Some(group) => self.render_pane(group, window, cx).into_any_element(),
        };
        let title = self.prefs.navigator.title(&self.categories(), OVERVIEW_TITLE);
        let height = cx.theme().metrics.toolbar_height;
        silvestris::chrome::WindowFrame::new(title)
            .fixed_size()
            .toolbar(toolbar, height)
            .child(
                div()
                    .id("settings-root")
                    .debug_selector(|| "settings-root".into())
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
                    .flex_col()
                    .child(content),
            )
            .into_any_element()
    }

    fn subgroups(&self, group: Group) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        for d in self.rows(group) {
            if !out.contains(&d.subgroup) {
                out.push(d.subgroup);
            }
        }
        out
    }

    fn render_pane(&mut self, group: Group, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let subgroups = self.subgroups(group);
        let tabbed = (2..=MAX_TABS).contains(&subgroups.len());
        let tab = self
            .prefs
            .tabs
            .get(&group)
            .copied()
            .filter(|t| subgroups.contains(t))
            .or_else(|| subgroups.first().copied())
            .unwrap_or("");
        let rows: Vec<&'static Descriptor> = self
            .rows(group)
            .into_iter()
            .filter(|d| !tabbed || d.subgroup == tab)
            .collect();
        let theme = cx.theme().clone();
        let mut body = div().flex().flex_col().gap(px(12.0)).children(self.status_notes(group, cx));
        if group == Group::General && self.target == Target::Global {
            body = body.child(Form::new(LABEL_WIDTH).rows(self.appearance_rows(cx)));
        }
        let mut current: Option<&'static str> = None;
        let mut form_rows = Vec::new();
        for descriptor in rows {
            if !tabbed && subgroups.len() > 1 && current != Some(descriptor.subgroup) {
                if !form_rows.is_empty() {
                    body = body.child(Form::new(LABEL_WIDTH).rows(std::mem::take(&mut form_rows)));
                }
                current = Some(descriptor.subgroup);
                body = body.child(
                    div()
                        .pt(px(4.0))
                        .font_weight(FontWeight::BOLD)
                        .text_size(px(theme.type_.size_small))
                        .child(subgroup_label(descriptor.subgroup)),
                );
            }
            form_rows.push(self.form_row(descriptor, window, cx));
        }
        if !form_rows.is_empty() {
            body = body.child(Form::new(LABEL_WIDTH).rows(form_rows));
        }
        let mut view = TabView::new(SharedString::from(format!("pane-{}", group_id(group))));
        if tabbed {
            let this = cx.entity().downgrade();
            let tabs = subgroups.iter().map(|s| Segment::new(*s, subgroup_label(s))).collect();
            view = view.tabs(tabs, tab).on_select(move |id, _, cx| {
                let id = id.clone();
                let _ = this.update(cx, |view, cx| {
                    if let Some(s) = view.subgroups(group).into_iter().find(|s| *s == id.as_ref()) {
                        view.prefs.tabs.insert(group, s);
                        view.prefs.fit_pending = true;
                        cx.notify();
                    }
                });
            });
        }
        let this = cx.entity().downgrade();
        let view = view.on_overflow(move |delta, window, cx| fit_pending(&this, delta, window, cx));
        prefs::pane_body().child(view.child(body))
    }

    fn status_notes(&self, group: Group, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let note = |text: String, color| {
            div()
                .text_size(px(theme.type_.size_small))
                .text_color(silvestris::convert::hsla(color))
                .child(text)
                .into_any_element()
        };
        let c = &theme.colors;
        let mut out = vec![note(
            match &self.target {
                Target::Global => "Defaults for every card and new secondary session. Worker settings \
                    apply to sessions started afterwards; input layouts apply at once."
                    .into(),
                Target::Card(card) => format!(
                    "Saved overrides of card {card}; unset values use the global defaults. Worker \
                     settings apply when its primary next starts; input layouts apply at once."
                ),
            },
            c.text_secondary,
        )];
        if let Some(error) = self.error.clone() {
            out.push(note(error, c.error));
        }
        if self.host.is_none() {
            out.push(note("No host configuration: only desktop preferences can be changed.".into(), c.warning));
        } else if !self.control {
            out.push(note(
                "This host is read-only (allow_control is off): only desktop preferences can be changed.".into(),
                c.warning,
            ));
        }
        if !self.lotus.read_with(cx, |l, _| l.preferences_saved()).unwrap_or(true) {
            out.push(note(
                "desktop.json has no location or could not be read: preference changes last until Lotus closes."
                    .into(),
                c.warning,
            ));
        }
        if group == Group::Discord {
            let palette = theme::palette(cx);
            if let Some((_, text)) = self.lotus.read_with(cx, |l, _| l.presence_line(palette)).ok().flatten() {
                out.push(note(text.to_string(), c.text_secondary));
            }
        }
        out
    }

    fn appearance_rows(&self, cx: &mut Context<Self>) -> Vec<FormRow> {
        use silvestris::{Appearance, ThemeFamily, ThemeId};
        let id = self.lotus.read_with(cx, |view, _| view.ui_theme()).unwrap_or(ThemeId::GEIST_DARK);
        let family_owner = self.lotus.clone();
        let appearance_owner = self.lotus.clone();
        vec![
            FormRow::new(
                "Style",
                SegmentedControl::new(
                    "theme-family",
                    "Style",
                    vec![Segment::new("aqua", "Aqua"), Segment::new("geist", "Geist")],
                )
                .selected(if id.family == ThemeFamily::Aqua { "aqua" } else { "geist" })
                .on_select(move |id, _, cx| {
                    let family = match id.as_ref() {
                        "aqua" => ThemeFamily::Aqua,
                        "geist" => ThemeFamily::Geist,
                        _ => return,
                    };
                    let _ = family_owner.update(cx, |view, cx| {
                        view.set_ui_theme(ThemeId::new(family, view.ui_theme().appearance), cx)
                    });
                }),
            )
            .id("appearance-style"),
            FormRow::new(
                "Appearance",
                SegmentedControl::new(
                    "theme-appearance",
                    "Appearance",
                    vec![Segment::new("light", "Light"), Segment::new("dark", "Dark")],
                )
                .selected(if id.appearance == Appearance::Light { "light" } else { "dark" })
                .on_select(move |id, _, cx| {
                    let appearance = match id.as_ref() {
                        "light" => Appearance::Light,
                        "dark" => Appearance::Dark,
                        _ => return,
                    };
                    let _ = appearance_owner.update(cx, |view, cx| {
                        view.set_ui_theme(ThemeId::new(view.ui_theme().family, appearance), cx)
                    });
                }),
            )
            .id("appearance-mode"),
        ]
    }

    /// One registry setting as a form row. Checkboxes carry their own label
    /// (HIG p. 348: checkboxes left-aligned in the control column).
    pub(super) fn form_row(&mut self, descriptor: &'static Descriptor, window: &mut Window, cx: &mut Context<Self>) -> FormRow {
        let (value, source) = self.value(descriptor, cx);
        let editable = self.editable(descriptor, &source);
        let overridden = match descriptor.owner {
            Owner::Desktop => value != descriptor.default,
            _ => source == self.target.layer(),
        };
        let key = descriptor.key.clone();
        let enabled = editable.is_ok();
        let mut control = div().flex().items_center().gap(px(prefs::CONTROL_GAP));
        let mut labelled = true;
        match &descriptor.kind {
            Kind::Boolean => {
                labelled = false;
                control = control.child(
                    Checkbox::new(SharedString::from(format!("toggle-{key}")), descriptor.label.clone())
                        .checked(value.as_bool().unwrap_or(false))
                        .disabled(!enabled)
                        .on_change(cx.listener(move |view, state: &CheckState, _, cx| {
                            view.write(descriptor, Some(Value::Bool(*state == CheckState::On)), cx);
                        })),
                );
            }
            kind if choices(kind).is_some() => {
                let options = choices(kind).unwrap_or_default();
                let items = options.iter().map(|o| SharedString::from(display(o))).collect();
                let selected = options.iter().position(|o| *o == value);
                control = control.child(
                    PopUpButton::new(SharedString::from(format!("choice-{key}")), descriptor.label.clone(), items)
                        .selected(selected)
                        .disabled(!enabled)
                        .min_width(140.0)
                        .on_choose(cx.listener(move |view, index: &usize, _, cx| {
                            if let Some(choice) = options.get(*index) {
                                view.write(descriptor, Some(choice.clone()), cx);
                            }
                        })),
                );
            }
            kind if stepper(kind).is_some() => {
                let (step, min, max, integer) = stepper(kind).unwrap_or((1.0, None, None, true));
                let current = value.as_f64().unwrap_or(0.0);
                let field = self.number_field(descriptor, &value, enabled, window, cx);
                let clamp = move |next: f64| {
                    let mut next = next;
                    if let Some(min) = min {
                        next = next.max(min);
                    }
                    if let Some(max) = max {
                        next = next.min(max);
                    }
                    if integer { Value::from(next.round() as i64) } else { Value::from((next * 1e4).round() / 1e4) }
                };
                control = control
                    .child(div().w(px(80.0)).child(field))
                    .child(
                        Stepper::new(SharedString::from(format!("stepper-{key}")), descriptor.label.clone())
                            .limits(
                                enabled && min.is_none_or(|m| current > m),
                                enabled && max.is_none_or(|m| current < m),
                            )
                            .on_step(cx.listener(move |view, dir: &i32, _, cx| {
                                let next = ((current + *dir as f64 * step) / step).round() * step;
                                view.write(descriptor, Some(clamp(next)), cx);
                            })),
                    );
            }
            _ => {
                control = control.child(with_tooltip(
                    div()
                        .id(SharedString::from(format!("text-{key}")))
                        .text_color(silvestris::convert::hsla(cx.theme().colors.text_secondary))
                        .child(display(&value)),
                    "Text and list values are edited with ctr-cli or MCP for now (#54).",
                ));
            }
        }
        if overridden && enabled {
            control = control.child(
                Button::new(SharedString::from(format!("reset-{key}")), reset_label(&self.target, descriptor))
                    .size(ControlSize::Mini)
                    .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| view.write(descriptor, None, cx))),
            );
        }
        // The consumer citation is a help tag, not body text (HIG p. 82 help tags).
        let control: AnyElement = if descriptor.sources.is_empty() {
            control.into_any_element()
        } else {
            with_tooltip(
                control.id(SharedString::from(format!("help-{key}"))),
                format!("Read by {}", descriptor.sources.join(", ")),
            )
            .into_any_element()
        };
        let mut row = if labelled {
            FormRow::new(descriptor.label.clone(), control)
        } else {
            FormRow::control(control)
        };
        // Window-wide reasons (no host, read-only host) are stated once in
        // status_notes; rows add only their own.
        let window_wide = self.host.is_none() || !self.control;
        let note = match editable {
            Err(reason) if !(window_wide && matches!(descriptor.owner, Owner::Azahar | Owner::Input)) => {
                format!("{} {reason}", descriptor.description)
            }
            _ => descriptor.description.clone(),
        };
        if !note.trim().is_empty() {
            row = row.note(note);
        }
        row.id(SharedString::from(format!("row-{key}")))
    }

    /// A text field for a numeric setting; commits on Return or leaving it.
    fn number_field(
        &mut self,
        descriptor: &'static Descriptor,
        value: &Value,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TextField> {
        let shown = display(value);
        let field = match self.prefs.fields.get(&descriptor.key) {
            Some(field) => field.clone(),
            None => {
                let field = cx.new(|cx| {
                    let mut f = TextField::new(window, cx);
                    f.set_size(ControlSize::Small, cx);
                    f.set_label(descriptor.label.clone(), cx);
                    f.set_text(shown.clone(), cx);
                    f
                });
                cx.subscribe(&field, move |view, field, event: &TextFieldEvent, cx| {
                    if !matches!(event, TextFieldEvent::Submitted | TextFieldEvent::EndedEditing) {
                        return;
                    }
                    let text = field.read(cx).text().trim().to_string();
                    let (current, _) = view.value(descriptor, cx);
                    let Some((_, min, max, integer)) = stepper(&descriptor.kind) else { return };
                    match text.parse::<f64>() {
                        Ok(mut next) if next.is_finite() => {
                            if let Some(min) = min { next = next.max(min); }
                            if let Some(max) = max { next = next.min(max); }
                            let next = if integer { Value::from(next.round() as i64) } else { Value::from(next) };
                            if next != current {
                                view.write(descriptor, Some(next), cx);
                            }
                            field.update(cx, |f, cx| f.set_text(display(&view.value(descriptor, cx).0), cx));
                        }
                        // Invalid input reverts to the saved value.
                        _ => field.update(cx, |f, cx| f.set_text(display(&current), cx)),
                    }
                })
                .detach();
                self.prefs.fields.insert(descriptor.key.clone(), field.clone());
                field
            }
        };
        let focused = field.read(cx).focus_handle(cx).is_focused(window);
        field.update(cx, |f, cx| {
            if !focused && f.text() != shown {
                f.set_text(shown, cx);
            }
            f.set_disabled(!enabled, window, cx);
        });
        field
    }
}

fn subgroup_label(subgroup: &str) -> String {
    // Registry subgroups are short identifiers ("cpu", "audio_input"…).
    subgroup
        .split(['_', '-'])
        .filter(|w| !w.is_empty())
        .enumerate()
        .map(|(i, w)| {
            let mut chars = w.chars();
            match chars.next() {
                Some(f) if i == 0 => f.to_uppercase().collect::<String>() + chars.as_str(),
                _ => w.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
