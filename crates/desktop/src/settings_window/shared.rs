//! #138: Settings as a System Preferences-style window (HIG pp. 243-244,
//! 347-350; silvestris docs/preferences.md). Overview of category icons,
//! one pane per registry Group, registry subgroups as the pane's tabs.
//! The toolbar holds the scope pop-up and a search field for its settings registry.
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use gpui::prelude::FluentBuilder;
use gpui::{AnyElement, Entity, Focusable, ImageFormat, Pixels, StatefulInteractiveElement};
use silvestris::controls::{
    Button, CheckState, Checkbox, ControlSize, PopUpButton, Segment, SegmentedControl, Stepper,
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
        width = width
            .min(visible.width * MAX_SCREEN_FRACTION - chrome.width)
            .max(px(prefs::WINDOW_WIDTH));
        height = height.min(visible.height * MAX_SCREEN_FRACTION - chrome.height);
    }
    if (width - viewport.width).abs() < px(1.0) && (height - viewport.height).abs() < px(1.0) {
        return true;
    }
    window.defer(cx, move |window, _| {
        window.resize(gpui::size(width, height))
    });
    false
}

/// Apply `fit_size` only while a navigation or tab switch is pending.
fn fit_pending(
    this: &gpui::WeakEntity<SettingsWindow>,
    delta: gpui::Size<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let Ok(true) = this.read_with(cx, |view, _| view.prefs.fit_pending) else {
        return;
    };
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
    (
        "Lotus",
        &[Group::General, Group::SessionsStorage, Group::Discord],
    ),
    (
        "Emulation",
        &[
            Group::Emulation,
            Group::GraphicsDisplay,
            Group::AudioDevices,
            Group::Controls,
        ],
    ),
    (
        "Development",
        &[
            Group::DebuggingTracing,
            Group::McpAutomation,
            Group::Advanced,
        ],
    ),
];

