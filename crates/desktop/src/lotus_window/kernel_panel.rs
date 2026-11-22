use super::*;
use crate::desktop_controller::DebugAction;
use breakpoints::Input;

#[derive(Default)]
pub(super) struct KernelPanel {
    process: Option<u32>,
    thread: Option<u32>,
    object: Option<(u64, u64)>,
    edit: Option<(Input, Input)>,
}

fn id(row: &Value, key: &str) -> Option<u32> {
    row[key]
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
}

fn status(value: &Value) -> &str {
    // Kernel::ThreadStatus from the pinned workers' thread.h.
    match value.as_u64() {
        Some(0) => "Running",
        Some(1) => "Ready",
        Some(2) => "Address arbiter",
        Some(3) => "Sleeping",
        Some(4) => "IPC",
        Some(5) => "Wait any",
        Some(6) => "Wait all",
        Some(7) => "HLE event",
        Some(8) => "Dormant",
        Some(9) => "Dead",
        _ => "Unknown",
    }
}

fn list(
    title: &'static str,
    rows: Vec<(u32, String)>,
    selected: Option<u32>,
    select: impl Fn(u32, &mut LotusWindow, &mut Context<LotusWindow>) + 'static,
    cx: &Context<LotusWindow>,
) -> impl IntoElement {
    let visible = rows.len().clamp(1, 6) as f32;
    let theme = theme::palette(cx);
    let select = std::rc::Rc::new(select);
    let weak = cx.entity().downgrade();
    silvestris::controls::table::virtual_table(title, rows.len(), move |range, _, cx| {
        range
            .map(|index| {
                let (id, text) = &rows[index];
                let (id, select, weak) = (*id, select.clone(), weak.clone());
                silvestris::controls::table::code_row((title, index), 26.0, cx)
                    .when(Some(id) == selected, |row| row.bg(theme.surface_active))
                    .hover(|row| row.bg(theme.surface_hover))
                    .cursor_pointer()
                    .child(text.clone())
                    .on_click(move |_, _, cx| {
                        let _ = weak.update(cx, |view, cx| {
                            select(id, view, cx);
                            cx.notify();
                        });
                    })
            })
            .collect()
    })
    .h(px(visible * 26.0))
    .flex_none()
}

