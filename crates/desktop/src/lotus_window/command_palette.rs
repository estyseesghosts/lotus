use gpui::{Entity, Subscription};
use silvestris::controls::{ControlSize, PopUpButton, TextField, TextFieldEvent};

use super::*;
use crate::debugger_palette::{self as palette, Field, Kind, Operation};
use lotus_session::supervisor::debug_requires_control;

pub(super) struct CommandPalette {
    search: Entity<TextField>,
    raw_json: Entity<TextField>,
    raw: bool,
    browsing: bool,
    selected: Option<&'static Operation>,
    fields: Vec<Argument>,
    prepared: Result<Prepared, String>,
    preview: bool,
    _search_subscription: Subscription,
    _raw_subscription: Subscription,
}

struct Prepared {
    operation: &'static Operation,
    command: D,
    json: String,
}

enum Argument {
    Text {
        entity: Entity<TextField>,
        _subscription: Subscription,
    },
    Choice(usize),
}

pub(super) fn field(
    window: &mut Window,
    cx: &mut Context<LotusWindow>,
    label: &str,
    hint: &str,
    search: bool,
) -> Entity<TextField> {
    let label = label.to_owned();
    let hint = hint.to_owned();
    cx.new(|cx| {
        let mut field = TextField::new(window, cx);
        field.set_size(ControlSize::Small, cx);
        field.set_label(label, cx);
        field.set_placeholder(hint, cx);
        field.set_search(search, cx);
        field
    })
}

fn choices(field: &Field) -> &[String] {
    static BOOLEAN: std::sync::OnceLock<[String; 2]> = std::sync::OnceLock::new();
    match &field.kind {
        Kind::Boolean => BOOLEAN.get_or_init(|| ["true".into(), "false".into()]),
        Kind::Choice(choices) => choices,
        _ => &[],
    }
}

impl CommandPalette {
    fn new(target: &SessionTarget, window: &mut Window, cx: &mut Context<LotusWindow>) -> Self {
        let search = field(window, cx, "Search commands", "Search commands", true);
        let raw_json = field(
            window,
            cx,
            "Command JSON",
            r#"{"operation":"status"}"#,
            false,
        );
        let search_subscription = cx.subscribe(&search, |_, _, _, cx| cx.notify());
        let raw_subscription = Self::observe_arguments(target, &raw_json, cx);
        Self {
            search,
            raw_json,
            raw: false,
            browsing: true,
            selected: None,
            fields: Vec::new(),
            prepared: Err("Choose a command".into()),
            preview: false,
            _search_subscription: search_subscription,
            _raw_subscription: raw_subscription,
        }
    }

    fn observe_arguments(
        target: &SessionTarget,
        entity: &Entity<TextField>,
        cx: &mut Context<LotusWindow>,
    ) -> Subscription {
        let target = target.clone();
        cx.subscribe(entity, move |view, _, event: &TextFieldEvent, cx| {
            if *event == TextFieldEvent::Changed {
                if let Some(palette) = view.command_palettes.get_mut(&target) {
                    palette.prepare(cx);
                    cx.notify();
                }
            }
        })
    }

    fn select(
        &mut self,
        target: &SessionTarget,
        operation: &'static Operation,
        window: &mut Window,
        cx: &mut Context<LotusWindow>,
    ) {
        self.selected = Some(operation);
        self.browsing = false;
        self.fields = operation
            .fields
            .iter()
            .map(|spec| {
                if !choices(spec).is_empty() {
                    return Argument::Choice(0);
                }
                let hint = spec
                    .default
                    .as_ref()
                    .map_or_else(|| spec.hint().into(), |value| format!("Default {value}"));
                let entity = field(window, cx, &spec.name, &hint, false);
                let subscription = Self::observe_arguments(target, &entity, cx);
                Argument::Text {
                    entity,
                    _subscription: subscription,
                }
            })
            .collect();
        self.prepare(cx);
    }

