use super::*;
use breakpoints::Input;
use gpui::App;
use lotus_protocol::debug::ScanMode;

const PAGE: u32 = 256;

pub(super) struct SearchForm {
    address: Input,
    length: Input,
    width: Input,
    value: Input,
    upper: Input,
    base: Input,
    offsets: Input,
}

impl SearchForm {
    pub(super) fn new(
        window: &mut Window,
        cx: &mut Context<LotusWindow>,
        cursor: Option<u32>,
    ) -> Self {
        let base = cursor.map_or_else(String::new, |address| format!("0x{address:08x}"));
        let mut input = |label, text: String| Input::new(window, cx, label, text);
        Self {
            address: input("Scan address", base.clone()),
            length: input("Scan length", "256".into()),
            width: input("Value width", "4".into()),
            value: input("Value", String::new()),
            upper: input("Range maximum", String::new()),
            base: input("Pointer address", base),
            offsets: input("Signed offsets", "0".into()),
        }
    }

    fn value(input: &Input, width: u32, cx: &App) -> Result<u64, String> {
        let value = input.number("Value", cx)?;
        if width < 8 && value >= (1_u64 << (width * 8)) {
            return Err("Value does not fit the scan width".into());
        }
        Ok(value)
    }

    fn scan(&self, cx: &App) -> Result<D, String> {
        let width = self.width.number("Width", cx)?;
        if !matches!(width, 1 | 2 | 4 | 8) {
            return Err("Use a width of 1, 2, 4 or 8 bytes".into());
        }
        let command = D::Scan {
            address: self.address.number("Address", cx)?,
            length: self.length.number("Length", cx)?,
            width,
            value: Self::value(&self.value, width, cx)?,
        };
        command.validate().map_err(str::to_owned)?;
        Ok(command)
    }

    fn narrow(&self, mode: ScanMode, width: u32, cx: &App) -> Result<D, String> {
        let command = D::ScanNarrow {
            value: matches!(mode, ScanMode::Exact | ScanMode::Range)
                .then(|| Self::value(&self.value, width, cx))
                .transpose()?,
            upper: matches!(mode, ScanMode::Range)
                .then(|| Self::value(&self.upper, width, cx))
                .transpose()?,
            mode,
        };
        command.validate().map_err(str::to_owned)?;
        Ok(command)
    }

