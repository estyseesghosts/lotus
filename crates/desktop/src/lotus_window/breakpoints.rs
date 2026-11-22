use super::*;
use crate::inspection::{BreakpointEntry, BreakpointList, BreakpointSettings};
use gpui::{App, Entity, Subscription};
use lotus_protocol::debug::{BreakpointKind, Condition};
use silvestris::controls::{ControlSize, PopUpButton, TextField};

const KINDS: [BreakpointKind; 4] = [
    BreakpointKind::Execute,
    BreakpointKind::Read,
    BreakpointKind::Write,
    BreakpointKind::Access,
];

fn kind_name(kind: BreakpointKind) -> &'static str {
    match kind {
        BreakpointKind::Execute => "Execute",
        BreakpointKind::Read => "Read",
        BreakpointKind::Write => "Write",
        BreakpointKind::Access => "Access",
    }
}

#[derive(Default)]
pub(super) struct BreakpointPanel {
    selected: Option<u64>,
    location: Option<(u32, u32, BreakpointKind)>,
    form: Option<Form>,
}

pub(super) struct Input {
    field: Entity<TextField>,
    _subscription: Subscription,
}

impl Input {
    pub(super) fn new(
        window: &mut Window,
        cx: &mut Context<LotusWindow>,
        label: &str,
        text: String,
    ) -> Self {
        let field = command_palette::field(window, cx, label, "", false);
        field.update(cx, |field, cx| field.set_text(text, cx));
        let subscription = cx.subscribe(&field, |_, _, _, cx| cx.notify());
        Self {
            field,
            _subscription: subscription,
        }
    }

    pub(super) fn text<'a>(&self, cx: &'a App) -> &'a str {
        self.field.read(cx).text()
    }

    pub(super) fn number<T: TryFrom<u64>>(&self, label: &str, cx: &App) -> Result<T, String> {
        crate::debugger_palette::parse_integer(self.text(cx).trim(), false)
            .ok()
            .and_then(|result| result.as_u64())
            .and_then(|n| T::try_from(n).ok())
            .ok_or_else(|| {
                format!("{label} requires an in-range decimal or 0x hexadecimal integer")
            })
    }

    pub(super) fn render(&self, label: &str, cx: &App) -> Div {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .text_color(theme::palette(cx).text_secondary)
            .child(label.to_owned())
            .child(self.field.clone())
    }
}

enum Form {
    Add {
        kind: BreakpointKind,
        address: Input,
        length: Input,
    },
    Edit(Box<Edit>),
    Remove {
        id: u64,
    },
}

struct Edit {
    id: u64,
    condition_kind: usize,
    process: Input,
    hit_after: Input,
    reference: Input,
    value: Input,
    mask_or_width: Input,
    log: Input,
}

impl Form {
    fn command(&self, list: &BreakpointList, cx: &App) -> Result<D, String> {
        let command = match self {
            Self::Add {
                kind,
                address,
                length,
            } => {
                if *kind != BreakpointKind::Execute && list.watchpoints.is_none() {
                    return Err("The worker does not report watchpoint support".into());
                }
                if list
                    .maximum
                    .is_some_and(|max| list.breakpoints.len() >= max)
                {
                    return Err("The worker's breakpoint list is full".into());
                }
                let address: u32 = address.number("Address", cx)?;
                let length = length.number("Length", cx)?;
                if *kind == BreakpointKind::Execute {
                    if !matches!(length, 2 | 4) || !address.is_multiple_of(length) {
                        return Err(
                            "Use a 2-byte aligned Thumb address or a 4-byte aligned ARM address"
                                .into(),
                        );
                    }
                } else if u64::from(address) + u64::from(length) >= 1 << 32 {
                    return Err("Watchpoint crosses the guest address boundary".into());
                }
                D::BreakpointSet {
                    address,
                    kind: *kind,
                    length,
                }
            }
            Self::Edit(edit) => {
                let condition = match edit.condition_kind {
                    0 => None,
                    1 => Some(Condition::RegisterEquals {
                        register: edit.reference.number("Register", cx)?,
                        value: edit.value.number("Value", cx)?,
                        mask: if edit.mask_or_width.text(cx).trim().is_empty() {
                            u32::MAX
                        } else {
                            edit.mask_or_width.number("Mask", cx)?
                        },
                    }),
                    _ => {
                        let width: u32 = edit.mask_or_width.number("Width", cx)?;
                        let value: u64 = edit.value.number("Value", cx)?;
                        if matches!(width, 1 | 2 | 4) && value >= (1_u64 << (8 * width)) {
                            return Err("Value does not fit the selected width".into());
                        }
                        Some(Condition::MemoryEquals {
                            address: edit.reference.number("Memory address", cx)?,
                            width,
                            value,
                        })
                    }
                };
                let log = edit.log.text(cx);
                if log.len() > 512 {
                    return Err("Log text exceeds 512 bytes".into());
                }
                D::BreakpointConfigure {
                    id: edit.id,
                    process: (!edit.process.text(cx).trim().is_empty())
                        .then(|| edit.process.number("Process ID", cx))
                        .transpose()?,
                    hit_after: edit.hit_after.number("Hit threshold", cx)?,
                    condition,
                    log: (!log.is_empty()).then(|| log.to_owned()),
                }
            }
            Self::Remove { id } => D::BreakpointRemove { id: *id },
        };
        if let D::BreakpointConfigure { id, .. } | D::BreakpointRemove { id } = &command {
            if !list.breakpoints.iter().any(|entry| entry.id == *id) {
                return Err("This breakpoint is no longer listed".into());
            }
        }
        command.validate().map_err(str::to_owned)?;
        Ok(command)
    }
}

