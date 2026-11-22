use super::*;
use breakpoints::Input;
use inspection::{MemoryInspection, MemoryPage, MEMORY_PAGE_BYTES};

#[derive(Default)]
pub(super) struct MemoryPanel {
    address: Option<Input>,
    pub(super) edit: Option<MemoryEdit>,
    cursor: Option<u32>,
    show_regions: bool,
}

pub(super) struct MemoryEdit {
    address: u32,
    original: Vec<u8>,
    pub(super) input: Input,
}

fn parse_bytes(text: &str, maximum: usize) -> Result<Vec<u8>, &'static str> {
    let mut bytes = Vec::new();
    for value in text.split_ascii_whitespace() {
        if value.len() != 2 || bytes.len() == maximum {
            return Err("Enter up to 16 two-digit hex bytes within the loaded page");
        }
        bytes.push(u8::from_str_radix(value, 16).map_err(|_| "Use two-digit hexadecimal bytes")?);
    }
    if bytes.is_empty() {
        return Err("Enter at least one byte");
    }
    Ok(bytes)
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn ascii(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&byte| {
            if (b' '..=b'~').contains(&byte) {
                char::from(byte)
            } else {
                '.'
            }
        })
        .collect()
}

impl LotusWindow {
    pub(super) fn jump_to_memory(&mut self, target: &SessionTarget, address: Option<u32>) {
        if self.inspection_target().as_ref() != Some(target) {
            return;
        }
        let state = self.memory_panels.entry(target.clone()).or_default();
        state.address = None;
        state.edit = None;
        state.cursor = address;
        state.show_regions = false;
        self.inspector_state
            .panel_mut(target, Panel::Memory)
            .set_memory_address(address);
        self.show_panel(Panel::Memory);
    }