    fn prepare(&mut self, cx: &gpui::App) {
        let command = if self.raw {
            palette::parse_json(self.raw_json.read(cx).text())
        } else if let Some(op) = self.selected {
            let args = op
                .fields
                .iter()
                .zip(&self.fields)
                .map(|(field, arg)| match arg {
                    Argument::Choice(index) => choices(field)
                        .get(index.wrapping_sub(1))
                        .map(String::as_str)
                        .unwrap_or_default(),
                    Argument::Text { entity, .. } => entity.read(cx).text(),
                })
                .collect::<Vec<_>>();
            op.parse(&args)
        } else {
            Err("Choose a command".into())
        };
        self.prepared = command.and_then(|command| {
            let operation = self
                .selected
                .filter(|_| !self.raw)
                .or_else(|| palette::operation(&command))
                .ok_or("Unknown debugger operation")?;
            let json = serde_json::to_string_pretty(&command).map_err(|error| error.to_string())?;
            Ok(Prepared {
                operation,
                command,
                json,
            })
        });
    }
}

impl LotusWindow {
    pub(super) fn ensure_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.inspection_panel == Panel::Commands {
            if let Some(target) = self.inspection_target() {
                self.command_palettes
                    .entry(target.clone())
                    .or_insert_with(|| CommandPalette::new(&target, window, cx));
            }
        }
    }

    fn palette_block(&self, target: &SessionTarget, prepared: &Prepared) -> Option<String> {
        let operation = prepared.operation;
        if operation.route == "external only" || operation.route == "unavailable" {
            return Some(operation.note.to_owned());
        }
        let Some(summary) = target.resolve(&self.sessions) else {
            return Some("Session or guest changed".into());
        };
        if !summary.telemetry.state.is_active() {
            return Some("The worker is inactive".into());
        }
        if debug_requires_control(&prepared.command) && !self.control {
            return Some("Control is disabled for this host".into());
        }
        if prepared.command.mutates() && summary.telemetry.desktop_debug_lease_blocked {
            return Some("A different client holds the debugger lease".into());
        }
        prepared.command.required_feature().and_then(|feature| {
            (!summary.telemetry.worker_features.contains(feature))
                .then(|| format!("Unsupported · worker does not advertise {feature}"))
        })
    }

    fn run_palette(&mut self, target: &SessionTarget) {
        if self.inspection_panel != Panel::Commands
            || self.inspection_target().as_ref() != Some(target)
        {
            return;
        }
        if let Some(prepared) = self
            .command_palettes
            .get(target)
            .and_then(|palette| palette.prepared.as_ref().ok())
            .filter(|prepared| self.palette_block(target, prepared).is_none())
        {
            self.debug_command(
                &Some(target.clone()),
                Panel::Commands,
                prepared.command.clone(),
            );
        }
    }

    pub(super) fn render_command_palette(
        &self,
        target: &SessionTarget,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let state = self.command_palettes.get(target)?;
        let summary = target.resolve(&self.sessions)?;
        let theme = theme::palette(cx);
        let control = if !self.control {
            "Host read-only"
        } else if summary.telemetry.desktop_debug_lease_blocked {
            "Debugger lease held by another client"
        } else {
            "Host control enabled"
        };
        let toggle_target = target.clone();
        let mut form = div()
            .id(if state.raw {
                "palette-raw-form"
            } else if state.browsing {
                "palette-command-search"
            } else {
                "palette-command-form"
            })
            .flex()
            .flex_col()
            .gap_2()
            .px_2()
            .py_2()
            .max_h(relative(0.65))
            .overflow_y_scroll()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_xs()
                    .text_color(theme.text_secondary)
                    .child(format!(
                        "{} {} · {} · guest epoch {}",
                        self.session_name(&target.id),
                        session_tree::short_id(&target.id),
                        summary.telemetry.backend_name,
                        target.epoch,
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.text_secondary)
                    .child(control),
            )
            .child(
                div().flex().justify_end().child(
                    small_button(
                        theme,
                        "palette-mode",
                        if state.raw {
                            "Command form"
                        } else {
                            "Raw JSON"
                        },
                        Tone::Plain,
                        true,
                    )
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if let Some(palette) = view.command_palettes.get_mut(&toggle_target) {
                            palette.raw = !palette.raw;
                            palette.prepare(cx);
                            cx.notify();
                        }
                    })),
                ),
            );
        if state.raw {
            form = form.child(div().flex_none().child(state.raw_json.clone()));
        } else if state.browsing {
            return Some(
                form.child(
                    div()
                        .id("palette-search-input")
                        .debug_selector(|| "palette-search-input".to_string())
                        .flex_none()
                        .child(state.search.clone()),
                )
                .child(self.render_palette_search(state, target, cx))
                .into_any_element(),
            );
        } else if let Some(op) = state.selected {
            form = form.child(self.render_palette_arguments(state, op, target, cx));
        }

        let ready = state.prepared.as_ref();
        let blocked = ready
            .ok()
            .and_then(|prepared| self.palette_block(target, prepared));
        let external = !state.raw && state.selected.is_some_and(|op| op.route == "external only");
        let status = blocked
            .as_ref()
            .or_else(|| ready.err().filter(|_| !external));
        form = form.when_some(status, |form, message| {
            form.child(
                div()
                    .text_xs()
                    .text_color(theme.text_secondary)
                    .child(message.clone()),
            )
        });
        if let Ok(prepared) = ready {
            if debug_requires_control(&prepared.command) {
                let message = if prepared.command.mutates() {
                    "Requires control. Running this command can change guest or session state."
                } else {
                    "Requires control of this host service."
                };
                form = form.child(div().text_xs().text_color(theme.warning).child(message));
            }
            if let Some(feature) = prepared.command.required_feature() {
                form = form.child(div().text_xs().child(format!("Worker feature · {feature}")));
            }
            if state.preview || state.raw {
                form = form.child(
                    div()
                        .id("palette-json-preview")
                        .max_h(px(96.))
                        .overflow_scroll()
                        .font_family(theme.mono)
                        .text_xs()
                        .child(prepared.json.clone()),
                );
            }
        }
        let run_target = target.clone();
        let toggle_target = target.clone();
        Some(
            form.child(
                div()
                    .flex()
                    .gap_2()
                    .when(!state.raw && ready.is_ok(), |row| {
                        row.child(
                            small_button(
                                theme,
                                "palette-preview",
                                if state.preview {
                                    "Hide JSON"
                                } else {
                                    "Preview JSON"
                                },
                                Tone::Plain,
                                true,
                            )
                            .on_click(cx.listener(
                                move |view, _, _, cx| {
                                    if let Some(palette) =
                                        view.command_palettes.get_mut(&toggle_target)
                                    {
                                        palette.preview = !palette.preview;
                                        cx.notify();
                                    }
                                },
                            )),
                        )
                    })
                    .child(
                        small_button(
                            theme,
                            "palette-run",
                            "Run command",
                            Tone::Primary,
                            ready.is_ok() && blocked.is_none(),
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.run_palette(&run_target);
                            cx.notify();
                        })),
                    ),
            )
            .into_any_element(),
        )
    }

    fn render_palette_search(
        &self,
        state: &CommandPalette,
        target: &SessionTarget,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let query = state.search.read(cx).text().to_ascii_lowercase();
        let theme = theme::palette(cx);
        let mut list = div().flex().flex_col().items_start().gap_1();
        let mut group = None;
        for op in palette::catalogue().iter().filter(|op| op.matches(&query)) {
            if group != Some(op.group) {
                group = Some(op.group);
                list = list.child(
                    div()
                        .pt_1()
                        .text_xs()
                        .text_color(theme.text_secondary)
                        .child(op.group),
                );
            }
            let target = target.clone();
            list = list.child(
                CompactButton::new(format!("palette-op-{}", op.name), op.name.replace('_', " "))
                    .size(ControlSize::Small)
                    .on_click(cx.listener(move |view, _, window, cx| {
                        if let Some(palette) = view.command_palettes.get_mut(&target) {
                            palette.select(&target, op, window, cx);
                            cx.notify();
                        }
                    })),
            );
        }
        if group.is_none() {
            list = list.child("No matching commands");
        }
        list
    }

    fn render_palette_arguments(
        &self,
        state: &CommandPalette,
        op: &'static Operation,
        target: &SessionTarget,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = theme::palette(cx);
        let browse_target = target.clone();
        let mut form = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(div().text_sm().child(op.name.replace('_', " ")))
                    .child(
                        small_button(theme, "palette-find", "Find command", Tone::Plain, true)
                            .on_click(cx.listener(move |view, _, window, cx| {
                                if let Some(palette) = view.command_palettes.get_mut(&browse_target)
                                {
                                    palette.browsing = true;
                                    window.refresh();
                                    cx.notify();
                                }
                            })),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.text_secondary)
                    .child(format!("{} · {}", op.group, op.permission)),
            );
        if op.feature != "-" {
            form = form.child(
                div()
                    .text_xs()
                    .child(format!("Feature requirement · {}", op.feature)),
            );
        }
        if op.route == "external only" {
            return form.child(div().text_xs().child(op.note));
        }
        for (index, (spec, field)) in op.fields.iter().zip(&state.fields).enumerate() {
            let label = format!(
                "{}{}",
                spec.name.replace('_', " "),
                if spec.required { "" } else { " (optional)" }
            );
            let mut row = div()
                .flex()
                .flex_col()
                .gap_1()
                .text_xs()
                .child(label.clone());
            if !spec.description.is_empty() {
                row = row.child(
                    div()
                        .text_color(theme.text_secondary)
                        .child(spec.description.clone()),
                );
            }
            match field {
                Argument::Text { entity, .. } => row = row.child(entity.clone()),
                Argument::Choice(selected) => {
                    let target = target.clone();
                    let initial = if spec.required {
                        "Choose a value"
                    } else {
                        "Unset"
                    };
                    row = row.child(
                        PopUpButton::new(
                            ("palette-option", index),
                            label,
                            std::iter::once(initial.into())
                                .chain(choices(spec).iter().map(|c| SharedString::from(c.clone())))
                                .collect(),
                        )
                        .selected(Some(*selected))
                        .on_choose(cx.listener(
                            move |view, choice: &usize, _, cx| {
                                if let Some(state) = view.command_palettes.get_mut(&target) {
                                    if state.selected.is_some_and(|active| active.name == op.name) {
                                        if let Some(Argument::Choice(selected)) =
                                            state.fields.get_mut(index)
                                        {
                                            *selected = *choice;
                                            state.prepare(cx);
                                            cx.notify();
                                        }
                                    }
                                }
                            },
                        )),
                    );
                }
            }
            form = form.child(row);
        }
        form
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Focusable, TestAppContext};
    use lotus_session::{supervisor::SessionInfo, telemetry::Telemetry};

    #[test]
    fn mounted_palette_filters_forms_and_isolates_guest_epochs() {
        let mut app = TestAppContext::single();
        app.update(|cx| {
            silvestris::test_support::initialize(
                silvestris::ThemePreference::explicit(silvestris::ThemeId::GEIST_DARK),
                cx,
            )
        });
        let (view, cx) = app.add_window_view(|window, cx| {
            let mut view = LotusWindow::test_preview(window, cx);
            let mut telemetry = Telemetry::new();
            telemetry.state = SessionState::Paused;
            telemetry.epoch = 2;
            telemetry.backend_name = "CITnLO".into();
            view.sessions = vec![SessionSummary {
                info: SessionInfo {
                    id: "fixture".into(),
                    kind: SessionKind::Secondary { parent: None },
                    asset: Some("fixture.3dsx".into()),
                    created_unix_ms: 1,
                },
                telemetry,
            }];
            view.selected = "fixture".into();
            view.inspected = "fixture".into();
            view.invoke_global(shared_commands::GlobalCommand::DebuggerCommands, window, cx);
            view
        });
        cx.simulate_resize(gpui::size(px(1400.), px(900.)));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let old_target = cx.update(|_, cx| view.read(cx).inspection_target().unwrap());
        let search_bounds = cx.debug_bounds("palette-search-input").unwrap();
        assert!(search_bounds.size.height > px(0.));
        cx.simulate_click(search_bounds.center(), gpui::Modifiers::default());
        cx.update(|window, cx| {
            let search = view.read(cx).command_palettes[&old_target].search.clone();
            assert!(
                search.read(cx).focus_handle(cx).is_focused(window),
                "search click makes it the input target"
            );
            window.draw(cx).clear(cx);
        });
        cx.simulate_keystrokes("r e a d");
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            assert_eq!(
                view.read(cx).command_palettes[&old_target]
                    .search
                    .read(cx)
                    .text(),
                "read"
            );
        });
        assert!(cx.debug_bounds("palette-op-write_memory").is_none());
        let item = cx
            .debug_bounds("palette-op-read_memory")
            .expect("filtered command");
        cx.simulate_click(item.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let fields = cx.update(|_, cx| {
            let state = &view.read(cx).command_palettes[&old_target];
            assert_eq!(state.selected.unwrap().name, "read_memory");
            assert!(state.prepared.is_err());
            state
                .fields
                .iter()
                .map(|argument| match argument {
                    Argument::Text { entity, .. } => entity.clone(),
                    Argument::Choice(_) => panic!("both read arguments are numeric"),
                })
                .collect::<Vec<_>>()
        });
        cx.update(|window, cx| {
            window.focus(&fields[0].read(cx).focus_handle(cx), cx);
            window.draw(cx).clear(cx);
        });
        cx.simulate_keystrokes("0 x 1 0 0 0 0");
        cx.update(|window, cx| {
            window.focus(&fields[1].read(cx).focus_handle(cx), cx);
            window.draw(cx).clear(cx);
        });
        cx.simulate_keystrokes("1 6");
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let view = view.read(cx);
            let ready = view.command_palettes[&old_target]
                .prepared
                .as_ref()
                .unwrap();
            assert!(matches!(
                ready.command,
                D::ReadMemory {
                    address: 0x10000,
                    length: 16
                }
            ));
            assert!(view.palette_block(&old_target, ready).is_none());
            assert!(view.keys.is_empty(), "typing must not reach guest input");
        });
        let raw_mode = cx.debug_bounds("palette-mode").unwrap();
        cx.simulate_click(raw_mode.center(), gpui::Modifiers::default());
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let raw = view.read(cx).command_palettes[&old_target].raw_json.clone();
            window.focus(&raw.read(cx).focus_handle(cx), cx);
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                r#"{"operation":"gpu_configure","enabled":true,"stop_events":[],"capture":"draws"}"#.into(),
            ));
        });
        cx.simulate_keystrokes("ctrl-v");
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                let ready = view.command_palettes[&old_target]
                    .prepared
                    .as_ref()
                    .unwrap();
                assert!(matches!(
                    ready.command,
                    D::GpuConfigure {
                        capture: Some(GpuCapture::Draws),
                        ..
                    }
                ));
                assert!(view
                    .palette_block(&old_target, ready)
                    .unwrap()
                    .contains("Control is disabled"));
                view.control = true;
                assert!(view
                    .palette_block(&old_target, ready)
                    .unwrap()
                    .contains("gpu_capture_levels"));
                view.sessions[0].telemetry.worker_features =
                    Arc::new(BTreeSet::from(["gpu_capture_levels".into()]));
                assert!(view.palette_block(&old_target, ready).is_none());
                view.sessions[0].telemetry.desktop_debug_lease_blocked = true;
                assert!(view
                    .palette_block(&old_target, ready)
                    .unwrap()
                    .contains("lease"));
                view.sessions[0].telemetry.epoch += 1;
                view.ensure_command_palette(window, cx);
                let current = view.inspection_target().unwrap();
                assert!(view.command_palettes[&current].selected.is_none());
                assert!(view
                    .palette_block(
                        &old_target,
                        view.command_palettes[&old_target]
                            .prepared
                            .as_ref()
                            .unwrap()
                    )
                    .is_some());
            });
        });
    }
}