impl LotusWindow {
    fn breakpoint_list(&self, target: &SessionTarget) -> Option<&BreakpointList> {
        self.inspection
            .as_ref()
            .filter(|snap| snap.panel == Panel::Breakpoints && snap.target.as_ref() == Some(target))
            .and_then(|snap| snap.view.breakpoints.as_deref())
    }

    fn breakpoint_block(&self, target: &SessionTarget) -> Option<&'static str> {
        let Some(summary) = target.resolve(&self.sessions) else {
            return Some("Session or guest changed");
        };
        if !self.control {
            Some("Control is disabled for this host")
        } else if summary.telemetry.desktop_debug_lease_blocked {
            Some("A different client holds the debugger lease")
        } else if !summary.telemetry.state.is_active() {
            Some("The worker is inactive")
        } else if self.debug_request.as_ref().is_some_and(|request| {
            request.target == *target
                && request.panel == Panel::Breakpoints
                && matches!(request.status, DebugStatus::Running)
        }) {
            Some("Waiting for the breakpoint command")
        } else {
            None
        }
    }

    pub(super) fn jump_to_breakpoint(&mut self, target: &SessionTarget, address: u32, length: u32) {
        self.jump_to_breakpoint_kind(target, address, length, BreakpointKind::Execute);
    }

    pub(super) fn jump_to_watchpoint(&mut self, target: &SessionTarget, address: u32) {
        self.jump_to_breakpoint_kind(target, address, 1, BreakpointKind::Access);
    }

    fn jump_to_breakpoint_kind(
        &mut self,
        target: &SessionTarget,
        address: u32,
        length: u32,
        kind: BreakpointKind,
    ) {
        if self.inspection_target().as_ref() == Some(target) {
            let state = self.breakpoint_panels.entry(target.clone()).or_default();
            state.location = Some((address, length, kind));
            state.selected = None;
            state.form = None;
            self.show_panel(Panel::Breakpoints);
        }
    }

    fn open_breakpoint_form(
        &mut self,
        target: &SessionTarget,
        id: Option<u64>,
        remove: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.inspection_target().as_ref() != Some(target)
            || self.breakpoint_block(target).is_some()
        {
            return;
        }
        let Some(list) = self.breakpoint_list(target) else {
            return;
        };
        let mut input = |label, text| Input::new(window, cx, label, text);
        let form = if let Some(id) = id {
            let Some(entry) = list.breakpoints.iter().find(|entry| entry.id == id) else {
                return;
            };
            if remove {
                Form::Remove { id }
            } else {
                let settings = entry.host_configuration.as_ref();
                let (condition_kind, reference, value, mask_or_width) =
                    match settings.and_then(|settings| settings.condition.as_ref()) {
                        Some(Condition::RegisterEquals {
                            register,
                            value,
                            mask,
                        }) => (
                            1,
                            register.to_string(),
                            format!("0x{value:x}"),
                            format!("0x{mask:x}"),
                        ),
                        Some(Condition::MemoryEquals {
                            address,
                            width,
                            value,
                        }) => (
                            2,
                            format!("0x{address:08x}"),
                            format!("0x{value:x}"),
                            width.to_string(),
                        ),
                        None => (0, String::new(), String::new(), String::new()),
                    };
                Form::Edit(Box::new(Edit {
                    id,
                    condition_kind,
                    process: input(
                        "Process ID",
                        settings
                            .and_then(|s| s.process)
                            .map_or_else(String::new, |p| p.to_string()),
                    ),
                    hit_after: input(
                        "Hit threshold",
                        settings.map_or(0, |s| s.hit_after).to_string(),
                    ),
                    reference: input("Register or memory address", reference),
                    value: input("Condition value", value),
                    mask_or_width: input("Register mask or memory width", mask_or_width),
                    log: input(
                        "Log text",
                        settings.and_then(|s| s.log.clone()).unwrap_or_default(),
                    ),
                }))
            }
        } else {
            let location = self
                .breakpoint_panels
                .get(target)
                .and_then(|state| state.location);
            let (address, length, kind) = location.map_or(
                (String::new(), 4, BreakpointKind::Execute),
                |(address, length, kind)| (format!("0x{address:08x}"), length, kind),
            );
            Form::Add {
                kind,
                address: input("Address", address),
                length: input("Length", length.to_string()),
            }
        };
        self.breakpoint_panels
            .entry(target.clone())
            .or_default()
            .form = Some(form);
        cx.notify();
    }

    fn apply_breakpoint_form(&mut self, target: &SessionTarget, cx: &mut Context<Self>) {
        if self.inspection_panel != Panel::Breakpoints
            || self.inspection_target().as_ref() != Some(target)
            || self.breakpoint_block(target).is_some()
        {
            return;
        }
        let Some(command) = self
            .breakpoint_panels
            .get(target)
            .and_then(|state| state.form.as_ref())
            .and_then(|form| form.command(self.breakpoint_list(target)?, cx).ok())
        else {
            return;
        };
        self.breakpoint_panels
            .entry(target.clone())
            .or_default()
            .form = None;
        self.debug_command(&Some(target.clone()), Panel::Breakpoints, command);
        cx.notify();
    }

    pub(super) fn render_breakpoint_manager(
        &self,
        target: &SessionTarget,
        list: &BreakpointList,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = theme::palette(cx);
        let state = self.breakpoint_panels.get(target);
        let blocked = self.breakpoint_block(target);
        if let Some(form) = state.and_then(|state| state.form.as_ref()) {
            return self.render_breakpoint_form(target, list, form, blocked, cx);
        }
        let location = state.and_then(|state| state.location);
        let selected = state
            .and_then(|state| state.selected)
            .and_then(|id| list.breakpoints.iter().find(|entry| entry.id == id))
            .or_else(|| {
                list.breakpoints.iter().find(|entry| {
                    location.is_some_and(|(address, _, kind)| {
                        if kind == BreakpointKind::Execute {
                            entry.kind == kind && entry.address == address
                        } else {
                            entry.kind != BreakpointKind::Execute
                                && address
                                    .checked_sub(entry.address)
                                    .is_some_and(|offset| offset < entry.length)
                        }
                    })
                })
            });
        let full = list
            .maximum
            .is_some_and(|max| list.breakpoints.len() >= max);
        let target = target.clone();
        let add_target = target.clone();
        let mut toolbar = div().flex().items_center().flex_wrap().gap_1().child(
            small_button(
                palette,
                "breakpoint-add",
                "Add…",
                Tone::Plain,
                blocked.is_none() && !full,
            )
            .on_click(cx.listener(move |view, _, window, cx| {
                view.open_breakpoint_form(&add_target, None, false, window, cx)
            })),
        );
        if let Some(entry) = selected {
            let (owner, address) = (target.clone(), entry.address);
            toolbar = toolbar.child(
                small_button(
                    palette,
                    "breakpoint-open-memory",
                    "Open in Memory",
                    Tone::Plain,
                    true,
                )
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.jump_to_memory(&owner, Some(address));
                    cx.notify();
                })),
            );
            for (button, label, remove) in [
                ("breakpoint-configure", "Configure…", false),
                ("breakpoint-remove", "Remove…", true),
            ] {
                let target = target.clone();
                let id = entry.id;
                toolbar = toolbar.child(
                    small_button(palette, button, label, Tone::Plain, blocked.is_none()).on_click(
                        cx.listener(move |view, _, window, cx| {
                            view.open_breakpoint_form(&target, Some(id), remove, window, cx)
                        }),
                    ),
                );
            }
        }
        let mut body = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .py_1()
            .text_xs()
            .child(toolbar)
            .child(format!(
                "{} breakpoints{}",
                list.breakpoints.len(),
                list.maximum
                    .map_or_else(String::new, |max| format!(" · capacity {max}"))
            ))
            .when_some(blocked, |body, message| body.child(message))
            .when_some(list.execution_mode.as_ref(), |body, mode| {
                body.child(format!("Worker execution · {}", mode.replace('_', " ")))
            })
            .child(match list.watchpoints.as_deref() {
                Some("instruction_step_when_armed") => {
                    "Watchpoints use instruction stepping while armed"
                }
                Some(_) => "Watchpoints available",
                None => "Watchpoint support unavailable",
            });
        if let Some((address, _, kind)) = location.filter(|_| selected.is_none()) {
            let label = if kind == BreakpointKind::Execute {
                "execute breakpoint"
            } else {
                "watchpoint"
            };
            body = body.child(format!("No {label} at 0x{address:08x}"));
        }
        if let Some(entry) = selected {
            body = body.child(breakpoint_details(entry));
        }
        let mut rows = div()
            .id("breakpoint-rows")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .text_xs();
        for entry in &list.breakpoints {
            let id = entry.id;
            let target = target.clone();
            let label = format!(
                "#{id} · 0x{:08x} · {} · {} bytes · {} hits",
                entry.address,
                kind_name(entry.kind),
                entry.length,
                entry
                    .hits
                    .map_or_else(|| "unknown".into(), |hits| hits.to_string())
            );
            rows = rows.child(
                div()
                    .id(("breakpoint", id as usize))
                    .debug_selector(move || format!("breakpoint-{id}"))
                    .px_2()
                    .py_1()
                    .font_family(palette.mono)
                    .cursor_pointer()
                    .when(selected.is_some_and(|entry| entry.id == id), |row| {
                        row.bg(palette.surface_active)
                    })
                    .hover(|row| row.bg(palette.surface_hover))
                    .child(label)
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if view.inspection_target().as_ref() == Some(&target) {
                            let state = view.breakpoint_panels.entry(target.clone()).or_default();
                            state.selected = Some(id);
                            state.location = None;
                            cx.notify();
                        }
                    })),
            );
        }
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(body)
            .child(rows)
            .into_any_element()
    }

    fn render_breakpoint_form(
        &self,
        target: &SessionTarget,
        list: &BreakpointList,
        form: &Form,
        blocked: Option<&str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = theme::palette(cx);
        let target = target.clone();
        let mut body = div()
            .id("breakpoint-form")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap_2()
            .px_3()
            .py_2()
            .text_sm();
        let submit = match form {
            Form::Add {
                kind,
                address,
                length,
            } => {
                let owner = target.clone();
                let kinds = if list.watchpoints.is_some() {
                    &KINDS[..]
                } else {
                    &KINDS[..1]
                };
                body = body
                    .child(
                        PopUpButton::new(
                            "breakpoint-kind",
                            "Breakpoint kind",
                            kinds.iter().map(|kind| kind_name(*kind).into()).collect(),
                        )
                        .size(ControlSize::Small)
                        .selected(kinds.iter().position(|value| value == kind))
                        .on_choose(cx.listener(
                            move |view, index: &usize, _, cx| {
                                if let Some(Form::Add { kind, .. }) = view
                                    .breakpoint_panels
                                    .get_mut(&owner)
                                    .and_then(|panel| panel.form.as_mut())
                                {
                                    if let Some(value) = KINDS.get(*index) {
                                        *kind = *value;
                                        cx.notify();
                                    }
                                }
                            },
                        )),
                    )
                    .child(address.render("Virtual address", cx))
                    .child(length.render(
                        if *kind == BreakpointKind::Execute {
                            "Instruction width (2 or 4 bytes)"
                        } else {
                            "Watch length (1–65,536 bytes)"
                        },
                        cx,
                    ))
                    .when(
                        *kind != BreakpointKind::Execute
                            && list.watchpoints.as_deref() == Some("instruction_step_when_armed"),
                        |body| {
                            body.child("The worker instruction-steps while a watchpoint is armed.")
                        },
                    );
                "Create"
            }
            Form::Edit(edit) => {
                let owner = target.clone();
                let known = list
                    .breakpoints
                    .iter()
                    .find(|entry| entry.id == edit.id)
                    .and_then(|entry| entry.host_configuration.as_ref())
                    .is_some();
                body = body.child(format!("Configure breakpoint #{}", edit.id))
                    .when(!known, |body| body.child("Previous settings unavailable. Apply replaces the entire configuration."))
                    .child(edit.process.render("Process ID (blank for any)", cx))
                    .child(edit.hit_after.render("Hit threshold (0 for every hit)", cx))
                    .child(PopUpButton::new("breakpoint-condition", "Condition", ["Always", "Register equals", "Memory equals"].map(SharedString::from).to_vec())
                        .size(ControlSize::Small).selected(Some(edit.condition_kind))
                        .on_choose(cx.listener(move |view, index: &usize, _, cx| {
                            if let Some(Form::Edit(edit)) = view.breakpoint_panels.get_mut(&owner).and_then(|panel| panel.form.as_mut()) {
                                if edit.condition_kind != *index && *index < 3 {
                                    edit.condition_kind = *index;
                                    for input in [&edit.reference, &edit.value, &edit.mask_or_width] {
                                        input.field.update(cx, |field, cx| field.set_text(String::new(), cx));
                                    }
                                    cx.notify();
                                }
                            }
                        })));
                if edit.condition_kind != 0 {
                    let register = edit.condition_kind == 1;
                    body = body
                        .child(edit.reference.render(
                            if register {
                                "Register index (0–16)"
                            } else {
                                "Memory address"
                            },
                            cx,
                        ))
                        .child(edit.value.render("Expected value", cx))
                        .child(edit.mask_or_width.render(
                            if register {
                                "Mask (blank for all bits)"
                            } else {
                                "Width (1, 2, 4 or 8 bytes)"
                            },
                            cx,
                        ));
                }
                body = body.child(edit.log.render("Log text (blank to stop on a match)", cx));
                "Apply"
            }
            Form::Remove { id } => {
                body = body.child(format!("Remove breakpoint #{id}?"));
                "Remove"
            }
        };
        let command = form.command(list, cx);
        let allowed = blocked.is_none() && command.is_ok();
        let cancel = target.clone();
        let footer = div()
            .px_2()
            .py_2()
            .flex()
            .flex_wrap()
            .gap_1()
            .child(
                small_button(palette, "breakpoint-cancel", "Cancel", Tone::Plain, true).on_click(
                    cx.listener(move |view, _, _, cx| {
                        if let Some(panel) = view.breakpoint_panels.get_mut(&cancel) {
                            panel.form = None;
                            cx.notify();
                        }
                    }),
                ),
            )
            .child(
                small_button(
                    palette,
                    "breakpoint-submit",
                    submit,
                    if submit == "Remove" {
                        Tone::Danger
                    } else {
                        Tone::Primary
                    },
                    allowed,
                )
                .on_click(
                    cx.listener(move |view, _, _, cx| view.apply_breakpoint_form(&target, cx)),
                ),
            );
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(body)
            .when_some(
                blocked.map(str::to_owned).or_else(|| command.err()),
                |panel, message| {
                    panel.child(
                        div()
                            .px_2()
                            .text_xs()
                            .text_color(palette.text_secondary)
                            .child(message),
                    )
                },
            )
            .child(footer)
            .into_any_element()
    }
}

