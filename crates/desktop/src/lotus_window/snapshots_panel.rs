use super::*;
use breakpoints::Input;
use gpui::App;
use lotus_protocol::capabilities::feature;

pub(super) struct SnapshotForm {
    capacity: Input,
    budget: Input,
    interval: Input,
    level: Input,
    queue: Input,
    core: Input,
    checkpoint: Input,
    movie: Input,
}

impl SnapshotForm {
    fn new(window: &mut Window, cx: &mut Context<LotusWindow>, status: &Value) -> Self {
        let value = |key: &str, default| status[key].as_i64().unwrap_or(default).to_string();
        let budget = status["budget_bytes"].as_u64().unwrap_or(128 << 20) >> 20;
        let mut field = |label, text| Input::new(window, cx, label, text);
        Self {
            capacity: field("Capacity", value("capacity", 64)),
            budget: field("Budget", budget.max(16).to_string()),
            interval: field("Checkpoint interval", value("interval_frames", 1)),
            level: field("Compression level", value("compression_level", 3)),
            queue: field("Compression queue", value("compression_queue", 1)),
            core: field("Core", "0".into()),
            checkpoint: field("Checkpoint name", String::new()),
            movie: field("Movie name", String::new()),
        }
    }

    fn configure(&self, enabled: bool, queue: bool, cx: &App) -> Result<D, String> {
        let compression_level =
            crate::debugger_palette::parse_integer(self.level.text(cx).trim(), true)
                .ok()
                .and_then(|value| value.as_i64())
                .and_then(|level| i8::try_from(level).ok())
                .ok_or("Compression level requires a signed integer")?;
        let command = D::RewindConfigure {
            enabled,
            capacity: self.capacity.number("Capacity", cx)?,
            budget_mib: self.budget.number("Budget", cx)?,
            interval_frames: Some(self.interval.number("Interval", cx)?),
            compression_level: Some(compression_level),
            compression_queue: queue
                .then(|| self.queue.number("Compression queue", cx))
                .transpose()?,
        };
        command.validate().map_err(str::to_owned)?;
        Ok(command)
    }
}

impl LotusWindow {
    pub(super) fn inspector_action(
        target: &SessionTarget,
        panel: Panel,
        id: &'static str,
        label: &'static str,
        command: D,
        permitted: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let target = target.clone();
        let enabled = permitted && command.validate().is_ok();
        small_button(theme::palette(cx), id, label, Tone::Plain, enabled).on_click(cx.listener(
            move |view, _, window, cx| {
                if enabled
                    && view.inspection_panel == panel
                    && view.inspection_target().as_ref() == Some(&target)
                {
                    view.dispatch_inspector_command(&target, panel, command.clone(), window, cx);
                    cx.notify();
                }
            },
        ))
    }

    pub(super) fn inspector_control(&self, target: &SessionTarget, panel: Panel) -> bool {
        self.control
            && target
                .resolve(&self.sessions)
                .is_some_and(|summary| !summary.telemetry.desktop_debug_lease_blocked)
            && !self
                .panel_history(target, panel)
                .any(|request| matches!(request.status, DebugStatus::Running))
    }