    fn chain(&self, cx: &App) -> Result<D, String> {
        let offsets = self
            .offsets
            .text(cx)
            .split_ascii_whitespace()
            .take(33)
            .map(|text| {
                crate::debugger_palette::parse_integer(text, true)
                    .ok()
                    .and_then(|value| value.as_i64())
                    .and_then(|value| i32::try_from(value).ok())
                    .ok_or_else(|| format!("Invalid signed offset {text}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if offsets.is_empty() {
            return Err("Enter at least one signed offset".into());
        }
        let command = D::PointerChain {
            base: self.base.number("Pointer address", cx)?,
            offsets,
        };
        command.validate().map_err(str::to_owned)?;
        Ok(command)
    }
}

impl LotusWindow {
    pub(super) fn panel_history<'a>(
        &'a self,
        target: &'a SessionTarget,
        panel: Panel,
    ) -> impl Iterator<Item = &'a DebugRequest> {
        self.debug_history
            .iter()
            .rev()
            .filter(move |request| request.panel == panel && &request.target == target)
    }

    pub(super) fn reply_data(request: &DebugRequest) -> Option<&Value> {
        match &request.status {
            DebugStatus::Finished { view, .. } => view.data.as_deref(),
            _ => None,
        }
    }

    pub(super) fn render_memory_search(
        &self,
        target: &SessionTarget,
        form: &SearchForm,
        cx: &mut Context<Self>,
    ) -> Div {
        let scan = self.panel_history(target, Panel::Memory).find(|request| {
            matches!(
                request.native_command(),
                Some(D::Scan { .. } | D::ScanNarrow { .. } | D::ScanResults { .. })
            )
        });
        let chain = self
            .panel_history(target, Panel::Memory)
            .find(|request| matches!(request.native_command(), Some(D::PointerChain { .. })));
        let running = self
            .panel_history(target, Panel::Memory)
            .any(|request| matches!(request.status, DebugStatus::Running));
        let editable = self.control
            && target
                .resolve(&self.sessions)
                .is_some_and(|summary| !summary.telemetry.desktop_debug_lease_blocked);
        let theme = theme::palette(cx);
        let action = |cx: &mut Context<Self>,
                      id,
                      label,
                      command: Result<D, String>,
                      generation,
                      allowed: bool| {
            let target = target.clone();
            let enabled = command.is_ok() && !running && allowed;
            small_button(theme, id, label, Tone::Plain, enabled).on_click(cx.listener(
                move |view, _, _, cx| {
                    if enabled
                        && view.inspection_panel == Panel::Memory
                        && view.inspection_target().as_ref() == Some(&target)
                    {
                        if let Ok(command) = &command {
                            if let Some(generation) = generation {
                                view.send(Command::ScanAt {
                                    target: target.clone(),
                                    command: command.clone(),
                                    generation,
                                });
                            } else {
                                view.debug_command(
                                    &Some(target.clone()),
                                    Panel::Memory,
                                    command.clone(),
                                );
                            }
                            cx.notify();
                        }
                    }
                },
            ))
        };
        let initial = form.scan(cx);
        let pointer = form.chain(cx);
        let mut body = div()
            .id("memory-search")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_1()
            .overflow_y_scroll()
            .child("Search little-endian values in a bounded guest memory snapshot.")
            .children([
                form.address.render("Scan address", cx),
                form.length.render("Length (up to 1 MiB)", cx),
                form.width.render("Width (bytes)", cx),
                form.value.render("Value / range minimum", cx),
            ])
            .children(initial.as_ref().err().cloned())
            .child(action(
                cx,
                "memory-new-scan",
                "New scan",
                initial,
                None,
                editable,
            ));
        if let Some(request) = scan {
            if let Some(data) = Self::reply_data(request) {
                if let (Some(generation), Some(total), Some(width)) = (
                    data["scan_generation"].as_u64(),
                    data["total"].as_u64(),
                    data["width"].as_u64(),
                ) {
                    let offset = match request.native_command() {
                        Some(D::ScanResults { offset, .. }) => *offset,
                        _ => 0,
                    };
                    body = body
                        .child(format!(
                            "Scan {generation} · {total} matches · page {} · {}",
                            offset / PAGE + 1,
                            if data["paused"] == true {
                                "Paused safe point"
                            } else {
                                "Live safe-point sample"
                            }
                        ))
                        .child(form.upper.render("Range maximum", cx));
                    let mut filters = div().flex().flex_wrap().gap_1();
                    for (id, label, mode) in [
                        ("scan-exact", "Exact", ScanMode::Exact),
                        ("scan-changed", "Changed", ScanMode::Changed),
                        ("scan-unchanged", "Unchanged", ScanMode::Unchanged),
                        ("scan-range", "Range", ScanMode::Range),
                    ] {
                        let command = form.narrow(mode, width as u32, cx);
                        filters = filters.child(action(
                            cx,
                            id,
                            label,
                            command,
                            Some(generation),
                            editable,
                        ));
                    }
                    body = body
                        .child(filters)
                        .children(form.narrow(ScanMode::Range, width as u32, cx).err());
                    for (id, label, next) in [
                        ("scan-previous", "Previous", offset.checked_sub(PAGE)),
                        (
                            "scan-next",
                            "Next",
                            offset.checked_add(PAGE).filter(|n| u64::from(*n) < total),
                        ),
                    ] {
                        body = body.child(action(
                            cx,
                            id,
                            label,
                            next.map(|offset| D::ScanResults {
                                offset,
                                limit: PAGE,
                            })
                            .ok_or_else(String::new),
                            Some(generation),
                            true,
                        ));
                    }
                    if let Some(matches) = data["matches"].as_array() {
                        let matches = matches
                            .iter()
                            .filter_map(|row| {
                                Some((
                                    u32::try_from(row["address"].as_u64()?).ok()?,
                                    row["value"].as_u64()?,
                                ))
                            })
                            .collect::<Vec<_>>();
                        let owner = target.clone();
                        let weak = cx.entity().downgrade();
                        body = body.child(
                            silvestris::controls::table::virtual_table(
                                "scan-matches",
                                matches.len(),
                                move |range, _, cx| {
                                    range
                                        .map(|index| {
                                            let (address, value) = matches[index];
                                            let (memory, watch) = (owner.clone(), owner.clone());
                                            let (weak_memory, weak_watch) =
                                                (weak.clone(), weak.clone());
                                            silvestris::controls::table::code_row(
                                                ("scan-match", index),
                                                30.0,
                                                cx,
                                            )
                                            .child(
                                                div().child(format!(
                                                    "0x{address:08x} · 0x{value:x}"
                                                )),
                                            )
                                            .child(
                                                small_button(
                                                    theme,
                                                    format!("scan-open-{index}"),
                                                    "Open",
                                                    Tone::Plain,
                                                    true,
                                                )
                                                .on_click(move |_, _, cx| {
                                                    let _ = weak_memory.update(cx, |view, cx| {
                                                        view.jump_to_memory(&memory, Some(address));
                                                        cx.notify();
                                                    });
                                                }),
                                            )
                                            .child(
                                                small_button(
                                                    theme,
                                                    format!("scan-watch-{index}"),
                                                    "Watch",
                                                    Tone::Plain,
                                                    true,
                                                )
                                                .on_click(move |_, _, cx| {
                                                    let _ = weak_watch.update(cx, |view, cx| {
                                                        view.jump_to_watchpoint(&watch, address);
                                                        cx.notify();
                                                    });
                                                }),
                                            )
                                        })
                                        .collect()
                                },
                            )
                            .h(px((matches_len(data).clamp(1, 8) * 30) as f32))
                            .flex_none(),
                        );
                    }
                }
            } else if let DebugStatus::Failed { message, .. } = &request.status {
                body = body.child(message.clone());
            }
        }
        body = body
            .child(form.base.render("Pointer address", cx))
            .child(
                form.offsets
                    .render("Signed offsets (space separated, up to 32)", cx),
            )
            .children(pointer.as_ref().err().cloned())
            .child(action(
                cx,
                "pointer-resolve",
                "Resolve pointer chain",
                pointer,
                None,
                true,
            ));
        if let Some(chain) = chain {
            if let Some(data) = Self::reply_data(chain) {
                if let Some(hops) = data["hops"].as_array() {
                    for hop in hops.iter().take(32) {
                        body = body.child(format!(
                            "[{}] = {} · offset {}",
                            hex_string(&hop["address"]),
                            hex_string(&hop["pointer"]),
                            hop["offset"]
                        ));
                    }
                }
                body = body.child(format!("Resolved address {}", hex_string(&data["address"])));
            } else if let DebugStatus::Failed { message, .. } = &chain.status {
                body = body.child(format!("Chain failed · {message}"));
            }
        }
        div().flex().flex_col().flex_1().min_h_0().child(body)
    }
}

fn matches_len(data: &Value) -> usize {
    data["matches"].as_array().map_or(0, Vec::len)
}