fn breakpoint_details(entry: &BreakpointEntry) -> Div {
    let Some(BreakpointSettings {
        process,
        hit_after,
        condition,
        log,
    }) = &entry.host_configuration
    else {
        return div().child("Conditions, process filter and log settings unavailable");
    };
    let condition = match condition {
        None => "Always".into(),
        Some(Condition::RegisterEquals {
            register,
            value,
            mask,
        }) => format!("Register {register} & 0x{mask:08x} = 0x{value:08x}"),
        Some(Condition::MemoryEquals {
            address,
            width,
            value,
        }) => format!("{width} bytes at 0x{address:08x} = 0x{value:x}"),
    };
    let process = process.map_or_else(|| "any".into(), |id| id.to_string());
    div()
        .child(format!("Process {process} · hit threshold {hit_after}"))
        .child(div().child(condition))
        .when_some(
            log.as_ref().filter(|log| !log.is_empty()),
            |details, log| details.child(div().truncate().child(format!("Log · {log}"))),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Focusable;
    use std::time::Instant;

    #[test]
    #[ignore = "requires LOTUS_NATIVE_WORKER and LOTUS_FIXTURE"]
    fn mounted_breakpoint_manager_controls_citnlo() {
        let worker = std::env::var_os("LOTUS_NATIVE_WORKER").unwrap();
        let fixture = std::env::var_os("LOTUS_FIXTURE").unwrap();
        let root = std::env::temp_dir().join(format!(
            "lotus-breakpoints-{}",
            lotus_protocol::envelope::SessionUuid::generate()
        ));
        let mut config = HostConfig::new(worker, &root);
        config.allow_control = true;
        config.assets.insert("fixture".into(), fixture.into());
        let mut app = gpui::TestAppContext::single();
        app.update(|cx| {
            silvestris::test_support::initialize(
                silvestris::ThemePreference::explicit(silvestris::ThemeId::GEIST_DARK),
                cx,
            );
        });
        let (entity, cx) = app.add_window_view(|window, cx| {
            let mut view = LotusWindow::new(Some(config), McpMode::Off, window, cx);
            view.preferences = Store::load(None);
            view.show_panel(Panel::Cpu);
            view.send(Command::Create {
                kind: SessionKind::Secondary { parent: None },
                asset: Some("fixture".into()),
            });
            view
        });
        cx.simulate_resize(gpui::size(px(1400.), px(1000.)));

        let wait = |cx: &mut gpui::VisualTestContext,
                    description: &str,
                    ready: &dyn Fn(&LotusWindow, &App) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                let done = cx.update(|_, cx| {
                    entity.update(cx, |view, cx| {
                        view.poll(cx);
                        cx.notify();
                        if let Some((operation, message)) =
                            view.debug_request
                                .as_ref()
                                .and_then(|request| match &request.status {
                                    DebugStatus::Failed { message, .. } => {
                                        Some((&request.operation, message))
                                    }
                                    _ => None,
                                })
                        {
                            panic!("{operation} during {description}: {message}");
                        }
                        assert!(
                            Instant::now() < deadline,
                            "deadline waiting for {description}: {} · {:?}",
                            view.error,
                            view.debug_request
                        );
                        ready(view, cx)
                    })
                });
                if done {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        let click = |cx: &mut gpui::VisualTestContext, selector: &'static str| {
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let bounds = cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("missing control {selector}"));
            cx.simulate_click(bounds.center(), gpui::Modifiers::default());
        };

        wait(cx, "session launch", &|view, _| !view.selected.is_empty());
        cx.update(|_, cx| {
            entity.update(cx, |view, _| {
                view.send(Command::Pause(view.selected.clone()))
            });
        });
        wait(cx, "paused disassembly", &|view, _| {
            view.inspection_target().is_some_and(|target| {
                target.resolve(&view.sessions).is_some_and(|summary| {
                    !summary.telemetry.playing
                        && view.inspection.as_ref().is_some_and(|snapshot| {
                            snapshot.panel == Panel::Cpu
                                && snapshot.view.instruction_width.is_some()
                                && snapshot
                                    .view
                                    .rows
                                    .iter()
                                    .any(|row| row.key == "instruction")
                        })
                })
            })
        });
        let (target, address) = cx.update(|_, cx| {
            let view = entity.read(cx);
            let address = view
                .inspection
                .as_ref()
                .unwrap()
                .view
                .rows
                .iter()
                .find_map(|row| {
                    (row.key == "instruction")
                        .then(|| inspection::instruction_address(&row.value))
                        .flatten()
                })
                .unwrap();
            (view.inspection_target().unwrap(), address)
        });
        click(cx, "cpu-open-address");
        cx.update(|_, cx| {
            let view = entity.read(cx);
            let super::cpu_panel::CpuForm::Address { input, .. } = &view.cpu_forms[&target] else {
                panic!("address form was not open");
            };
            let field = input.field.clone();
            field.update(cx, |field, cx| {
                field.set_text(format!("0x{address:08x}"), cx)
            });
        });
        click(cx, "cpu-form-submit");
        wait(cx, "address navigation", &|view, _| {
            view.inspection.as_ref().is_some_and(|snapshot| {
                snapshot
                    .cpu
                    .address
                    .is_some_and(|(selected, _)| selected == address)
                    && snapshot.view.rows.iter().any(|row| {
                        row.key == "instruction"
                            && inspection::instruction_address(&row.value) == Some(address)
                    })
            })
        });

        click(cx, "cpu-core");
        click(cx, "popup-item-1");
        wait(cx, "core 1 registers", &|view, _| {
            view.inspection.as_ref().is_some_and(|snapshot| {
                snapshot.cpu.core == 1 && snapshot.view.rows.iter().any(|row| row.key == "r0")
            })
        });
        click(cx, "cpu-register-0");
        cx.update(|_, cx| {
            let view = entity.read(cx);
            let super::cpu_panel::CpuForm::Register {
                input,
                core,
                register,
            } = &view.cpu_forms[&target]
            else {
                panic!("register form was not open");
            };
            assert_eq!((*core, *register), (1, 0));
            let field = input.field.clone();
            field.update(cx, |field, cx| field.set_text("0x12345678".to_owned(), cx));
        });
        click(cx, "cpu-form-submit");
        wait(cx, "core 1 r0 readback", &|view, _| {
            view.inspection.as_ref().is_some_and(|snapshot| {
                snapshot.cpu.core == 1
                    && snapshot
                        .view
                        .rows
                        .iter()
                        .any(|row| row.key == "r0" && row.value == "12345678")
            })
        });
        click(cx, "cpu-core");
        click(cx, "popup-item-0");
        click(cx, "cpu-follow-pc");
        wait(cx, "core 0 PC", &|view, _| {
            view.inspection.as_ref().is_some_and(|snapshot| {
                snapshot.cpu == inspection::CpuQuery::default()
                    && snapshot
                        .view
                        .rows
                        .iter()
                        .any(|row| row.key == "instruction" && row.lane == "PC")
            })
        });
        click(cx, "cpu-open-memory");
        let get_memory = |view: &LotusWindow| {
            view.inspection
                .as_ref()
                .filter(|snap| snap.panel == Panel::Memory)
                .and_then(|snap| snap.view.memory.as_ref())
                .and_then(|data| data.page.as_ref())
                .map(|page| (page.address, page.bytes[0]))
        };
        wait(cx, "mapped memory page", &|view, _| {
            get_memory(view).is_some()
        });
        let (memory_address, original) = cx.update(|_, cx| get_memory(entity.read(cx)).unwrap());
        let address = memory_address;
        for desired in [!original, original] {
            click(cx, "memory-edit");
            cx.update(|_, cx| {
                let input = entity.read(cx).memory_panels[&target]
                    .edit
                    .as_ref()
                    .unwrap()
                    .input
                    .field
                    .clone();
                input.update(cx, |input, cx| input.set_text(format!("{desired:02x}"), cx));
            });
            click(cx, "memory-write");
            wait(cx, "memory write and reload", &|view, _| {
                matches!(&view.debug_request, Some(request) if request.operation == "write_memory"
                    && matches!(request.status, DebugStatus::Finished { .. }))
                    && get_memory(view) == Some((memory_address, desired))
            });
        }
        cx.update(|_, cx| entity.update(cx, |view, _| view.jump_to_memory(&target, Some(0))));
        wait(cx, "unmapped address", &|view, _| {
            view.inspection
                .as_ref()
                .filter(|snap| snap.offset == Some(1))
                .and_then(|snap| snap.view.memory.as_ref())
                .is_some_and(|memory| {
                    memory.page.is_none()
                        && memory
                            .error
                            .as_ref()
                            .is_some_and(|message| message.contains("unmapped"))
                })
        });
        cx.update(|_, cx| entity.update(cx, |view, _| view.show_panel(Panel::Cpu)));
        wait(cx, "return to CPU", &|view, _| {
            view.inspection
                .as_ref()
                .is_some_and(|snap| snap.panel == Panel::Cpu)
        });
        click(
            cx,
            Box::leak(format!("instruction-{address:08x}").into_boxed_str()),
        );
        wait(cx, "breakpoint manager", &|view, _| {
            view.inspection_panel == Panel::Breakpoints
                && view
                    .breakpoint_list(&target)
                    .is_some_and(|list| list.breakpoints.is_empty() && list.watchpoints.is_some())
        });
        click(cx, "breakpoint-add");
        click(cx, "breakpoint-submit");
        wait(cx, "created breakpoint", &|view, _| {
            view.breakpoint_list(&target)
                .is_some_and(|list| list.breakpoints.iter().any(|bp| bp.address == address))
        });
        click(cx, "breakpoint-configure");
        cx.update(|window, cx| {
            let view = entity.read(cx);
            let Some(Form::Edit(edit)) = &view.breakpoint_panels[&target].form else {
                panic!("expected configuration form");
            };
            window.focus(&edit.hit_after.field.read(cx).focus_handle(cx), cx);
        });
        cx.simulate_keystrokes("backspace 1");
        click(cx, "breakpoint-condition");
        click(cx, "popup-item-1");
        cx.update(|_, cx| {
            let view = entity.read(cx);
            let Some(Form::Edit(edit)) = &view.breakpoint_panels[&target].form else {
                panic!("expected configuration form");
            };
            assert_eq!(edit.condition_kind, 1);
            let fields = [&edit.reference, &edit.value, &edit.mask_or_width]
                .map(|input| input.field.clone());
            for (field, text) in fields.iter().zip(["17", "0", "0x0"]) {
                field.update(cx, |field, cx| field.set_text(text.to_owned(), cx));
            }
        });
        click(cx, "breakpoint-submit"); // Register 17 is invalid, so the form stays open.
        cx.update(|_, cx| {
            let view = entity.read(cx);
            let Some(Form::Edit(edit)) = &view.breakpoint_panels[&target].form else {
                panic!("invalid register unexpectedly accepted");
            };
            edit.reference
                .field
                .clone()
                .update(cx, |field, cx| field.set_text("0".to_owned(), cx));
        });
        click(cx, "breakpoint-submit");
        wait(cx, "acknowledged condition and threshold", &|view, _| {
            view.breakpoint_list(&target).is_some_and(|list| {
                list.breakpoints[0]
                    .host_configuration
                    .as_ref()
                    .is_some_and(|settings| {
                        settings.hit_after == 1
                            && matches!(
                                settings.condition,
                                Some(Condition::RegisterEquals {
                                    register: 0,
                                    value: 0,
                                    mask: 0
                                })
                            )
                    })
            })
        });
        cx.update(|_, cx| {
            entity.update(cx, |view, _| {
                view.debug_command(
                    &Some(target.clone()),
                    Panel::Breakpoints,
                    D::Advance { frames: 1 },
                );
            });
        });
        wait(cx, "breakpoint hit", &|view, _| {
            view.breakpoint_list(&target)
                .is_some_and(|list| list.breakpoints[0].hits == Some(1))
        });
        click(cx, "breakpoint-remove");
        click(cx, "breakpoint-submit");
        wait(cx, "removed breakpoint", &|view, _| {
            view.inspection_panel == Panel::Breakpoints
                && view
                    .breakpoint_list(&target)
                    .is_some_and(|list| list.breakpoints.is_empty())
        });
        click(cx, "breakpoint-add");
        click(cx, "breakpoint-kind");
        click(cx, "popup-item-2");
        click(cx, "breakpoint-submit");
        wait(cx, "created write watchpoint", &|view, _| {
            view.breakpoint_list(&target).is_some_and(|list| {
                list.breakpoints
                    .iter()
                    .any(|bp| bp.kind == BreakpointKind::Write && bp.address == address)
            })
        });
        cx.update(|_, cx| entity.update(cx, |view, _| view.jump_to_memory(&target, Some(address))));
        wait(cx, "watchpoint target memory", &|view, _| {
            get_memory(view).is_some()
        });
        click(cx, "memory-watch");
        wait(cx, "watchpoint selection", &|view, _| {
            view.breakpoint_list(&target).is_some()
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("breakpoint-configure").is_some());
        for (index, kind) in [
            ("popup-item-1", BreakpointKind::Read),
            ("popup-item-3", BreakpointKind::Access),
        ] {
            click(cx, "breakpoint-add");
            click(cx, "breakpoint-kind");
            click(cx, index);
            click(cx, "breakpoint-submit");
            wait(cx, "created watchpoint", &|view, _| {
                view.breakpoint_list(&target).is_some_and(|list| {
                    list.breakpoints
                        .iter()
                        .any(|bp| bp.kind == kind && bp.address == address)
                })
            });
        }
        click(cx, "breakpoint-add");
        cx.update(|_, cx| {
            entity.update(cx, |view, _| view.send(Command::Reset(target.id.clone())));
        });
        wait(cx, "reset expires the open form", &|view, _| {
            view.inspection_target()
                .is_some_and(|current| current != target)
                && !view.breakpoint_panels.contains_key(&target)
        });
        cx.update(|_, cx| entity.update(cx, |view, _| view.send(Command::Stop(target.id.clone()))));
        wait(cx, "worker stop", &|view, _| {
            target.resolve(&view.sessions).is_none()
        });
    }
}
