use super::*;
use breakpoints::Input;
use silvestris::controls::{ControlSize, PopUpButton};

pub(in crate::lotus_window) enum CpuForm {
    Address {
        input: Input,
        thumb: bool,
    },
    Register {
        input: Input,
        core: u32,
        register: u32,
    },
}

impl LotusWindow {
    fn cpu_edit_block(&self, target: &SessionTarget) -> Option<&'static str> {
        let Some(summary) = target.resolve(&self.sessions) else {
            return Some("Session or guest changed");
        };
        if !self.control {
            Some("Control is disabled for this host")
        } else if summary.telemetry.desktop_debug_lease_blocked {
            Some("A different client holds the debugger lease")
        } else if summary.telemetry.playing || summary.telemetry.state != SessionState::Paused {
            Some("Pause the guest to use CPU execution controls or edit registers")
        } else if self.debug_request.as_ref().is_some_and(|request| {
            request.target == *target
                && request.panel == Panel::Cpu
                && matches!(request.status, DebugStatus::Running)
        }) {
            Some("Waiting for the CPU command")
        } else {
            None
        }
    }

    fn render_cpu_form(
        &self,
        target: &SessionTarget,
        form: &CpuForm,
        cx: &mut Context<Self>,
    ) -> Div {
        let palette = theme::palette(cx);
        let mut body = div().flex().flex_col().gap_2().px_2().pb_2();
        let mut command = None;
        let mut jump = None;
        let validation = match form {
            CpuForm::Address { input, thumb } => {
                let choose = target.clone();
                body = body.child(input.render("Guest address", cx)).child(
                    PopUpButton::new(
                        "cpu-address-mode",
                        "Instruction mode",
                        vec!["ARM".into(), "Thumb".into()],
                    )
                    .size(ControlSize::Small)
                    .selected(Some(usize::from(*thumb)))
                    .on_choose(cx.listener(
                        move |view, index: &usize, _, cx| {
                            if let Some(CpuForm::Address { thumb, .. }) =
                                view.cpu_forms.get_mut(&choose)
                            {
                                *thumb = *index == 1;
                                cx.notify();
                            }
                        },
                    )),
                );
                match input.number::<u32>("Address", cx) {
                    Ok(address) if address.is_multiple_of(if *thumb { 2 } else { 4 }) => {
                        jump = Some((address, *thumb));
                        None
                    }
                    Ok(_) => Some("Align the address to the selected instruction mode".into()),
                    Err(error) => Some(error),
                }
            }
            CpuForm::Register {
                input,
                core,
                register,
            } => {
                body =
                    body.child(input.render(&format!("New value for core {core} r{register}"), cx));
                match input.number("Register value", cx) {
                    Ok(value) => {
                        command = Some(D::WriteRegister {
                            core: *core,
                            register: *register,
                            value,
                        });
                        self.cpu_edit_block(target).map(str::to_owned)
                    }
                    Err(error) => Some(error),
                }
            }
        };
        if let Some(error) = &validation {
            body = body.child(
                div()
                    .text_color(palette.text_secondary)
                    .child(error.clone()),
            );
        }
        let submit = target.clone();
        let cancel = target.clone();
        body.child(
            div()
                .flex()
                .gap_1()
                .child(
                    small_button(
                        palette,
                        "cpu-form-submit",
                        if jump.is_some()
                            || command.is_none() && matches!(form, CpuForm::Address { .. })
                        {
                            "Go"
                        } else {
                            "Write register"
                        },
                        Tone::Plain,
                        validation.is_none(),
                    )
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if view.inspection_panel != Panel::Cpu
                            || view.inspection_target().as_ref() != Some(&submit)
                        {
                            return;
                        }
                        if let Some(location) = jump {
                            view.inspector_state
                                .panel_mut(&submit, Panel::Cpu)
                                .cpu
                                .address = Some(location);
                            view.sync_inspection_selection();
                        } else if let Some(command) = command
                            .as_ref()
                            .filter(|_| view.cpu_edit_block(&submit).is_none())
                        {
                            view.debug_command(&Some(submit.clone()), Panel::Cpu, command.clone());
                        } else {
                            return;
                        }
                        view.cpu_forms.remove(&submit);
                        cx.notify();
                    })),
                )
                .child(
                    small_button(palette, "cpu-form-cancel", "Cancel", Tone::Plain, true).on_click(
                        cx.listener(move |view, _, _, cx| {
                            view.cpu_forms.remove(&cancel);
                            cx.notify();
                        }),
                    ),
                ),
        )
    }

    pub(super) fn open_cpu_register(
        &mut self,
        target: &SessionTarget,
        core: u32,
        register: u32,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.inspection_panel != Panel::Cpu
            || self.inspection_target().as_ref() != Some(target)
            || self.cpu_query(target).core != core
            || self.cpu_edit_block(target).is_some()
        {
            return;
        }
        self.cpu_forms.insert(
            target.clone(),
            CpuForm::Register {
                input: Input::new(window, cx, "Register value", format!("0x{text}")),
                core,
                register,
            },
        );
        cx.notify();
    }

    pub(super) fn render_cpu_controls(
        &self,
        target: &SessionTarget,
        snapshot: Option<&InspectionSnapshot>,
        cx: &mut Context<Self>,
    ) -> Div {
        let palette = theme::palette(cx);
        let query = self.cpu_query(target);
        let choose = target.clone();
        let browse = target.clone();
        let follow = target.clone();
        let cpu_view = snapshot.filter(|snap| snap.offset.is_some() && snap.view.error.is_none());
        let thumb = cpu_view.and_then(|snap| {
            snap.view
                .rows
                .iter()
                .find(|row| row.key == "CPSR")
                .map(|row| row.value.ends_with(" · Thumb"))
        });
        let mut controls = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap_1()
            .p_2()
            .text_xs()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .child(
                        PopUpButton::new(
                            "cpu-core",
                            "CPU core",
                            (0..4).map(|i| format!("Core {i}").into()).collect(),
                        )
                        .size(ControlSize::Small)
                        .selected(Some(query.core as usize))
                        .on_choose(cx.listener(
                            move |view, index: &usize, _, cx| {
                                if view.inspection_target().as_ref() == Some(&choose) && *index < 4
                                {
                                    view.inspector_state.panel_mut(&choose, Panel::Cpu).cpu.core =
                                        *index as u32;
                                    if matches!(
                                        view.cpu_forms.get(&choose),
                                        Some(CpuForm::Register { .. })
                                    ) {
                                        view.cpu_forms.remove(&choose);
                                    }
                                    view.sync_inspection_selection();
                                    cx.notify();
                                }
                            },
                        )),
                    )
                    .child(
                        small_button(
                            palette,
                            "cpu-open-address",
                            "Go to address",
                            Tone::Plain,
                            true,
                        )
                        .on_click(cx.listener(
                            move |view, _, window, cx| {
                                if view.inspection_target().as_ref() == Some(&browse) {
                                    let (text, thumb) = query.address.map_or_else(
                                        || (String::new(), thumb.unwrap_or(false)),
                                        |(address, thumb)| (format!("0x{address:08x}"), thumb),
                                    );
                                    let input = Input::new(window, cx, "Guest address", text);
                                    view.cpu_forms
                                        .insert(browse.clone(), CpuForm::Address { input, thumb });
                                    cx.notify();
                                }
                            },
                        )),
                    )
                    .child(
                        small_button(
                            palette,
                            "cpu-follow-pc",
                            "Follow PC",
                            Tone::Plain,
                            query.address.is_some(),
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            if view.inspection_target().as_ref() == Some(&follow) {
                                view.inspector_state
                                    .panel_mut(&follow, Panel::Cpu)
                                    .cpu
                                    .address = None;
                                view.cpu_forms.remove(&follow);
                                view.sync_inspection_selection();
                                cx.notify();
                            }
                        })),
                    ),
            )
            .child(query.address.map_or_else(
                || "Following PC".into(),
                |(address, thumb)| {
                    format!("0x{address:08x} · {}", if thumb { "Thumb" } else { "ARM" })
                },
            ));
        if let Some(form) = self.cpu_forms.get(target) {
            controls = controls.child(self.render_cpu_form(target, form, cx));
        }
        let blocked = self.cpu_edit_block(target);
        let executable = cpu_view.is_some() && blocked.is_none();
        let core = query.core;
        let mut commands = vec![
            ("Step", D::Step { core, count: 1 }, executable),
            ("Over", D::StepOver { core }, executable),
            ("Out", D::StepOut { core }, executable),
            ("Back", D::ReverseStep { core }, executable),
        ];
        if let Some((address, address_thumb)) = query.address {
            commands.push((
                "Run to address",
                D::RunTo { core, address },
                executable && thumb == Some(address_thumb),
            ));
            if thumb.is_some_and(|current| current != address_thumb) {
                controls =
                    controls.child("Run to address requires the core's current instruction mode");
            }
        }
        let mut actions = div().flex().flex_wrap().gap_1();
        for (label, command, enabled) in commands {
            let owner = target.clone();
            actions = actions.child(
                small_button(
                    palette,
                    format!("debug-{label}"),
                    label,
                    Tone::Plain,
                    enabled,
                )
                .on_click(cx.listener(move |view, _, _, _| {
                    if enabled
                        && view.cpu_query(&owner) == query
                        && view.cpu_edit_block(&owner).is_none()
                    {
                        view.debug_command(&Some(owner.clone()), Panel::Cpu, command.clone());
                    }
                })),
            );
        }
        controls.child(actions).children(blocked.map(str::to_owned))
    }
}