#[derive(Default)]
pub(super) struct PreferencesState {
    pub navigator: PreferencesNavigator,
    tabs: HashMap<Group, &'static str>,
    pub(super) fields: HashMap<String, Entity<TextField>>,
    pub(super) field_errors: HashMap<String, String>,
    search: Option<Entity<TextField>>,
    pub(super) reset_card: Option<Target>,
    pane_scroll: gpui::ScrollHandle,
    reveal: Option<&'static Descriptor>,
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
        self.cancel_binding_capture(cx);
        self.prefs.pane_scroll = gpui::ScrollHandle::new();
        self.prefs.reveal = None;
        if let Some(search) = &self.prefs.search {
            search.update(cx, |field, cx| field.set_text("", cx));
        }
        if let Some(group) = self.prefs.navigator.current().and_then(|id| group_from(id)) {
            self.group = group;
            if group == Group::Controls {
                self.refresh_controllers(cx);
            }
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
        let mut targets: Vec<Target> = std::iter::once(Target::Global)
            .chain(self.cards.iter().cloned().map(Target::Card))
            .chain(
                self.controllers
                    .secondaries
                    .iter()
                    .cloned()
                    .map(Target::Secondary),
            )
            .collect();
        if !targets.contains(&self.target) && matches!(self.target, Target::Secondary(_)) {
            targets.push(self.target.clone());
        }
        let items = targets
            .iter()
            .map(|t| match t {
                Target::Global => SharedString::from("All Cards (Global)"),
                Target::Card(card) => SharedString::from(format!("Card {card}")),
                Target::Secondary(id) => SharedString::from(
                    self.controllers
                        .sessions
                        .iter()
                        .find(|(session, _)| session == id)
                        .map(|(_, label)| format!("{label} (temporary)"))
                        .unwrap_or_else(|| {
                            format!("Secondary {} (stopped)", crate::session_tree::short_id(id))
                        }),
                ),
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

    fn search_field(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<TextField> {
        self.prefs
            .search
            .get_or_insert_with(|| {
                let field = cx.new(|cx| {
                    let mut field = TextField::new(window, cx);
                    field.set_label("Search Settings", cx);
                    field.set_placeholder("Search Settings", cx);
                    field.set_size(ControlSize::Small, cx);
                    field.set_search(true, cx);
                    field
                });
                cx.subscribe(&field, |_, _, event, cx| {
                    if matches!(event, TextFieldEvent::Changed) {
                        cx.notify();
                    }
                })
                .detach();
                field
            })
            .clone()
    }

    fn render_search(&self, query: &str, cx: &mut Context<Self>) -> AnyElement {
        let words: Vec<_> = query.split_whitespace().collect();
        let rows: Vec<_> = BANDS
            .iter()
            .flat_map(|(_, groups)| groups.iter().copied())
            .flat_map(|group| self.rows(group))
            .filter(|setting| {
                let haystack = format!(
                    "{} {} {} {} {} {}",
                    setting.key,
                    setting.label,
                    setting.subgroup,
                    setting.group.label(),
                    setting.description,
                    setting.keywords.join(" "),
                )
                .to_lowercase();
                words.iter().all(|word| haystack.contains(word))
            })
            .collect();
        let theme = cx.theme().clone();
        let mut results = div()
            .id("settings-search-results")
            .debug_selector(|| "settings-search-results".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(px(16.0));
        if rows.is_empty() {
            results = results.child(
                div()
                    .text_color(silvestris::convert::hsla(theme.colors.text_secondary))
                    .child("No settings match your search."),
            );
        } else {
            results = results.children(rows.into_iter().map(|setting| {
                div()
                    .flex()
                    .items_start()
                    .gap(px(12.0))
                    .py(px(5.0))
                    .child(
                        Button::new(
                            SharedString::from(format!("find-{}", setting.key)),
                            setting.label.clone(),
                        )
                        .size(ControlSize::Small)
                        .on_click(cx.listener(
                            move |view, _: &ClickEvent, window, cx| {
                                view.show_group(setting.group, window, cx);
                                view.prefs.tabs.insert(setting.group, setting.subgroup);
                                view.prefs.reveal = Some(setting);
                            },
                        )),
                    )
                    .child(
                        div()
                            .text_size(px(theme.type_.size_small))
                            .text_color(silvestris::convert::hsla(theme.colors.text_secondary))
                            .child(format!("{} · {}", setting.group.label(), setting.subgroup)),
                    )
            }));
        }
        results.into_any_element()
    }

    pub(super) fn render_preferences(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // A scope with fewer groups may remove the shown pane.
        let categories = self.categories();
        self.prefs
            .navigator
            .retain(|id| prefs::find(&categories, id).is_some());
        self.fit_window(window);
        let this = cx.entity().downgrade();
        let search = self.search_field(window, cx);
        let query = search.read(cx).text().trim().to_lowercase();
        let toolbar = PreferencesToolbar::new(&self.prefs.navigator)
            .on_navigate(move |to, window, cx| {
                let _ = this.update(cx, |view, cx| view.navigate(to, window, cx));
            })
            .search(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(self.scope_popup(cx))
                    .child(div().w(px(166.0)).child(search)),
            );
        let content = if !query.is_empty() {
            self.render_search(&query, cx)
        } else {
            match self.prefs.navigator.current().and_then(|id| group_from(id)) {
                None => {
                    let this = cx.entity().downgrade();
                    let overview =
                        PreferencesOverview::new(categories).on_select(move |id, window, cx| {
                            if let Some(group) = group_from(id) {
                                let _ =
                                    this.update(cx, |view, cx| view.show_group(group, window, cx));
                            }
                        });
                    let this = cx.entity().downgrade();
                    prefs::fit_content(overview, move |needed, window, cx| {
                        let viewport = window.viewport_size();
                        let delta = gpui::size(
                            px(prefs::WINDOW_WIDTH) - viewport.width,
                            needed - viewport.height,
                        );
                        fit_pending(&this, delta, window, cx);
                    })
                    .into_any_element()
                }
                Some(group) => self.render_pane(group, window, cx).into_any_element(),
            }
        };

        let title = self
            .prefs
            .navigator
            .title(&self.categories(), OVERVIEW_TITLE);
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
                    .on_key_up(cx.listener(|view, event: &gpui::KeyUpEvent, _, cx| {
                        view.release_captured_key(&event.keystroke.key, cx);
                    }))
                    .size_full()
                    .flex()
                    .flex_col()
                    .when(matches!(self.target, Target::Card(_)), |root| {
                        root.child(
                            div()
                                .px(px(16.0))
                                .py(px(8.0))
                                .child(self.card_reset_control(cx)),
                        )
                    })
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

    fn render_pane(
        &mut self,
        group: Group,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .children(self.status_notes(group, cx));
        if group == Group::General && self.target == Target::Global {
            body = body.child(Form::new(LABEL_WIDTH).rows(self.appearance_rows(cx)));
        }
        if group == Group::Controls {
            if tab == "Bindings" {
                body = body.child(self.render_binding_preview(cx));
            } else {
                body = body.child(self.render_controllers(cx));
            }
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
        let mut view = TabView::new(SharedString::from(format!("pane-{}", group_id(group))))
            .scroll_handle(self.prefs.pane_scroll.clone());
        if tabbed {
            let this = cx.entity().downgrade();
            let tabs = subgroups
                .iter()
                .map(|s| Segment::new(*s, subgroup_label(s)))
                .collect();
            view = view.tabs(tabs, tab).on_select(move |id, _, cx| {
                let id = id.clone();
                let _ = this.update(cx, |view, cx| {
                    if let Some(s) = view
                        .subgroups(group)
                        .into_iter()
                        .find(|s| *s == id.as_ref())
                    {
                        view.cancel_binding_capture(cx);
                        view.prefs.tabs.insert(group, s);
                        view.prefs.pane_scroll = gpui::ScrollHandle::new();
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

    pub(super) fn reveal_binding(&mut self, setting: &'static Descriptor, cx: &mut Context<Self>) {
        self.cancel_binding_capture(cx);
        self.prefs.reveal = Some(setting);
        cx.notify();
    }

    pub(super) fn binding_preview_visible(&self) -> bool {
        self.prefs.navigator.current().and_then(|id| group_from(id)) == Some(Group::Controls)
            && self.prefs.tabs.get(&Group::Controls) == Some(&"Bindings")
    }

    #[cfg(all(test, debug_assertions))]
    pub(super) fn show_bindings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_group(Group::Controls, window, cx);
        self.prefs.tabs.insert(Group::Controls, "Bindings");
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
                Target::Global => {
                    "Defaults for cards and new secondary sessions. Live settings update \
                    mounted cards that inherit the value. Other worker settings apply at the next start."
                        .into()
                }
                Target::Card(card) => format!(
                    "Overrides for card {card}. Unset values inherit Global. Live settings update \
                     the mounted worker. Other worker settings apply at the next start."
                ),
                Target::Secondary(_) =>
                    "Changes apply to this secondary session and expire when it stops.".into(),
            },
            c.text_secondary,
        )];
        if let Some(error) = self.error.clone() {
            out.push(note(error, c.error));
        }
        if let Some(error) = &self.report_error {
            let context = if matches!(self.target, Target::Secondary(_)) {
                "Could not read temporary settings."
            } else {
                "Could not read saved settings."
            };
            out.push(note(format!("{context} {error}"), c.error));
        } else if self.host.is_some() && self.report.is_none() {
            out.push(note("Loading saved settings…".into(), c.text_secondary));
        }
        if self.saved_read_only() {
            out.push(note(
                "The saved settings file could not be read. Its values are read-only.".into(),
                c.warning,
            ));
        }
        if self.host.is_none() {
            out.push(note(
                "No host configuration: only desktop preferences can be changed.".into(),
                c.warning,
            ));
        } else if !self.control {
            out.push(note(
                "This host is read-only (allow_control is off): only desktop preferences can be changed.".into(),
                c.warning,
            ));
        }
        if !self
            .lotus
            .read_with(cx, |l, _| l.preferences_saved())
            .unwrap_or(true)
        {
            out.push(note(
                "desktop.json has no location or could not be read: preference changes last until Lotus closes."
                    .into(),
                c.warning,
            ));
        }
        if group == Group::McpAutomation {
            if let Ok(text) = self.lotus.read_with(cx, |l, _| l.mcp_line()) {
                out.push(note(text.to_string(), c.text_secondary));
            }
        }
        if group == Group::Discord {
            let palette = theme::palette(cx);
            if let Some((_, text)) = self
                .lotus
                .read_with(cx, |l, _| l.presence_line(palette))
                .ok()
                .flatten()
            {
                out.push(note(text.to_string(), c.text_secondary));
            }
        }
        out
    }

    fn card_reset_control(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let target = self.target.clone();
        let confirm = self.prefs.reset_card.as_ref() == Some(&target);
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(prefs::CONTROL_GAP))
            .when(!confirm, |row| {
                row.child(
                    Button::new("reset-card-overrides", "Reset Card Overrides…")
                        .disabled(!self.saved_editable())
                        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                            if view.target == target {
                                view.prefs.reset_card = Some(target.clone());
                                cx.notify();
                            }
                        })),
                )
            })
            .when(confirm, |row| {
                row.child("Clear all saved overrides for this card?")
                    .child(
                        Button::new("cancel-reset-card", "Cancel").on_click(cx.listener(
                            |view, _: &ClickEvent, _, cx| {
                                view.prefs.reset_card = None;
                                cx.notify();
                            },
                        )),
                    )
                    .child(
                        Button::new("confirm-reset-card", "Reset to Global")
                            .disabled(!self.saved_editable())
                            .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                                if let Some(target) = view.prefs.reset_card.take() {
                                    view.reset_card(target, cx);
                                }
                                cx.notify();
                            })),
                    )
            })
    }

    fn appearance_rows(&self, cx: &mut Context<Self>) -> Vec<FormRow> {
        use silvestris::{Appearance, ThemeFamily, ThemeId};
        let id = self
            .lotus
            .read_with(cx, |view, _| view.ui_theme())
            .unwrap_or(ThemeId::GEIST_DARK);
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
                .selected(if id.family == ThemeFamily::Aqua {
                    "aqua"
                } else {
                    "geist"
                })
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
                .selected(if id.appearance == Appearance::Light {
                    "light"
                } else {
                    "dark"
                })
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
    pub(super) fn form_row(
        &mut self,
        descriptor: &'static Descriptor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> FormRow {
        let (value, source) = self.value(descriptor, cx);
        let editable = self.editable(descriptor, &source);
        let binding = matches!(descriptor.kind, Kind::Bindings { .. });
        let ready = !binding
            || (self.binding_editor.layout.is_some()
                && !self.binding_editor.pending.contains(&self.target));
        let overridden = match descriptor.owner {
            Owner::Desktop => value != descriptor.default,
            _ => source == self.target.layer(),
        };
        let key = descriptor.key.clone();
        let card_value = matches!(self.target, Target::Card(_))
            && descriptor.owner.saved_in_layers()
            && source != "managed";
        let enabled = ready && editable.is_ok() && (!card_value || overridden);
        let mut control = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(prefs::CONTROL_GAP));
        let mut labelled = true;
        match &descriptor.kind {
            _ if key == "lotus.input.preset" => {
                let current = value.as_str().unwrap_or(lotus_input::layout::BUILTIN);
                let mut presets: Vec<String> = lotus_input::layout::builtins()
                    .into_iter()
                    .map(|preset| preset.name)
                    .chain(self.presets.iter().cloned())
                    .collect();
                let missing = !presets.iter().any(|name| name == current);
                if missing {
                    presets.push(current.to_string());
                }
                let selected = presets.iter().position(|name| name == current);
                let items = presets
                    .iter()
                    .map(|name| {
                        if missing && name == current {
                            SharedString::from(format!("{name} (missing)"))
                        } else {
                            SharedString::from(name.clone())
                        }
                    })
                    .collect();
                control = control.child(
                    PopUpButton::new("input-preset", descriptor.label.clone(), items)
                        .selected(selected)
                        .disabled(!enabled)
                        .min_width(140.0)
                        .on_choose(cx.listener(move |view, index: &usize, _, cx| {
                            if let Some(name) = presets.get(*index) {
                                view.write(descriptor, Some(Value::String(name.clone())), cx);
                            }
                        })),
                );
            }
            Kind::Boolean => {
                labelled = false;
                control = control.child(
                    Checkbox::new(
                        SharedString::from(format!("toggle-{key}")),
                        descriptor.label.clone(),
                    )
                    .checked(value.as_bool().unwrap_or(false))
                    .disabled(!enabled)
                    .on_change(cx.listener(
                        move |view, state: &CheckState, _, cx| {
                            view.write(descriptor, Some(Value::Bool(*state == CheckState::On)), cx);
                        },
                    )),
                );
            }
            kind if choices(kind).is_some() => {
                let options = choices(kind).unwrap_or_default();
                let items = options
                    .iter()
                    .map(|o| SharedString::from(choice_text(descriptor, o)))
                    .collect();
                let selected = options.iter().position(|o| *o == value);
                control = control.child(
                    PopUpButton::new(
                        SharedString::from(format!("choice-{key}")),
                        descriptor.label.clone(),
                        items,
                    )
                    .selected(selected)
                    .disabled(!enabled)
                    .min_width(140.0)
                    .on_choose(cx.listener(
                        move |view, index: &usize, _, cx| {
                            if let Some(choice) = options.get(*index) {
                                view.write(descriptor, Some(choice.clone()), cx);
                            }
                        },
                    )),
                );
            }
            kind if stepper(kind).is_some() => {
                let (step, min, max, integer) = stepper(kind).unwrap_or((1.0, None, None, true));
                let current = value.as_f64().unwrap_or(0.0);
                let field = self.value_field(descriptor, &value, enabled, window, cx);
                let clamp = move |next: f64| {
                    let mut next = next;
                    if let Some(min) = min {
                        next = next.max(min);
                    }
                    if let Some(max) = max {
                        next = next.min(max);
                    }
                    if integer {
                        Value::from(next.round() as i64)
                    } else {
                        Value::from((next * 1e4).round() / 1e4)
                    }
                };
                control = control.child(div().w(px(80.0)).child(field)).child(
                    Stepper::new(
                        SharedString::from(format!("stepper-{key}")),
                        descriptor.label.clone(),
                    )
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
            Kind::Bindings { .. } => {
                control = control.child(self.render_binding(descriptor, enabled, cx));
            }
            Kind::String { .. } | Kind::Json { .. } => {
                let field = self.value_field(descriptor, &value, enabled, window, cx);
                control = control.child(div().w(px(260.0)).child(field));
            }
            _ => {
                control = control.child(div().child(display(&value)));
            }
        }
        if overridden && enabled {
            control =
                control.child(
                    Button::new(
                        SharedString::from(format!("reset-{key}")),
                        reset_label(&self.target, descriptor),
                    )
                    .size(ControlSize::Mini)
                    .on_click(cx.listener(
                        move |view, _: &ClickEvent, _, cx| view.write(descriptor, None, cx),
                    )),
                );
        } else if ready && card_value && editable.is_ok() {
            control = control.child(
                Button::new(SharedString::from(format!("override-{key}")), "Override")
                    .size(ControlSize::Mini)
                    .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                        let (value, source) = view.value(descriptor, cx);
                        if matches!(view.target, Target::Card(_)) && source != view.target.layer() {
                            let value = if binding {
                                serde_json::json!(view.bindings(descriptor))
                            } else {
                                value
                            };
                            view.write(descriptor, Some(value), cx);
                        }
                    })),
            );
        }
        if self.prefs.reveal.is_some_and(|setting| setting.key == key) {
            self.prefs.reveal = None;
            let scroll = self.prefs.pane_scroll.clone();
            control = control.relative().child(
                gpui::canvas(
                    move |bounds, window, cx| {
                        let scroll = scroll.clone();
                        window.defer(cx, move |window, _| {
                            let offset = scroll.offset();
                            let y = (offset.y + scroll.bounds().top() - bounds.top())
                                .clamp(-scroll.max_offset().y, px(0.0));
                            scroll.set_offset(gpui::point(offset.x, y));
                            window.refresh();
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            );
        }
        if let Some(error) = self.prefs.field_errors.get(&key) {
            let error_id = format!("invalid-{key}");
            control = div()
                .flex()
                .flex_col()
                .w_full()
                .min_w_0()
                .gap(px(2.0))
                .child(control)
                .child(
                    div()
                        .id(SharedString::from(error_id.clone()))
                        .debug_selector(move || error_id.clone())
                        .text_size(px(cx.theme().type_.size_small))
                        .text_color(silvestris::convert::hsla(cx.theme().colors.error))
                        .child(error.clone()),
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
        let mut note = match editable {
            Err(reason) if !(window_wide && descriptor.owner.saved_in_layers()) => {
                format!("{} {reason}", descriptor.description)
            }
            _ => descriptor.description.clone(),
        };
        if binding && self.binding_editor.layout.is_some() {
            let source = self.binding_source(descriptor);
            note = match editable {
                Err(reason) if !window_wide => format!("{source}. {reason}"),
                _ => source,
            };
        } else if card_value && self.report.is_some() {
            note.push_str(if overridden {
                " Overridden for this card."
            } else {
                " Inherited from Global."
            });
        }
        if let Some(pending) = self.pending_note(&key) {
            if !note.is_empty() {
                note.push(' ');
            }
            note.push_str(&pending);
        } else if matches!(
            descriptor.apply,
            lotus_settings::Apply::Renderer
                | lotus_settings::Apply::Boot
                | lotus_settings::Apply::CfgSavedata
        ) {
            note.push_str(" Changes apply at the next start.");
        }
        if !note.trim().is_empty() {
            row = row.note(note);
        }
        row.id(SharedString::from(format!("row-{key}")))
    }

    /// Text, numeric and structured values share the same editor lifecycle.
    fn value_field(
        &mut self,
        descriptor: &'static Descriptor,
        value: &Value,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TextField> {
        let shown = field_text(descriptor, value);
        let field = match self.prefs.fields.get(&descriptor.key) {
            Some(field) => field.clone(),
            None => {
                let field = cx.new(|cx| {
                    let mut f = TextField::new(window, cx);
                    f.set_size(ControlSize::Small, cx);
                    f.set_label(descriptor.label.clone(), cx);
                    if matches!(descriptor.kind, Kind::Json { .. }) {
                        f.set_placeholder("JSON value", cx);
                    }
                    f.set_text(shown.clone(), cx);
                    f
                });
                let dirty = std::cell::Cell::new(false);
                let target = self.target.clone();
                cx.subscribe(&field, move |view, field, event: &TextFieldEvent, cx| {
                    if view.target != target
                        || view.prefs.fields.get(&descriptor.key) != Some(&field)
                    {
                        return;
                    }
                    if *event == TextFieldEvent::Changed {
                        dirty.set(true);
                        if view.prefs.field_errors.remove(&descriptor.key).is_some() {
                            cx.notify();
                        }
                        return;
                    }
                    if !dirty.replace(false) {
                        return;
                    }
                    let (current, source) = view.value(descriptor, cx);
                    if view.editable(descriptor, &source).is_err() {
                        return;
                    }
                    let input = field.read(cx).text();
                    match parse_field(descriptor, input) {
                        Ok(next) if next != current => view.write(descriptor, Some(next), cx),
                        Ok(_) => {}
                        Err(error) => {
                            view.prefs
                                .field_errors
                                .insert(descriptor.key.clone(), error);
                            cx.notify();
                        }
                    }
                })
                .detach();
                self.prefs
                    .fields
                    .insert(descriptor.key.clone(), field.clone());
                field
            }
        };
        let focused = field.read(cx).focus_handle(cx).is_focused(window);
        let invalid = self.prefs.field_errors.contains_key(&descriptor.key);
        field.update(cx, |f, cx| {
            if (!focused && !invalid || !enabled) && f.text() != shown {
                f.set_text(shown, cx);
            }
            f.set_disabled(!enabled, window, cx);
        });
        field
    }
}

fn field_text(descriptor: &Descriptor, value: &Value) -> String {
    // Host rows are read-only: show paths as text and handle maps as `handle = path`.
    if descriptor.owner == Owner::Host && matches!(descriptor.kind, Kind::Json { .. }) {
        let path = |value: &Value| {
            value
                .as_str()
                .or_else(|| value["path"].as_str())
                .map_or_else(|| value.to_string(), str::to_owned)
        };
        return match value {
            Value::Object(map) if map.is_empty() => "None".into(),
            Value::Object(map) => map
                .iter()
                .map(|(handle, value)| format!("{handle} = {}", path(value)))
                .collect::<Vec<_>>()
                .join("; "),
            other => path(other),
        };
    }
    match &descriptor.kind {
        Kind::Integer { nullable: true, .. } if value.is_null() => String::new(),
        Kind::String { .. } => value.as_str().unwrap_or_default().to_owned(),
        Kind::Json { .. } | Kind::Bindings { .. } => value.to_string(),
        _ => display(value),
    }
}

fn parse_field(descriptor: &Descriptor, text: &str) -> Result<Value, String> {
    let value = match &descriptor.kind {
        Kind::String { .. } => Value::String(text.to_owned()),
        Kind::Json { .. } | Kind::Bindings { .. } => {
            serde_json::from_str(text).map_err(|error| format!("{}: {error}", descriptor.label))?
        }
        Kind::Integer { nullable: true, .. } if text.trim().is_empty() => Value::Null,
        Kind::Integer { .. } => Value::from(
            text.trim()
                .parse::<i64>()
                .map_err(|_| format!("{} requires a whole number.", descriptor.label))?,
        ),
        Kind::Float { .. } => Value::from(
            text.trim()
                .parse::<f64>()
                .ok()
                .filter(|number| number.is_finite())
                .ok_or_else(|| format!("{} requires a finite number.", descriptor.label))?,
        ),
        _ => return Err("Unsupported editor".into()),
    };
    descriptor
        .validate(&value)
        .map_err(|error| error.to_string())?;
    Ok(value)
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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Focusable, TestAppContext};

    #[test]
    fn host_locations_read_as_paths_and_handles() {
        let registry = Registry::builtin();
        let text = |key: &str, value: Value| field_text(registry.get(key).unwrap(), &value);
        assert_eq!(
            text("lotus.host.storage_root", Value::from(r"D:\lotus\data")),
            r"D:\lotus\data"
        );
        assert_eq!(text("lotus.host.assets", serde_json::json!({})), "None");
        assert_eq!(
            text(
                "lotus.host.source_roots",
                serde_json::json!({"game": {"path": "C:/src", "from": "/opt/build"},
                    "lib": {"path": "C:/lib", "from": null}})
            ),
            "game = C:/src; lib = C:/lib"
        );
    }

    #[test]
    fn scalar_and_structured_editors_use_registry_validation() {
        let string = Registry::builtin()
            .descriptors()
            .iter()
            .find(|descriptor| {
                matches!(
                    descriptor.kind,
                    Kind::String {
                        choices: None,
                        hex: false,
                        exact_len: None,
                        ..
                    }
                )
            })
            .expect("string setting");
        assert_eq!(
            parse_field(string, "test").unwrap(),
            Value::String("test".into())
        );
        let bindings = Registry::builtin()
            .descriptors()
            .iter()
            .find(|descriptor| matches!(descriptor.kind, Kind::Bindings { .. }))
            .expect("input binding setting");
        assert_eq!(
            parse_field(bindings, "[]").unwrap(),
            Value::Array(Vec::new())
        );
        assert!(parse_field(bindings, "{}").is_err());
        assert!(parse_field(bindings, "[").is_err());
    }

    #[test]
    #[cfg(debug_assertions)]
    fn invalid_text_stays_beside_the_control_until_corrected() {
        let mut app = TestAppContext::single();
        app.update(|cx| {
            silvestris::test_support::initialize(silvestris::ThemePreference::default(), cx)
        });
        let (main, _) = app.add_window_view(LotusWindow::test_preview);
        let (settings, cx) = app.add_window_view(|window, cx| {
            let mut view =
                SettingsWindow::new(main.downgrade(), None, false, Target::Global, window, cx);
            view.show_group(Group::Discord, window, cx);
            view
        });
        let key = "lotus.desktop.discord_application_id";
        let invalid = "invalid-lotus.desktop.discord_application_id";
        let draw = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| window.draw(cx).clear(cx));
        };
        cx.simulate_resize(gpui::size(px(668.), px(480.)));
        cx.simulate_scale_factor_change(1.75);
        draw(cx);
        cx.update(|window, cx| {
            let field = settings.read(cx).prefs.fields[key].clone();
            window.focus(&field.focus_handle(cx), cx);
        });
        let too_long = "1234567890".repeat(4);
        cx.simulate_input(&too_long);
        cx.simulate_keystrokes("enter");
        draw(cx);
        let error = cx.debug_bounds(invalid).unwrap();
        let row = cx
            .debug_bounds("row-lotus.desktop.discord_application_id")
            .unwrap();
        assert!(error.left() >= row.left() && error.right() <= row.right());
        cx.update(|_, cx| {
            let view = settings.read(cx);
            assert_eq!(view.prefs.fields[key].read(cx).text(), too_long);
            assert_eq!(main.read(cx).preference(key), Value::Null);
            assert!(view.error.is_none());
        });
        // A rejected draft is local to its row and survives a window resize.
        cx.simulate_resize(gpui::size(px(1024.), px(640.)));
        draw(cx);
        let error = cx.debug_bounds(invalid).unwrap();
        let row = cx
            .debug_bounds("row-lotus.desktop.discord_application_id")
            .unwrap();
        assert!(error.left() >= row.left() && error.right() <= row.right());
        cx.simulate_keystrokes("ctrl-a");
        cx.simulate_input("123456789012345678");
        cx.simulate_keystrokes("enter");
        draw(cx);
        assert!(cx.debug_bounds(invalid).is_none());
        assert_eq!(
            cx.update(|_, cx| main.read(cx).preference(key)),
            Value::from("123456789012345678")
        );
    }

    #[test]
    fn typing_and_escape_filter_settings_without_changing_the_pane() {
        let mut app = TestAppContext::single();
        app.update(|cx| {
            silvestris::test_support::initialize(silvestris::ThemePreference::default(), cx)
        });
        let (main, _) = app.add_window_view(LotusWindow::test_preview);
        let (settings, cx) = app.add_window_view(|window, cx| {
            SettingsWindow::new(main.downgrade(), None, false, Target::Global, window, cx)
        });
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let search = settings.read(cx).prefs.search.as_ref().unwrap().clone();
            window.focus(&search.read(cx).focus_handle(cx), cx);
        });
        cx.simulate_keystrokes("d i s c o r d");
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let view = settings.read(cx);
            assert_eq!(
                view.prefs.search.as_ref().unwrap().read(cx).text(),
                "discord"
            );
            assert!(view.prefs.navigator.current().is_none());
        });
        assert!(cx.debug_bounds("settings-search-results").is_some());
        assert!(cx.debug_bounds("preference-pane-general").is_none());
        cx.simulate_keystrokes("escape");
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("settings-search-results").is_none());
        assert!(cx.debug_bounds("preference-pane-general").is_some());
        let key = "use_hw_shader";
        let descriptor = Registry::builtin().get(key).unwrap();
        cx.update(|window, cx| {
            let search = settings.read(cx).prefs.search.as_ref().unwrap().clone();
            search.update(cx, |field, cx| field.set_text(key, cx));
            window.draw(cx).clear(cx);
        });
        assert!(cx.debug_bounds("toggle-use_hw_shader").is_none());
        let found = cx.debug_bounds("find-use_hw_shader").unwrap();
        cx.simulate_click(found.center(), gpui::Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let view = settings.read(cx);
            assert_eq!(view.group, descriptor.group);
            assert_eq!(view.target, Target::Global);
            assert_eq!(
                view.prefs.tabs.get(&descriptor.group),
                Some(&descriptor.subgroup)
            );
        });
        assert!(cx.debug_bounds("settings-search-results").is_none());
        let row = cx.debug_bounds("row-use_hw_shader").unwrap();
        let viewport = cx.debug_bounds("tab-view-box").unwrap();
        assert!(row.top() >= viewport.top() && row.top() < viewport.bottom());
    }