impl LotusWindow {
    pub(super) fn render_kernel_panel(
        &self,
        target: &SessionTarget,
        snapshot: Option<&InspectionSnapshot>,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut body = div()
            .id("kernel-panel")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_2()
            .p_2()
            .text_xs()
            .overflow_y_scroll()
            .child(format!("{} · epoch {}", target.id, target.epoch));
        if let Some((_, error)) = snapshot.and_then(|snapshot| snapshot.view.error.as_ref()) {
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(body.child(error.clone()));
        }
        let Some(data) = snapshot.and_then(|snapshot| snapshot.view.data.as_deref()) else {
            return div().child("Loading processes and threads…");
        };
        let Some(summary) = target.resolve(&self.sessions) else {
            return div().child("Guest changed");
        };
        let advanced = summary.telemetry.backend_name == "CITnLO";
        let paused = !summary.telemetry.playing && summary.telemetry.state == SessionState::Paused;
        let control = self.inspector_control(target, Panel::Threads);
        let can_read = advanced
            && !self
                .panel_history(target, Panel::Threads)
                .any(|r| matches!(r.status, DebugStatus::Running));
        let state = self.kernel_panels.get(target);
        let processes = data["processes"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let threads = data["threads"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let process = state
            .and_then(|state| state.process)
            .filter(|pid| processes.iter().any(|row| id(row, "id") == Some(*pid)));
        let process_rows = processes
            .iter()
            .filter_map(|row| {
                Some((
                    id(row, "id")?,
                    format!(
                        "PID {} · {} · status {}",
                        row["id"], row["name"], row["status"]
                    ),
                ))
            })
            .collect::<Vec<_>>();
        if process_rows.is_empty() {
            body = body.child("No guest processes.");
        } else {
            let owner = target.clone();
            body = body.child("Processes").child(list(
                "kernel-processes",
                process_rows,
                process,
                move |id, view, _| {
                    if view.inspection_target().as_ref() == Some(&owner) {
                        view.kernel_panels.insert(
                            owner.clone(),
                            KernelPanel {
                                process: Some(id),
                                ..Default::default()
                            },
                        );
                    }
                },
                cx,
            ));
        }
        let thread_rows = threads
            .iter()
            .filter(|row| process.is_some() && id(row, "process") == process)
            .filter_map(|row| {
                Some((
                    id(row, "id")?,
                    format!(
                        "TID {} · {} · core {} · priority {} · {}",
                        row["id"],
                        row["name"],
                        row["core"],
                        row["priority"],
                        status(&row["status"])
                    ),
                ))
            })
            .collect::<Vec<_>>();
        let thread = state
            .and_then(|s| s.thread)
            .filter(|id| thread_rows.iter().any(|row| row.0 == *id));
        if let Some(process) = process {
            if thread_rows.is_empty() {
                body = body.child("No threads in this process.");
            } else {
                let owner = target.clone();
                body = body.child("Threads").child(list(
                    "kernel-threads",
                    thread_rows,
                    thread,
                    move |id, view, _| {
                        if view.inspection_target().as_ref() == Some(&owner) {
                            let state = view.kernel_panels.entry(owner.clone()).or_default();
                            state.thread = Some(id);
                            state.edit = None;
                            if advanced {
                                view.send(Command::KernelAt {
                                    target: owner.clone(),
                                    process,
                                    thread: id,
                                    command: D::ThreadRegisters { thread: id },
                                });
                            }
                        }
                    },
                    cx,
                ));
            }
            if !advanced {
                body = body.child("This worker exposes process and thread snapshots. Thread registers, suspension, handles and cycle reports require CITnLO.");
            }
            if let Some(thread) = thread {
                if let Some(row) = threads.iter().find(|row| id(row, "id") == Some(thread)) {
                    body = body.child(format!(
                        "TID {thread} · wait address {} · saved PC {} · scheduling {}",
                        hex_string(&row["wait_address"]),
                        hex_string(&row["pc_saved"]),
                        if row["suspended"] == true {
                            "suspended"
                        } else {
                            "allowed"
                        }
                    ));
                }
                if let Some(edges) = data["wait_edges"].as_array() {
                    for edge in edges
                        .iter()
                        .filter(|edge| id(edge, "thread") == Some(thread))
                        .take(16)
                    {
                        body = body.child(format!(
                            "Waiting on {} {} · object {} · owner TID {}",
                            edge["type"], edge["name"], edge["object"], edge["owner_thread"]
                        ));
                    }
                }
                if advanced {
                    body = body.child(self.render_thread_details(
                        target,
                        (process, thread),
                        (paused, control),
                        state,
                        cx,
                    ));
                }
            }
            body = body.child(self.render_kernel_handles(target, process, can_read, state, cx));
        }
        body = body.child(Self::inspector_action(
            target,
            Panel::Threads,
            "kernel-deadlocks",
            "Inspect wait cycles",
            D::Deadlocks,
            can_read,
            cx,
        ));
        if let Some(data) = self
            .panel_history(target, Panel::Threads)
            .find(|request| matches!(request.native_command(), Some(D::Deadlocks)))
            .and_then(Self::reply_data)
        {
            body = body.child(format!(
                "Scope · {}",
                data["scope"]
                    .as_str()
                    .unwrap_or("Worker-reported wait edges")
            ));
            if let Some(cycles) = data["mutex_cycles"].as_array() {
                if cycles.is_empty() {
                    body = body.child("The worker reported no mutex cycles.");
                }
                for cycle in cycles.iter().take(64) {
                    body = body.child(format!("Mutex cycle · {cycle}"));
                }
            }
            body = body.child(
                div()
                    .id("kernel-deadlock-raw")
                    .max_h(px(120.0))
                    .overflow_y_scroll()
                    .font_family(theme::palette(cx).mono)
                    .child(inspection::json_preview(&data["wait_edges"], 16_384)),
            );
        }
        div().flex().flex_col().flex_1().min_h_0().child(body)
    }

    fn render_thread_details(
        &self,
        target: &SessionTarget,
        (process, thread): (u32, u32),
        (paused, control): (bool, bool),
        state: Option<&KernelPanel>,
        cx: &mut Context<Self>,
    ) -> Div {
        let palette = theme::palette(cx);
        let running = self
            .panel_history(target, Panel::Threads)
            .any(|r| matches!(r.status, DebugStatus::Running));
        let action = |cx: &mut Context<Self>, id, label, command: D, allowed: bool| {
            let owner = target.clone();
            small_button(palette, id, label, Tone::Plain, allowed && !running).on_click(
                cx.listener(move |view, _, _, cx| {
                    if allowed
                        && !running
                        && view.inspection_target().as_ref() == Some(&owner)
                        && view.inspection_panel == Panel::Threads
                        && view.kernel_panels.get(&owner).is_some_and(|state| {
                            state.process == Some(process) && state.thread == Some(thread)
                        })
                    {
                        view.send(Command::KernelAt {
                            target: owner.clone(),
                            process,
                            thread,
                            command: command.clone(),
                        });
                        cx.notify();
                    }
                }),
            )
        };
        let is_selected = |request: &&DebugRequest| matches!(&*request.action, DebugAction::KernelAt(_, pid, tid) if *pid == process && *tid == thread);
        let regs = self
            .panel_history(target, Panel::Threads)
            .filter(is_selected)
            .find(|r| {
                matches!(
                    r.native_command(),
                    Some(D::ThreadRegisters { .. } | D::WriteThreadRegister { .. })
                )
            })
            .and_then(Self::reply_data);
        let mut body = div().flex().flex_col().gap_1().child(
            div()
                .flex()
                .flex_wrap()
                .gap_1()
                .child(action(
                    cx,
                    "thread-refresh",
                    "Read registers",
                    D::ThreadRegisters { thread },
                    true,
                ))
                .child(action(
                    cx,
                    "thread-suspend",
                    "Suspend",
                    D::Suspend {
                        thread: Some(thread),
                        suspended: true,
                    },
                    paused && control,
                ))
                .child(action(
                    cx,
                    "thread-resume",
                    "Resume",
                    D::Suspend {
                        thread: Some(thread),
                        suspended: false,
                    },
                    paused && control,
                )),
        );
        let Some(regs) = regs else {
            return body;
        };
        body = body.child(if regs["saved_context"] == true {
            "Saved thread register sample"
        } else {
            "Active thread register sample"
        });
        let mut registers = div().flex().flex_wrap().gap_2().font_family(palette.mono);
        if let Some(values) = regs["registers"].as_array() {
            for (index, value) in values.iter().enumerate().take(16) {
                registers = registers.child(format!("r{index} {}", hex_string(value)));
            }
        }
        body = body.child(registers.child(format!("CPSR {}", hex_string(&regs["cpsr"]))));
        let owner = target.clone();
        body = body.child(
            small_button(
                palette,
                "thread-edit",
                "Edit register…",
                Tone::Plain,
                paused && control,
            )
            .on_click(cx.listener(move |view, _, window, cx| {
                if paused && control && view.inspection_target().as_ref() == Some(&owner) {
                    if let Some(state) = view
                        .kernel_panels
                        .get_mut(&owner)
                        .filter(|state| state.thread == Some(thread))
                    {
                        state.edit = Some((
                            Input::new(window, cx, "Register index", "0".into()),
                            Input::new(window, cx, "Register value", String::new()),
                        ));
                        cx.notify();
                    }
                }
            })),
        );
        if let Some((index, value)) = state.and_then(|state| state.edit.as_ref()) {
            body = body
                .child(index.render(
                    "Register index (0–15; 16 CPSR; 17–80 VFP; 81 FPSCR; 82 FPEXC)",
                    cx,
                ))
                .child(value.render("New value", cx));
            let command = index.number::<u32>("Index", cx).and_then(|register| {
                if register > 82 {
                    return Err("Register index must be 0–82".into());
                }
                Ok(D::WriteThreadRegister {
                    thread,
                    register,
                    value: value.number("Value", cx)?,
                })
            });
            match command {
                Ok(command) => {
                    body = body.child(action(
                        cx,
                        "thread-write",
                        "Write register",
                        command,
                        paused && control,
                    ))
                }
                Err(error) => body = body.child(error),
            }
        }
        if let Some(core) = id(regs, "core") {
            body = body.child(action(
                cx,
                "thread-backtrace",
                "Unwind active stack",
                D::Backtrace { core, limit: 32 },
                paused,
            ));
        } else {
            body = body.child("The pinned worker unwinds only an active core. This thread's saved PC is shown above.");
        }
        let trace = self
            .panel_history(target, Panel::Threads)
            .filter(is_selected)
            .find(|r| matches!(r.native_command(), Some(D::Backtrace { .. })))
            .and_then(Self::reply_data);
        if let Some(frames) = trace.and_then(|trace| trace["frames"].as_array()) {
            let thumb = regs["cpsr"].as_u64().is_some_and(|cpsr| cpsr & 32 != 0);
            for (index, frame) in frames.iter().take(32).enumerate() {
                let Some(pc) = id(frame, "pc") else {
                    continue;
                };
                let thumb = if index == 0 { thumb } else { pc & 1 != 0 };
                let core = id(regs, "core");
                let owner = target.clone();
                body = body.child(
                    div()
                        .flex()
                        .gap_2()
                        .child(format!("#{index} · 0x{pc:08x}"))
                        .child(
                            small_button(
                                palette,
                                format!("thread-frame-{index}"),
                                "Open CPU",
                                Tone::Plain,
                                true,
                            )
                            .on_click(cx.listener(
                                move |view, _, _, cx| {
                                    if view.inspection_target().as_ref() == Some(&owner) {
                                        let cpu = &mut view
                                            .inspector_state
                                            .panel_mut(&owner, Panel::Cpu)
                                            .cpu;
                                        cpu.address = Some((pc & !1, thumb));
                                        if let Some(core) = core {
                                            cpu.core = core;
                                        }
                                        view.show_panel(Panel::Cpu);
                                        view.sync_inspection_selection();
                                        cx.notify();
                                    }
                                },
                            )),
                        ),
                );
            }
        }
        body
    }

    fn render_kernel_handles(
        &self,
        target: &SessionTarget,
        process: u32,
        enabled: bool,
        state: Option<&KernelPanel>,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut body = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(Self::inspector_action(
                target,
                Panel::Threads,
                "kernel-handles",
                "Inspect handles",
                D::Handles,
                enabled,
                cx,
            ));
        let Some(data) = self
            .panel_history(target, Panel::Threads)
            .find(|request| matches!(request.native_command(), Some(D::Handles)))
            .and_then(Self::reply_data)
        else {
            return body;
        };
        let handles = data["handles"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter(|row| id(row, "process") == Some(process))
            .take(256)
            .collect::<Vec<_>>();
        if data["truncated"] == true {
            body = body.child("The worker capped the full handle snapshot at 256 entries.");
        }
        let rows = handles
            .iter()
            .filter_map(|row| {
                Some((
                    id(row, "handle")?,
                    format!(
                        "{} · {} · {}",
                        hex_string(&row["handle"]),
                        row["type"],
                        row["name"]
                    ),
                ))
            })
            .collect::<Vec<_>>();
        if rows.is_empty() {
            return body.child("No handles in this process snapshot.");
        }
        let selected = state.and_then(|state| state.object);
        let owner = target.clone();
        let identities = handles
            .iter()
            .filter_map(|row| Some((id(row, "handle")?, row["object"].as_u64()?)))
            .collect::<std::collections::BTreeMap<_, _>>();
        body = body.child(list(
            "kernel-objects",
            rows,
            selected.map(|(handle, _)| handle as u32),
            move |handle, view, _| {
                if view.inspection_target().as_ref() == Some(&owner) {
                    if let Some(state) = view
                        .kernel_panels
                        .get_mut(&owner)
                        .filter(|state| state.process == Some(process))
                    {
                        state.object = identities
                            .get(&handle)
                            .map(|object| (u64::from(handle), *object));
                    }
                }
            },
            cx,
        ));
        if let Some((handle, object)) = selected {
            if let Some(row) = handles
                .iter()
                .find(|row| row["handle"] == handle && row["object"] == object)
            {
                body = body.child(inspection::json_preview(row, 2048));
            }
        }
        body
    }
}