    pub(super) fn render_snapshots_panel(
        &self,
        target: &SessionTarget,
        snapshot: Option<&InspectionSnapshot>,
        cx: &mut Context<Self>,
    ) -> Div {
        let Some(summary) = target.resolve(&self.sessions) else {
            return div().child("Guest changed");
        };
        let paused = !summary.telemetry.playing && summary.telemetry.state == SessionState::Paused;
        let idle = summary.telemetry.state == SessionState::Idle;
        let data = snapshot.and_then(|snapshot| snapshot.view.data.as_deref());
        let rewind = data.map(|data| &data["rewind"]).unwrap_or(&Value::Null);
        let movie = data.map(|data| &data["movie"]).unwrap_or(&Value::Null);
        let control = self.inspector_control(target, Panel::States);
        let queue = summary
            .telemetry
            .worker_features
            .contains(feature::REWIND_COMPRESSION_QUEUE);
        let palette = theme::palette(cx);
        let mut body = div()
            .id("snapshots-scroll")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_2()
            .p_2()
            .text_xs()
            .overflow_y_scroll()
            .child(format!("{} · epoch {}", target.id, target.epoch));
        if let Some((_, message)) = snapshot.and_then(|snapshot| snapshot.view.error.as_ref()) {
            body = body.child(message.clone());
        }
        if let Some(error) = rewind["error"].as_str() {
            body = body.child(format!("Rewind · {error}"));
        }
        let rewind_supported = rewind["enabled"].is_boolean();
        if rewind_supported {
            body = body.child(format!(
                "Rewind {} · {} of {} checkpoints · {} lost · {} bytes stored, {} awaiting compression",
                if rewind["enabled"] == true { "enabled" } else { "disabled" },
                rewind["snapshots"], rewind["capacity"], rewind["lost_snapshots"],
                rewind["bytes"], rewind["compression_pending_bytes"]
            )).child(format!(
                "Worker settings · {} MiB · every {} frames · zstd {} · queue {}",
                rewind["budget_bytes"].as_u64().unwrap_or(0) >> 20, rewind["interval_frames"],
                rewind["compression_level"], rewind["compression_queue"]
            ));
        }
        if let Some(policy) = data.map(|data| &data["rewind_policy"]) {
            body = body.child(format!(
                "Host rewind limit · {}",
                policy["memory_limit_mib"]
                    .as_u64()
                    .map_or_else(|| "unlimited".into(), |cap| format!("{cap} MiB"))
            ));
            if let Some(owners) = policy["memory_reserved_mib"].as_object() {
                for (owner, budget) in owners {
                    body = body.child(format!("{owner} · {budget} MiB reserved"));
                }
            }
        }
        if !summary.telemetry.policy_note.is_empty() {
            body = body.child(summary.telemetry.policy_note.clone());
        }
        if let Some(error) = movie["error"].as_str() {
            body = body.child(format!("Movie · {error}"));
        } else if let Some(mode) = movie["mode"].as_u64() {
            let mode = match mode {
                1 => "Recording",
                2 => "Playing",
                3 => "Finished",
                _ => "Inactive",
            };
            body = body.child(format!(
                "Movie {mode} · {} · {}/{} inputs · {} dropped",
                movie["name"], movie["input_index"], movie["total_inputs"], movie["dropped_inputs"]
            ));
            if movie["prepared"]
                .as_str()
                .is_some_and(|plan| !plan.is_empty())
                && idle
            {
                body = body.child(format!(
                    "Prepared to {}. Open the guest content in this session.",
                    movie["prepared"]
                ));
            }
        }
        let Some(form) = self.snapshot_forms.get(target) else {
            let owner = target.clone();
            let status = rewind.clone();
            return div().flex().flex_col().flex_1().min_h_0().child(
                body.child(
                    small_button(
                        palette,
                        "snapshots-edit",
                        "Open history controls",
                        Tone::Plain,
                        data.is_some(),
                    )
                    .on_click(cx.listener(move |view, _, window, cx| {
                        if view.inspection_target().as_ref() == Some(&owner) {
                            view.snapshot_forms
                                .insert(owner.clone(), SnapshotForm::new(window, cx, &status));
                            cx.notify();
                        }
                    })),
                ),
            );
        };
        if rewind_supported {
            body = body.children([
                form.capacity.render("Capacity (1–128)", cx),
                form.budget.render("Worker budget (16–256 MiB)", cx),
                form.interval.render("Interval (1–60 frames)", cx),
                form.level.render("Compression level (−7 to 19)", cx),
            ]);
            if queue {
                body = body.child(form.queue.render("Compression queue (0–4)", cx))
                    .child("Each queued checkpoint holds serialized data in RAM. Zero compresses on the emulation thread.");
            } else {
                body = body.child("This worker does not advertise compression queue control.");
            }
            for (id, label, enabled) in [
                ("rewind-configure", "Enable or update rewind", true),
                ("rewind-disable", "Disable rewind", false),
            ] {
                match form.configure(enabled, queue, cx) {
                    Ok(command) => {
                        body = body.child(Self::inspector_action(
                            target,
                            Panel::States,
                            id,
                            label,
                            command,
                            control,
                            cx,
                        ))
                    }
                    Err(error) if enabled => body = body.child(error),
                    Err(_) => {}
                }
            }
            let available = rewind["enabled"] == true
                && rewind["snapshots"].as_u64().is_some_and(|depth| depth > 0);
            body = body.child(form.core.render("ARM11 core (0–3)", cx));
            match form.core.number("Core", cx) {
                Ok(core) => {
                    body = body.child(Self::inspector_action(
                        target,
                        Panel::States,
                        "rewind-step",
                        "Reverse step",
                        D::ReverseStep { core },
                        available && control && paused,
                        cx,
                    ))
                }
                Err(error) => body = body.child(error),
            }
            body = body.child(Self::inspector_action(
                target,
                Panel::States,
                "rewind-continue",
                "Reverse continue",
                D::ReverseContinue,
                available && control && paused,
                cx,
            ));
        }
        body = body.child(form.checkpoint.render("Checkpoint name", cx));
        let command = D::SaveState {
            name: form.checkpoint.text(cx).trim().into(),
        };
        body = body
            .children(command.validate().err())
            .child(Self::inspector_action(
                target,
                Panel::States,
                "checkpoint-save",
                "Save state",
                command,
                control && paused,
                cx,
            ));
        if let Some(checkpoints) = data.and_then(|data| data["checkpoints"]["states"].as_array()) {
            let mut names = checkpoints
                .iter()
                .filter_map(Value::as_str)
                .take(16)
                .collect::<Vec<_>>();
            names.sort_unstable();
            if names.is_empty() {
                body = body.child("No saved checkpoints.");
            }
            for (index, name) in names.into_iter().enumerate() {
                let owner = target.clone();
                let name = name.to_owned();
                body = body.child(
                    div().flex().justify_between().child(name.clone()).child(
                        small_button(
                            palette,
                            format!("checkpoint-restore-{index}"),
                            "Restore",
                            Tone::Plain,
                            control && paused,
                        )
                        .on_click(cx.listener(
                            move |view, _, window, cx| {
                                if control && paused && view.inspection_panel == Panel::States {
                                    view.dispatch_inspector_command(
                                        &owner,
                                        Panel::States,
                                        D::LoadState { name: name.clone() },
                                        window,
                                        cx,
                                    );
                                }
                            },
                        )),
                    ),
                );
            }
        } else if let Some(message) = data.and_then(|data| data["checkpoints"]["error"].as_str()) {
            body = body.child(message.to_owned());
        } else if !paused {
            body = body.child("Pause the guest to browse or restore checkpoints.");
        }
        body = body.child(form.movie.render("Movie name", cx))
            .child("Prepare a movie before opening guest content. To replay a saved movie, reopen its primary card without content and prepare read-only playback.")
            .child("Repeatable playback requires the recorded content, worker pin and deterministic profile.");
        let name = form.movie.text(cx).trim().to_owned();
        let record = D::MovieRecord { name: name.clone() };
        let play = D::MoviePlay {
            name,
            read_only: true,
        };
        body = body.children(record.validate().err());
        for (id, label, command, allowed) in [
            ("movie-record", "Prepare recording", record, idle),
            ("movie-play", "Prepare read-only replay", play, idle),
            (
                "movie-stop",
                "Stop movie",
                D::MovieStop,
                movie["mode"].as_u64().is_some_and(|mode| mode > 0)
                    || movie["prepared"]
                        .as_str()
                        .is_some_and(|plan| !plan.is_empty()),
            ),
        ] {
            body = body.child(Self::inspector_action(
                target,
                Panel::States,
                id,
                label,
                command,
                control && allowed && movie["error"].is_null(),
                cx,
            ));
        }
        div().flex().flex_col().flex_1().min_h_0().child(body)
    }
}