    #[test]
    #[cfg(debug_assertions)]
    fn mounted_card_override_and_confirmed_reset_leave_global_settings_intact() {
        use lotus_protocol::envelope::SessionUuid;
        use lotus_session::supervisor::HostConfig;

        let storage =
            std::env::temp_dir().join(format!("lotus-settings-ui-{}", SessionUuid::generate()));
        std::fs::create_dir_all(storage.join("cards/dev")).unwrap();
        let mut config = HostConfig::new(std::env::current_exe().unwrap(), &storage);
        config.allow_control = true;
        let host = Arc::new(Supervisor::new(config).unwrap());
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let key = "use_hw_shader";
        let set_global = |value| {
            runtime
                .block_on(host.set_setting(
                    &SettingsTarget::Global,
                    key,
                    Some(Value::Bool(value)),
                    Actor::Mcp,
                ))
                .unwrap()
        };
        set_global(false);
        let mut app = TestAppContext::single();
        app.update(|cx| {
            silvestris::test_support::initialize(silvestris::ThemePreference::default(), cx)
        });
        let (main, _) = app.add_window_view(LotusWindow::test_preview);
        let (settings, cx) = app.add_window_view(|window, cx| {
            SettingsWindow::new(
                main.downgrade(),
                Some(host.clone()),
                true,
                Target::Card("dev".into()),
                window,
                cx,
            )
        });
        let descriptor = Registry::builtin().get(key).unwrap();
        let draw = |cx: &mut gpui::VisualTestContext| {
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
        };
        let wait_for = |cx: &mut gpui::VisualTestContext, want: &str, value: bool| {
            for _ in 0..100 {
                draw(cx);
                let source = cx.update(|_, cx| {
                    let view = settings.read(cx);
                    view.report.as_ref().map(|_| view.value(descriptor, cx))
                });
                if source.as_ref().is_some_and(|(current, source)| {
                    current == &Value::Bool(value) && source == want
                }) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("timed out waiting for {want} level");
        };
        wait_for(cx, "global", false);
        let click = |cx: &mut gpui::VisualTestContext, selector| {
            let bounds = cx.debug_bounds(selector).unwrap();
            cx.simulate_click(bounds.center(), gpui::Modifiers::default());
            draw(cx);
        };
        cx.update(|window, cx| {
            settings.update(cx, |view, cx| {
                view.show_group(descriptor.group, window, cx);
                view.prefs
                    .tabs
                    .insert(descriptor.group, descriptor.subgroup);
                view.prefs.reveal = Some(descriptor);
            });
        });
        draw(cx);
        draw(cx);
        click(cx, "toggle-use_hw_shader");
        wait_for(cx, "global", false);
        click(cx, "override-use_hw_shader");
        wait_for(cx, "card", false);
        click(cx, "toggle-use_hw_shader");
        wait_for(cx, "card", true);
        click(cx, "reset-use_hw_shader");
        wait_for(cx, "global", false);
        click(cx, "override-use_hw_shader");
        wait_for(cx, "card", false);
        click(cx, "reset-card-overrides");
        click(cx, "cancel-reset-card");
        wait_for(cx, "card", false);
        click(cx, "reset-card-overrides");
        click(cx, "confirm-reset-card");
        wait_for(cx, "global", false);
        set_global(true);
        cx.update(|_, cx| settings.update(cx, |view, cx| view.refresh(cx)));
        wait_for(cx, "global", true);
        let _ = std::fs::remove_dir_all(storage);
    }
    /// lotus#56: a card overrides a global debugger default through the mounted pop-up,
    /// including an explicit "Worker default" (null), and the saved card file holds it.
    #[gpui::test]
    fn card_debugger_default_overrides_global_through_the_pop_up() {
        use lotus_protocol::envelope::SessionUuid;
        use lotus_session::supervisor::HostConfig;

        let storage =
            std::env::temp_dir().join(format!("lotus-settings-debug-{}", SessionUuid::generate()));
        std::fs::create_dir_all(storage.join("cards/dev")).unwrap();
        let mut config = HostConfig::new(std::env::current_exe().unwrap(), &storage);
        config.allow_control = true;
        let host = Arc::new(Supervisor::new(config).unwrap());
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let key = "lotus.debug.gpu.capture";
        runtime
            .block_on(host.set_setting(
                &SettingsTarget::Global,
                key,
                Some(Value::from("events")),
                Actor::Mcp,
            ))
            .unwrap();
        let mut app = TestAppContext::single();
        app.update(|cx| {
            silvestris::test_support::initialize(silvestris::ThemePreference::default(), cx)
        });
        let (main, _) = app.add_window_view(LotusWindow::test_preview);
        let (settings, cx) = app.add_window_view(|window, cx| {
            SettingsWindow::new(
                main.downgrade(),
                Some(host.clone()),
                true,
                Target::Card("dev".into()),
                window,
                cx,
            )
        });
        let descriptor = Registry::builtin().get(key).unwrap();
        let draw = |cx: &mut gpui::VisualTestContext| {
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
        };
        let wait_for = |cx: &mut gpui::VisualTestContext, want: &str, value: Value| {
            for _ in 0..100 {
                draw(cx);
                let current = cx.update(|_, cx| {
                    let view = settings.read(cx);
                    view.report.as_ref().map(|_| view.value(descriptor, cx))
                });
                if current
                    .as_ref()
                    .is_some_and(|(current, source)| current == &value && source == want)
                {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("timed out waiting for {value} from {want}");
        };
        wait_for(cx, "global", Value::from("events"));
        // `debug_bounds` takes a static selector; the test leaks a few short strings.
        let click = |cx: &mut gpui::VisualTestContext, selector: &str| {
            let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
            let bounds = cx.debug_bounds(selector).unwrap();
            cx.simulate_click(bounds.center(), gpui::Modifiers::default());
            draw(cx);
        };
        cx.update(|window, cx| {
            settings.update(cx, |view, cx| {
                view.show_group(descriptor.group, window, cx);
                view.prefs
                    .tabs
                    .insert(descriptor.group, descriptor.subgroup);
                view.prefs.reveal = Some(descriptor);
            });
        });
        draw(cx);
        draw(cx);
        let choice = format!("choice-{key}");
        click(cx, &format!("override-{key}"));
        wait_for(cx, "card", Value::from("events"));
        // Items: Worker default, counters, events, draws, full.
        click(cx, &choice);
        click(cx, "popup-item-1");
        wait_for(cx, "card", Value::from("counters"));
        click(cx, &choice);
        click(cx, "popup-item-0");
        wait_for(cx, "card", Value::Null);
        let saved: Value = serde_json::from_slice(
            &std::fs::read(storage.join("cards/dev/lotus-settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(saved["values"][key], Value::Null, "{saved}");
        assert!(saved["values"].get(key).is_some(), "{saved}");
        click(cx, &format!("reset-{key}"));
        wait_for(cx, "global", Value::from("events"));
        let _ = std::fs::remove_dir_all(storage);
    }

    /// lotus#56, lotus#58 (headless parity), lotus#154 (settings tools): one host.json,
    /// parsed as `lotus-desktop` and `lotus-mcp` parse it, backs the mounted Settings
    /// window and a separate headless server on the same storage root. Host rows match
    /// what the server enforces, each side reads the other's saved changes and presets,
    /// and the window's rows are the schema the server publishes.
    #[gpui::test]
    fn settings_window_matches_a_headless_server_on_the_same_host_configuration() {
        use lotus_mcp::tool_arguments::{
            InputPresetArgs, InputPresetOperation, SettingsArgs, SettingsChangeArgs,
            SettingsOperation, SettingsSchemaArgs, SettingsScope, SettingsTargetArg,
        };
        use lotus_mcp::LotusServer;
        use lotus_protocol::envelope::SessionUuid;
        use lotus_session::supervisor::HostConfig;
        use rmcp::handler::server::wrapper::Parameters;

        let storage = std::env::temp_dir()
            .join(format!("lotus-settings-parity-{}", SessionUuid::generate()));
        std::fs::create_dir_all(storage.join("cards/dev")).unwrap();
        let mut config = HostConfig::new(std::env::current_exe().unwrap(), &storage);
        config.allow_control = true;
        config.max_sessions = 3;
        config.policy.gpu_capture_sessions = Some(0);
        config.policy.snapshot_slots = Some(3);
        let path = storage.join("host.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
        let parse = || {
            serde_json::from_str::<HostConfig>(&std::fs::read_to_string(&path).unwrap()).unwrap()
        };
        let host = Arc::new(Supervisor::new(parse()).unwrap());
        let headless = LotusServer::configured(parse(), Default::default()).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let tool = |result: rmcp::model::CallToolResult| -> Value {
            let text = serde_json::to_value(&result.content[0]).unwrap()["text"]
                .as_str()
                .unwrap()
                .to_string();
            assert_ne!(result.is_error, Some(true), "{text}");
            serde_json::from_str(&text).unwrap()
        };
        let read = |target: SettingsTargetArg| {
            tool(runtime.block_on(headless.lotus_settings(Parameters(SettingsArgs { target }))))
        };
        let change = |target, operation, key: &str, value: Option<Value>| {
            tool(
                runtime.block_on(headless.lotus_settings_change(Parameters(SettingsChangeArgs {
                    target,
                    operation,
                    key: Some(key.into()),
                    value,
                }))),
            )
        };
        let card = || SettingsTargetArg::Card { card: "dev".into() };

        let mut app = TestAppContext::single();
        app.update(|cx| {
            silvestris::test_support::initialize(silvestris::ThemePreference::default(), cx)
        });
        let (main, _) = app.add_window_view(LotusWindow::test_preview);
        let (settings, cx) = app.add_window_view(|window, cx| {
            SettingsWindow::new(
                main.downgrade(),
                Some(host.clone()),
                true,
                Target::Global,
                window,
                cx,
            )
        });
        let draw = |cx: &mut gpui::VisualTestContext| {
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
        };
        // The window's report once its settings equal `want`'s, or the last one seen.
        let shown = |cx: &mut gpui::VisualTestContext, want: &Value| {
            let mut last = Value::Null;
            for _ in 0..100 {
                draw(cx);
                last = cx.update(|_, cx| settings.read(cx).report.clone().unwrap_or_default());
                if last["settings"] == want["settings"] {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            last
        };

        // Schema: every row the window offers at a scope, in order, and nothing else.
        for (target, scope) in [
            (Target::Global, SettingsScope::Global),
            (Target::Card("dev".into()), SettingsScope::Primary),
        ] {
            let schema = tool(runtime.block_on(headless.lotus_settings_schema(Parameters(
                SettingsSchemaArgs { scope: Some(scope) },
            ))));
            cx.update(|_, cx| {
                settings.update(cx, |view, cx| view.set_target(target.clone(), cx));
            });
            draw(cx);
            for group in schema["groups"].as_array().unwrap() {
                let published: Vec<&str> = group["settings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| s["key"].as_str().unwrap())
                    .filter(|key| Registry::builtin().get(key).unwrap().presentation().has_row())
                    .collect();
                let group = Group::ALL
                    .into_iter()
                    .find(|g| serde_json::to_value(g).unwrap() == group["group"])
                    .unwrap();
                let rows: Vec<String> = cx.update(|_, cx| {
                    settings
                        .read(cx)
                        .rows(group)
                        .iter()
                        .map(|d| d.key.clone())
                        .collect()
                });
                assert_eq!(rows, published, "{target:?} {group:?}");
            }
        }
        cx.update(|_, cx| settings.update(cx, |view, cx| view.set_target(Target::Global, cx)));

        // Host rows: the same parsed configuration, and what the headless server enforces.
        let status: Value =
            serde_json::from_str(&runtime.block_on(headless.lotus_status())).unwrap();
        let policy = &status["policy"];
        let headless_config = serde_json::to_value(headless.host().unwrap().config()).unwrap();
        let limit_note = |limit: &Value| match limit.as_u64() {
            Some(limit) => format!("In effect: {limit}."),
            None => "In effect: unlimited.".to_string(),
        };
        // (value the server enforces) or (the note the row must show for an unset budget).
        let enforced = |key: &str| -> Option<Result<Value, String>> {
            Some(match key {
                "lotus.host.max_sessions" => Ok(status["max_sessions"].clone()),
                "lotus.host.allow_control" => Ok(status["control"].clone()),
                "lotus.host.policy.workload" => Ok(policy["workload"].clone()),
                "lotus.host.policy.focus_priority" => Ok(policy["focus_priority"].clone()),
                "lotus.host.policy.snapshot_queue_limit" => {
                    Ok(policy["snapshots"]["queue_limit"].clone())
                }
                "lotus.host.policy.memory_reserve_mib" => {
                    Ok(policy["memory"]["reserve_mib"].clone())
                }
                "lotus.host.artifacts.max_count" => Ok(status["artifacts"]["max_count"].clone()),
                "lotus.host.artifacts.max_bytes" => Ok(status["artifacts"]["max_bytes"].clone()),
                "lotus.host.artifacts.lease_seconds" => {
                    Ok(status["artifacts"]["lease_seconds"].clone())
                }
                "lotus.host.policy.rewind_sessions" => Err(limit_note(&policy["rewind"]["limit"])),
                "lotus.host.policy.rewind_memory_mib" => {
                    Err(limit_note(&policy["rewind"]["memory_limit_mib"]))
                }
                "lotus.host.policy.gpu_capture_sessions" => {
                    Err(limit_note(&policy["gpu_capture"]["limit"]))
                }
                "lotus.host.policy.snapshot_slots" => {
                    Err(limit_note(&policy["snapshots"]["limit"]))
                }
                _ => return None,
            })
        };
        let mut compared = 0;
        for descriptor in Registry::builtin()
            .descriptors()
            .iter()
            .filter(|d| d.owner == Owner::Host)
        {
            let (value, note) = cx.update(|_, cx| {
                let view = settings.read(cx);
                (
                    view.value(descriptor, cx).0,
                    view.pending_note(&descriptor.key),
                )
            });
            let mut parsed = &headless_config;
            for part in descriptor.key.strip_prefix("lotus.host.").unwrap().split('.') {
                parsed = &parsed[part];
            }
            assert_eq!(&value, parsed, "{}", descriptor.key);
            match enforced(&descriptor.key) {
                Some(Ok(effective)) => assert_eq!(value, effective, "{}", descriptor.key),
                Some(Err(effective)) => {
                    assert_eq!(note.as_deref(), Some(effective.as_str()), "{}", descriptor.key)
                }
                None => continue,
            }
            compared += 1;
        }
        assert_eq!(compared, 13);

        // Saved settings: a desktop edit is what the headless server reads, and back.
        let global = read(SettingsTargetArg::Global);
        assert_eq!(shown(cx, &global)["settings"], global["settings"]);
        let key = "use_hw_shader";
        let descriptor = Registry::builtin().get(key).unwrap();
        let before = global["settings"][key]["value"].as_bool().unwrap();
        cx.update(|window, cx| {
            settings.update(cx, |view, cx| {
                view.show_group(descriptor.group, window, cx);
                view.prefs.tabs.insert(descriptor.group, descriptor.subgroup);
                view.prefs.reveal = Some(descriptor);
            });
        });
        draw(cx);
        draw(cx);
        let bounds = cx.debug_bounds("toggle-use_hw_shader").unwrap();
        cx.simulate_click(bounds.center(), gpui::Modifiers::default());
        let mut global = read(SettingsTargetArg::Global);
        for _ in 0..100 {
            if global["settings"][key]["value"] == Value::Bool(!before) {
                break;
            }
            draw(cx);
            std::thread::sleep(std::time::Duration::from_millis(10));
            global = read(SettingsTargetArg::Global);
        }
        assert_eq!(global["settings"][key]["source"], "global", "{global}");
        assert_eq!(global["settings"][key]["value"], !before, "{global}");
        assert_eq!(shown(cx, &global)["settings"], global["settings"]);

        let interval = "lotus.debug.rewind.interval_frames";
        let capture = "lotus.debug.gpu.capture";
        change(
            SettingsTargetArg::Global,
            SettingsOperation::Set,
            capture,
            Some("events".into()),
        );
        let changed = change(card(), SettingsOperation::Set, interval, Some(7.into()));
        assert_eq!(changed["settings"][interval]["source"], "card", "{changed}");
        cx.update(|_, cx| {
            settings.update(cx, |view, cx| {
                view.set_target(Target::Card("dev".into()), cx);
            });
        });
        let on_card = read(card());
        assert_eq!(on_card["settings"][capture]["source"], "global", "{on_card}");
        assert_eq!(shown(cx, &on_card)["settings"], on_card["settings"]);
        for (key, source) in [(interval, "card"), (capture, "global"), (key, "global")] {
            let descriptor = Registry::builtin().get(key).unwrap();
            let (value, from) = cx.update(|_, cx| settings.read(cx).value(descriptor, cx));
            assert_eq!(value, on_card["settings"][key]["value"], "{key}");
            assert_eq!(from, source, "{key}");
        }
        change(card(), SettingsOperation::Reset, interval, None);
        let reset = read(card());
        assert_eq!(reset["settings"][interval]["source"], "default", "{reset}");
        cx.update(|_, cx| settings.update(cx, |view, cx| view.refresh(cx)));
        assert_eq!(shown(cx, &reset)["settings"], reset["settings"]);

        // Presets: a preset duplicated headless is offered by the window's preset control.
        let presets = |operation, name: Option<&str>, to: Option<&str>| InputPresetArgs {
            operation,
            name: name.map(str::to_string),
            to: to.map(str::to_string),
            json: None,
        };
        tool(runtime.block_on(headless.lotus_input_presets(Parameters(presets(
            InputPresetOperation::Duplicate,
            Some("xbox-labels"),
            Some("pad-b"),
        )))));
        let listed = tool(runtime.block_on(
            headless.lotus_input_presets(Parameters(presets(InputPresetOperation::List, None, None))),
        ));
        assert_eq!(listed["saved"], serde_json::json!(["pad-b"]));
        cx.update(|_, cx| settings.update(cx, |view, cx| view.refresh(cx)));
        draw(cx);
        let offered = cx.update(|_, cx| settings.read(cx).presets.clone());
        assert_eq!(serde_json::to_value(&offered).unwrap(), listed["saved"]);

        drop(headless);
        let _ = std::fs::remove_dir_all(storage);
    }
}
