use super::*;
use breakpoints::Input;
use lotus_protocol::{capabilities::feature, debug::EventCategory};
use silvestris::controls::{ControlSize, PopUpButton};

const CATEGORIES: [(&str, &str, EventCategory); 6] = [
    ("stopped", "Stops", EventCategory::Stopped),
    ("log", "Guest logs", EventCategory::Log),
    ("native_log", "Native logs", EventCategory::NativeLog),
    ("svc", "SVC", EventCategory::Svc),
    ("ipc", "IPC", EventCategory::Ipc),
    ("pica", "PICA", EventCategory::Pica),
];

fn category(kind: &str) -> &str {
    match kind {
        "guest_log" | "hio_stdout" | "logpoint" => "log",
        "svc_enter" | "svc_exit" => "svc",
        "stopped" | "stop" => "stopped",
        other => other,
    }
}

pub(super) struct TimelineForm {
    query: Input,
    filter: usize,
    selected: Option<u64>,
    capture: bool,
}

impl LotusWindow {
    pub(super) fn render_event_timeline(
        &self,
        target: &SessionTarget,
        panel: Panel,
        snapshot: Option<&InspectionSnapshot>,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = theme::palette(cx);
        let owner = target.clone();
        let key = (target.clone(), panel);
        let form = self.timeline_forms.get(&key);
        let mut body = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_1()
            .p_2()
            .text_xs()
            .child(format!("{} · epoch {}", target.id, target.epoch))
            .child(
                small_button(
                    theme,
                    "event-filters",
                    "Search and capture controls",
                    Tone::Plain,
                    true,
                )
                .on_click(cx.listener(move |view, _, window, cx| {
                    if view.inspection_panel != panel
                        || view.inspection_target().as_ref() != Some(&owner)
                    {
                        return;
                    }
                    let form = view
                        .timeline_forms
                        .entry((owner.clone(), panel))
                        .or_insert_with(|| TimelineForm {
                            query: Input::new(window, cx, "Search this page", String::new()),
                            filter: 0,
                            selected: None,
                            capture: false,
                        });
                    form.capture = !form.capture;
                    cx.notify();
                })),
            );
        let Some(snapshot) = snapshot else {
            return body.child("Loading events…");
        };
        if let Some((_, error)) = &snapshot.view.error {
            return body.child(error.clone());
        }
        let Some(data) = snapshot.view.data.as_ref() else {
            return body;
        };
        let offset = snapshot.offset.unwrap_or(0);
        let next = snapshot.view.next;
        let latest = data["latest"].as_u64().unwrap_or(0);
        let mut pages = div().flex().flex_wrap().gap_1();
        for (id, label, destination, enabled) in [
            ("event-previous", "Previous", None, offset > 0),
            (
                "event-next",
                "Next",
                next,
                next.is_some_and(|n| n > offset && n < latest),
            ),
            (
                "event-latest",
                "Latest page",
                Some(latest.saturating_sub(64)),
                latest.saturating_sub(64) > offset,
            ),
        ] {
            let owner = target.clone();
            pages = pages.child(
                small_button(theme, id, label, Tone::Plain, enabled).on_click(cx.listener(
                    move |view, _, _, cx| {
                        if enabled {
                            view.change_inspection_page(&owner, panel, offset, destination);
                            cx.notify();
                        }
                    },
                )),
            );
        }
        body = body.child(pages).child(format!(
            "After #{offset} · latest #{latest} · {} history evictions · {} pending drops",
            data["lost_total"], data["pending_lost_total"]
        ));
        if data["history_gap"] == true {
            body = body.child("The worker reports a history gap before this page.");
        }
        let Some(summary) = target.resolve(&self.sessions) else {
            return body;
        };
        if let Some(form) = form {
            body = body.child(form.query.render("Search raw fields on this page", cx));
            if panel == Panel::Timeline {
                let categories = [
                    "All",
                    "Stops",
                    "Guest logs",
                    "Native logs",
                    "SVC",
                    "IPC",
                    "PICA",
                    "Other",
                ];
                let owner = target.clone();
                body = body.child(
                    PopUpButton::new(
                        "event-category",
                        "Event category",
                        categories.map(SharedString::from).to_vec(),
                    )
                    .selected(Some(form.filter))
                    .size(ControlSize::Small)
                    .on_choose(cx.listener(
                        move |view, index: &usize, _, cx| {
                            if let Some(form) = view.timeline_forms.get_mut(&(owner.clone(), panel))
                            {
                                form.filter = *index;
                                cx.notify();
                            }
                        },
                    )),
                );
            }
            if form.capture {
                let control = self.inspector_control(target, panel);
                let paused =
                    !summary.telemetry.playing && summary.telemetry.state == SessionState::Paused;
                body = body.child(
                    self.render_event_capture(
                        target,
                        panel,
                        data,
                        (
                            control,
                            paused,
                            summary
                                .telemetry
                                .worker_features
                                .contains(feature::EVENT_SUBSCRIPTIONS),
                        ),
                        cx,
                    ),
                );
            }
        }
        let events = data["events"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let page_gaps = events
            .windows(2)
            .filter(|pair| {
                pair[0]["sequence"]
                    .as_u64()
                    .zip(pair[1]["sequence"].as_u64())
                    .is_some_and(|(a, b)| b > a.saturating_add(1))
            })
            .count();
        if page_gaps > 0 {
            body = body.child(format!("{page_gaps} sequence gaps in this page"));
        }
        if let Some(raw) = form.and_then(|form| form.selected).and_then(|sequence| {
            events
                .iter()
                .find(|event| event["sequence"].as_u64() == Some(sequence))
        }) {
            body = body.child(
                div()
                    .id("event-raw")
                    .max_h(px(130.0))
                    .overflow_y_scroll()
                    .font_family(theme.mono)
                    .child(inspection::json_preview(raw, 48_000)),
            );
        }
        let filter = if panel == Panel::Ipc {
            5
        } else {
            form.map_or(0, |form| form.filter)
        };
        let search = form
            .map_or("", |form| form.query.text(cx))
            .trim()
            .to_lowercase();
        let rows = events
            .iter()
            .filter_map(|event| {
                let kind = event["kind"].as_str().unwrap_or("unknown");
                let group = category(kind);
                let matched = match filter {
                    0 => true,
                    1..=6 => CATEGORIES[filter - 1].0 == group,
                    _ => !CATEGORIES.iter().any(|(key, _, _)| *key == group),
                };
                if !matched
                    || (!search.is_empty()
                        && inspection::json_preview(event, 48_000)
                            .to_lowercase()
                            .contains(&search))
                {
                    return None;
                }
                let context = event["frame"].as_u64().map_or_else(
                    || format!("tick {}", event["guest_ticks"]),
                    |frame| format!("frame {frame} · tick {}", event["guest_ticks"]),
                );
                Some((
                    event["sequence"].as_u64()?,
                    format!("#{} · {} · {context}", event["sequence"], kind),
                ))
            })
            .collect::<Vec<_>>();
        if rows.is_empty() {
            return body.child("No events match this page.");
        }
        let weak = cx.entity().downgrade();
        let owner = target.clone();
        body.child(
            silvestris::controls::table::virtual_table(
                "event-page",
                rows.len(),
                move |range, _, cx| {
                    range
                        .map(|index| {
                            let (seq, text) = &rows[index];
                            let (seq, owner, weak) = (*seq, owner.clone(), weak.clone());
                            silvestris::controls::table::code_row(("event", index), 26.0, cx)
                                .cursor_pointer()
                                .hover(|row| row.bg(theme.surface_hover))
                                .child(text.clone())
                                .on_click(move |_, window, cx| {
                                    let _ = weak.update(cx, |view, cx| {
                                        if view.inspection_target().as_ref() == Some(&owner)
                                            && view.inspection_panel == panel
                                        {
                                            let form = view
                                                .timeline_forms
                                                .entry((owner.clone(), panel))
                                                .or_insert_with(|| TimelineForm {
                                                    query: Input::new(
                                                        window,
                                                        cx,
                                                        "Search this page",
                                                        String::new(),
                                                    ),
                                                    filter: 0,
                                                    selected: None,
                                                    capture: false,
                                                });
                                            form.selected =
                                                (form.selected != Some(seq)).then_some(seq);
                                            cx.notify();
                                        }
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

    fn render_event_capture(
        &self,
        target: &SessionTarget,
        panel: Panel,
        data: &Value,
        (control, paused, subscriptions): (bool, bool, bool),
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = theme::palette(cx);
        let mut controls = div()
            .id("event-capture")
            .max_h(px(200.0))
            .flex_none()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1();
        let status = &data["subscriptions"];
        if subscriptions {
            for (field, label) in [
                ("history", "Worker history"),
                ("notify", "Worker notifications"),
            ] {
                let selected = CATEGORIES
                    .iter()
                    .filter(|(key, _, _)| {
                        status[field]
                            .as_array()
                            .is_some_and(|items| items.iter().any(|item| item == key))
                    })
                    .map(|(_, _, category)| *category)
                    .collect::<Vec<_>>();
                let mut row = div().flex().flex_wrap().gap_1();
                for (key, name, category) in CATEGORIES {
                    let enabled = selected.contains(&category);
                    let mut changed = selected.clone();
                    if enabled {
                        changed.retain(|item| *item != category);
                    } else {
                        changed.push(category);
                    }
                    let command = if field == "notify" {
                        D::EventsConfigure {
                            notify: Some(changed),
                            history: None,
                        }
                    } else {
                        D::EventsConfigure {
                            notify: None,
                            history: Some(changed),
                        }
                    };
                    let owner = target.clone();
                    let allowed =
                        control && category != EventCategory::Stopped && status[field].is_array();
                    row = row.child(
                        small_button(
                            theme,
                            format!("event-{field}-{key}"),
                            name,
                            if enabled { Tone::Primary } else { Tone::Plain },
                            allowed,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            if allowed
                                && view.inspection_panel == panel
                                && view.inspection_target().as_ref() == Some(&owner)
                            {
                                view.debug_command(&Some(owner.clone()), panel, command.clone());
                                cx.notify();
                            }
                        })),
                    );
                }
                controls = controls.child(label).child(row);
            }
            controls = controls.child("MCP delivery subscriptions are managed by each client. PICA events also require GPU capture.");
        } else {
            controls = controls.child("The worker does not advertise event subscriptions.");
        }
        for (id, label, enabled) in [
            ("event-trace-on", "Enable SVC and IPC trace", true),
            ("event-trace-off", "Disable SVC and IPC trace", false),
        ] {
            controls = controls.child(Self::inspector_action(
                target,
                panel,
                id,
                label,
                D::Trace { enabled },
                control,
                cx,
            ));
        }
        if panel == Panel::Timeline {
            let trace = &data["citrace"];
            if let Some(error) = trace["error"].as_str() {
                controls = controls.child(format!("CiTrace · {error}"));
            }
            let supported = trace["recording"].is_boolean();
            let recording = trace["recording"] == true;
            if supported {
                controls = controls
                    .child(format!(
                        "CiTrace · {} · {} frames · {} elements · {} bytes · {} dropped",
                        if recording {
                            "Recording"
                        } else if trace["gpu_replay_view"] == true {
                            "Replay view"
                        } else {
                            "Idle"
                        },
                        trace["frames"],
                        trace["elements"],
                        trace["data_bytes"],
                        trace["dropped"]
                    ))
                    .child(format!(
                        "Worker pin {} · profile {}",
                        trace["identity"]["source"], trace["profile_hash"]
                    ))
                    .child("CiTrace scanning is opt-in and can slow emulation.");
                if trace["truncated"] == true {
                    controls = controls.child("The recording is truncated.");
                }
            }
            for (id, label, command, permitted) in [
                (
                    "citrace-start",
                    "Record CiTrace",
                    D::CitraceStart,
                    paused && !recording,
                ),
                ("citrace-stop", "Finish CiTrace", D::CitraceStop, recording),
                (
                    "citrace-replay",
                    "Replay last CiTrace",
                    D::CitraceReplay,
                    paused && !recording,
                ),
            ] {
                controls = controls.child(Self::inspector_action(
                    target,
                    panel,
                    id,
                    label,
                    command,
                    supported && control && permitted,
                    cx,
                ));
            }
            if let Some(artifact) = self
                .panel_history(target, panel)
                .find(|request| matches!(request.native_command(), Some(D::CitraceStop)))
                .and_then(Self::reply_data)
                .map(|reply| &reply["artifacts"][0])
                .filter(|artifact| artifact["uri"].is_string())
            {
                let uri = artifact["uri"].as_str().unwrap_or_default().to_owned();
                let export_uri = uri.clone();
                let owner = target.clone();
                controls = controls
                    .child(format!(
                        "Captured artifact · SHA-256 {}",
                        artifact["sha256"]
                    ))
                    .child(
                        small_button(theme, "citrace-export", "Save trace as…", Tone::Plain, true)
                            .on_click(cx.listener(move |_, _, _, cx| {
                                let path = cx.prompt_for_new_path(
                                    &std::path::PathBuf::new(),
                                    Some("lotus-trace.ctf"),
                                );
                                let (target, uri) = (owner.clone(), export_uri.clone());
                                cx.spawn(async move |this, cx| {
                                    if let Ok(Ok(Some(path))) = path.await {
                                        let _ = this.update(cx, |view, _| {
                                            view.send(Command::ExportTrace { target, uri, path });
                                        });
                                    }
                                })
                                .detach();
                            })),
                    )
                    .child(
                        small_button(
                            theme,
                            "citrace-copy",
                            "Copy artifact URI",
                            Tone::Plain,
                            true,
                        )
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(uri.clone()));
                        })),
                    );
            }
            controls = controls.child(
                small_button(
                    theme,
                    "events-import",
                    "Import event JSONL",
                    Tone::Plain,
                    true,
                )
                .on_click(cx.listener(|view, _, _, cx| view.open_trace(cx))),
            );
        }
        controls
    }
}