    fn memory_edit_block(&self, target: &SessionTarget) -> Option<&'static str> {
        let Some(summary) = target.resolve(&self.sessions) else {
            return Some("Session or guest changed");
        };
        if !self.control {
            Some("Control is disabled for this host")
        } else if summary.telemetry.desktop_debug_lease_blocked {
            Some("A different client holds the debugger lease")
        } else if summary.telemetry.backend_name == "AZAHAR" {
            Some("Memory writes require CITnLO")
        } else if summary.telemetry.playing || summary.telemetry.state != SessionState::Paused {
            Some("Pause the guest to edit memory")
        } else if self.debug_request.as_ref().is_some_and(|request| {
            request.target == *target
                && request.panel == Panel::Memory
                && matches!(request.status, DebugStatus::Running)
        }) {
            Some("Waiting for memory write and readback")
        } else {
            None
        }
    }

    fn render_memory_edit(
        &self,
        target: &SessionTarget,
        edit: &MemoryEdit,
        cx: &mut Context<Self>,
    ) -> Div {
        let palette = theme::palette(cx);
        let result = parse_bytes(edit.input.text(cx), edit.original.len());
        let mut body = div().flex().flex_col().gap_1().child(
            edit.input
                .render(&format!("Hex bytes at 0x{:08x}", edit.address), cx),
        );
        let address = edit.address;
        let (write, cancel) = (target.clone(), target.clone());
        match result {
            Ok(bytes) => {
                let original = edit.original[..bytes.len()].to_vec();
                let allowed = original != bytes && self.memory_edit_block(target).is_none();
                body = body
                    .child(div().font_family(palette.mono).child(format!(
                        "{} → {}",
                        hex(&original),
                        hex(&bytes)
                    )))
                    .child(
                        small_button(
                            palette,
                            "memory-write",
                            "Commit write",
                            Tone::Danger,
                            allowed,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            if allowed
                                && view.inspection_panel == Panel::Memory
                                && view.inspection_target().as_ref() == Some(&write)
                                && view.memory_edit_block(&write).is_none()
                            {
                                view.send(Command::PatchMemory {
                                    target: write.clone(),
                                    command: D::WriteMemory {
                                        address,
                                        bytes: bytes.clone(),
                                    },
                                    original: original.clone(),
                                });
                                if let Some(state) = view.memory_panels.get_mut(&write) {
                                    state.edit = None;
                                }
                                cx.notify();
                            }
                        })),
                    );
            }
            Err(error) => body = body.child(error),
        }
        body.child(
            small_button(palette, "memory-discard", "Discard edit", Tone::Plain, true).on_click(
                cx.listener(move |view, _, _, cx| {
                    if let Some(state) = view.memory_panels.get_mut(&cancel) {
                        state.edit = None;
                        cx.notify();
                    }
                }),
            ),
        )
    }

    fn render_memory_regions(
        &self,
        target: &SessionTarget,
        memory: &MemoryInspection,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = theme::palette(cx);
        let regions = Arc::new(memory.regions.clone());
        let weak = cx.entity().downgrade();
        let target = target.clone();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(format!(
                "Mapped regions · {} shown · {} omitted",
                regions.len(),
                memory.omitted_regions
            ))
            .child("Memory reads use the worker's current guest process.")
            .child(
                silvestris::controls::table::virtual_table(
                    "memory-regions",
                    regions.len(),
                    move |range, _, cx| {
                        range
                            .map(|index| {
                                let region = &regions[index];
                                let (weak, target, address) =
                                    (weak.clone(), target.clone(), region.address);
                                silvestris::controls::table::code_row(
                                    ("memory-region", index),
                                    27.0,
                                    cx,
                                )
                                .child(format!(
                                    "PID {} · {:08X} +{:X} · perms {:X}",
                                    region.process, address, region.length, region.permissions
                                ))
                                .cursor_pointer()
                                .hover(|row| row.bg(theme.surface_hover))
                                .on_click(move |_, _, cx| {
                                    let _ = weak.update(cx, |view, cx| {
                                        view.jump_to_memory(&target, Some(address));
                                        cx.notify();
                                    });
                                })
                            })
                            .collect()
                    },
                )
                .flex_1()
                .min_h_0(),
            )
    }

    fn render_memory_bytes(
        &self,
        target: &SessionTarget,
        page: &MemoryPage,
        cursor: u32,
        cx: &mut Context<Self>,
    ) -> Div {
        const WIDTH: usize = 8;
        let palette = theme::palette(cx);
        let original = Arc::new(page.bytes.clone());
        let base = page.address;
        let target = target.clone();
        let weak = cx.entity().downgrade();
        div().flex().flex_col().flex_1().min_h_0().child(
            silvestris::controls::table::virtual_table(
                "memory-bytes",
                original.len().div_ceil(WIDTH),
                move |range, _, cx| {
                    range
                        .map(|index| {
                            let offset = index * WIDTH;
                            let bytes = &original[offset..original.len().min(offset + WIDTH)];
                            let address = base + offset as u32;
                            let mut row = silvestris::controls::table::code_row(
                                ("memory-row", index),
                                24.0,
                                cx,
                            )
                            .child(
                                div()
                                    .w(px(68.0))
                                    .flex_none()
                                    .child(format!("{address:08X}")),
                            );
                            for (column, byte) in bytes.iter().enumerate() {
                                let (owner, weak) = (target.clone(), weak.clone());
                                let cell = address + column as u32;
                                row = row.child(
                                    div()
                                        .id(("memory-byte", column))
                                        .debug_selector(move || format!("memory-byte-{cell:08x}"))
                                        .w(px(19.0))
                                        .flex_none()
                                        .text_center()
                                        .when(cursor == cell, |cell| {
                                            cell.bg(palette.surface_active)
                                        })
                                        .hover(|cell| cell.bg(palette.surface_hover))
                                        .cursor_pointer()
                                        .child(format!("{byte:02X}"))
                                        .on_click(move |_, _, cx| {
                                            let _ = weak.update(cx, |view, cx| {
                                                if view.inspection_target().as_ref() == Some(&owner)
                                                    && inspection::memory_address(
                                                        view.inspection_offset(
                                                            Some(&owner),
                                                            Panel::Memory,
                                                        ),
                                                    ) == Some(base)
                                                {
                                                    let state = view
                                                        .memory_panels
                                                        .entry(owner.clone())
                                                        .or_default();
                                                    state.cursor = Some(cell);
                                                    state.edit = None;
                                                    cx.notify();
                                                }
                                            });
                                        }),
                                );
                            }
                            row.child(div().pl_2().child(ascii(bytes)))
                        })
                        .collect()
                },
            )
            .flex_1()
            .min_h_0(),
        )
    }

    pub(super) fn render_memory_panel(
        &self,
        target: &SessionTarget,
        snapshot: Option<&InspectionSnapshot>,
        cx: &mut Context<Self>,
    ) -> Div {
        let palette = theme::palette(cx);
        let state = self.memory_panels.get(target);
        let address =
            inspection::memory_address(self.inspection_offset(Some(target), Panel::Memory));
        let cursor = state.and_then(|state| state.cursor).or(address);
        let memory = snapshot.and_then(|snapshot| snapshot.view.memory.as_ref());
        let (open, regions, go) = (target.clone(), target.clone(), target.clone());
        let mut toolbar = div()
            .flex()
            .flex_wrap()
            .gap_1()
            .child(
                small_button(
                    palette,
                    "memory-open-address",
                    "Go to address",
                    Tone::Plain,
                    true,
                )
                .on_click(cx.listener(move |view, _, window, cx| {
                    if view.inspection_target().as_ref() == Some(&open) {
                        let state = view.memory_panels.entry(open.clone()).or_default();
                        state.address = Some(Input::new(
                            window,
                            cx,
                            "Guest address",
                            cursor.map_or_else(String::new, |a| format!("0x{a:08x}")),
                        ));
                        cx.notify();
                    }
                })),
            )
            .child(
                small_button(
                    palette,
                    "memory-show-regions",
                    "Mapped regions",
                    Tone::Plain,
                    memory.is_some(),
                )
                .on_click(cx.listener(move |view, _, _, cx| {
                    if view.inspection_target().as_ref() == Some(&regions) {
                        let state = view.memory_panels.entry(regions.clone()).or_default();
                        state.show_regions = !state.show_regions;
                        cx.notify();
                    }
                })),
            );
        if let Some(address) = address {
            for (id, label, dest) in [
                (
                    "memory-prev",
                    "Previous",
                    address.checked_sub(MEMORY_PAGE_BYTES),
                ),
                (
                    "memory-next",
                    "Next",
                    address.checked_add(inspection::memory_length(address)),
                ),
            ] {
                let owner = target.clone();
                toolbar = toolbar.child(
                    small_button(palette, id, label, Tone::Plain, dest.is_some()).on_click(
                        cx.listener(move |view, _, _, cx| {
                            view.jump_to_memory(&owner, dest);
                            cx.notify();
                        }),
                    ),
                );
            }
        }
        let mut body = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .p_2()
            .gap_1()
            .text_xs()
            .child(format!("{} · epoch {}", target.id, target.epoch))
            .child(toolbar);
        if let Some(input) = state.and_then(|state| state.address.as_ref()) {
            let next = input.number::<u32>("Address", cx);
            body =
                body.child(input.render("Guest address", cx))
                    .children(next.as_ref().err().cloned())
                    .child(
                        small_button(palette, "memory-go", "Go", Tone::Plain, next.is_ok())
                            .on_click(cx.listener(move |view, _, _, cx| {
                                if let Ok(address) = next {
                                    view.jump_to_memory(&go, Some(address));
                                    cx.notify();
                                }
                            })),
                    );
        }
        if let Some((kind, error)) = snapshot.and_then(|snapshot| snapshot.view.error.as_ref()) {
            let label = if *kind == std::io::ErrorKind::Unsupported {
                "Unsupported · "
            } else {
                ""
            };
            return body.child(format!("{label}{error}"));
        }
        let Some(memory) = memory else {
            return body.child("Loading memory…");
        };
        if address.is_none() || state.is_some_and(|state| state.show_regions) {
            return body.child(self.render_memory_regions(target, memory, cx));
        }
        if let Some(error) = &memory.error {
            return body.child(error.clone());
        }
        let Some(page) = memory
            .page
            .as_ref()
            .filter(|page| Some(page.address) == address)
        else {
            return body.child("The worker returned no data for this address");
        };
        let Some(cursor) = cursor else {
            return body;
        };
        body = body.child(format!(
            "0x{:08X} · {} of {} bytes · {}",
            page.address,
            page.bytes.len(),
            inspection::memory_length(page.address),
            if page.paused {
                "Paused safe point"
            } else {
                "Live safe-point sample"
            }
        ));
        let index = cursor.saturating_sub(page.address) as usize;
        if let Some(tail) = page.bytes.get(index..).filter(|slice| !slice.is_empty()) {
            if let Some(word) = tail
                .get(..4)
                .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            {
                body = body.child(format!(
                    "0x{cursor:08X} · u32 LE {:08X} · BE {:08X}",
                    u32::from_le_bytes(word),
                    u32::from_be_bytes(word)
                ));
            }
            let (edit, watch) = (target.clone(), target.clone());
            let original = tail[..tail.len().min(16)].to_vec();
            let blocked = self.memory_edit_block(target);
            let can_edit = page.paused && blocked.is_none();
            let mut actions = div()
                .flex()
                .flex_wrap()
                .gap_1()
                .child(
                    small_button(palette, "memory-edit", "Edit bytes", Tone::Plain, can_edit)
                        .on_click(cx.listener(move |view, _, window, cx| {
                            if can_edit
                                && view.inspection_target().as_ref() == Some(&edit)
                                && view.memory_edit_block(&edit).is_none()
                            {
                                let input = Input::new(
                                    window,
                                    cx,
                                    "Replacement hex bytes",
                                    hex(&original[..1]),
                                );
                                view.memory_panels.entry(edit.clone()).or_default().edit =
                                    Some(MemoryEdit {
                                        address: cursor,
                                        original: original.clone(),
                                        input,
                                    });
                                cx.notify();
                            }
                        })),
                )
                .child(
                    small_button(palette, "memory-watch", "Watchpoint", Tone::Plain, true)
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.jump_to_watchpoint(&watch, cursor);
                            cx.notify();
                        })),
                );
            for (id, label, text) in [
                (
                    "memory-copy-address",
                    "Copy address",
                    format!("0x{cursor:08x}"),
                ),
                ("memory-copy-hex", "Copy hex", hex(&page.bytes)),
                ("memory-copy-ascii", "Copy ASCII", ascii(&page.bytes)),
            ] {
                actions = actions.child(
                    small_button(palette, id, label, Tone::Plain, true).on_click(cx.listener(
                        move |_, _, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()))
                        },
                    )),
                );
            }
            body = body.child(actions).children(blocked);
            if let Some(edit) = state.and_then(|state| state.edit.as_ref()) {
                body = body.child(self.render_memory_edit(target, edit, cx));
            }
        }
        body.child(self.render_memory_bytes(target, page, cursor, cx))
    }
}
