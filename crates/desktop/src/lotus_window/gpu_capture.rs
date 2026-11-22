use super::*;
use crate::{desktop_controller::Update, inspection::GpuActivity};
use gpui::{Entity, Subscription};
use lotus_protocol::capabilities::feature::GPU_CAPTURE_LEVELS;
use silvestris::controls::{CheckState, Checkbox, ControlSize, PopUpButton, TextField};

const MODES: [Mode; 5] = [
    Mode(Some(GpuCapture::Counters)),
    Mode(Some(GpuCapture::Events)),
    Mode(Some(GpuCapture::Draws)),
    Mode(Some(GpuCapture::Full)),
    Mode(None),
];

// Pica::DebugContext::Event order in video_core/debug_utils/debug_utils.h.
const EVENTS: [&str; 8] = [
    "PICA command loaded",
    "PICA command processed",
    "Primitive batch begins",
    "Primitive batch ends",
    "Vertex shader invocation",
    "Display transfer begins",
    "GSP command processed",
    "Buffer swapped",
];

#[derive(Clone, Copy, PartialEq, Eq)]
struct Mode(Option<GpuCapture>);

impl Mode {
    fn label(self) -> &'static str {
        match self.0 {
            Some(GpuCapture::Counters) => "Counters",
            Some(GpuCapture::Events) => "Events",
            Some(GpuCapture::Draws) => "Draws",
            Some(GpuCapture::Full) => "Full",
            None => "Disabled",
        }
    }

    fn cost(self) -> &'static str {
        match self.0 {
            Some(GpuCapture::Counters) => "Collects counters on the batched PICA path.",
            Some(GpuCapture::Events) => "Per-command events and draw metadata add CPU work.",
            Some(GpuCapture::Draws) => "Snapshots register and shader state. Copying resources adds memory and readback cost.",
            Some(GpuCapture::Full) => "Snapshots every draw with resources. This has the highest CPU, memory and readback cost.",
            None => "Restores the batched PICA path.",
        }
    }

    fn takes_events(self) -> bool {
        self.0
            .is_some_and(|capture| capture != GpuCapture::Counters)
    }

    fn command(
        self,
        first: &str,
        last: &str,
        budget: &str,
        resources: bool,
        events: &BTreeSet<u32>,
    ) -> Result<D, &'static str> {
        let draws = self.0 == Some(GpuCapture::Draws);
        let draw_range = if draws && !(first.trim().is_empty() && last.trim().is_empty()) {
            let parse_id = |text: &str| {
                text.trim()
                    .parse::<u64>()
                    .map_err(|_| "Enter both draw IDs as unsigned 64-bit integers")
            };
            Some([parse_id(first)?, parse_id(last)?])
        } else {
            None
        };
        let max_draws = if draws && !budget.trim().is_empty() {
            Some(
                budget
                    .trim()
                    .parse::<u32>()
                    .map_err(|_| "Snapshot budget must be 1–4096")?,
            )
        } else {
            None
        };
        let command = D::GpuConfigure {
            enabled: self.0.is_some(),
            capture: Some(self.0.unwrap_or(GpuCapture::Counters)),
            draw_range,
            max_draws,
            resources: draws.then_some(resources),
            stop_events: if self.takes_events() {
                events.iter().copied().collect()
            } else {
                vec![]
            },
        };
        command.validate()?;
        Ok(command)
    }
}

#[derive(Default)]
pub(super) struct GpuPanel {
    form: Option<CaptureForm>,
    activity: Option<(SessionTarget, GpuActivity)>,
    request: Option<DebugRequest>,
}

struct CaptureForm {
    target: SessionTarget,
    mode: Mode,
    first: Entity<TextField>,
    last: Entity<TextField>,
    budget: Entity<TextField>,
    resources: bool,
    events: BTreeSet<u32>,
    _fields: [Subscription; 3],
}

impl GpuPanel {
    pub(super) fn refresh(&mut self, update: &Update) {
        if self
            .form
            .as_ref()
            .is_some_and(|form| !form.target.is_current(&update.inspected, &update.sessions))
        {
            self.form = None;
        }
        self.activity = update.inspection.as_ref().and_then(|snapshot| {
            let target = snapshot.target.as_ref()?;
            let activity = snapshot.view.gpu.as_ref()?;
            target
                .is_current(&update.inspected, &update.sessions)
                .then(|| (target.clone(), activity.clone()))
        });
        self.request = update
            .debug_requests()
            .rev()
            .find(|request| {
                matches!(request.native_command(), Some(D::GpuConfigure { .. }))
                    && request
                        .target
                        .is_current(&update.inspected, &update.sessions)
            })
            .cloned();
    }
}

fn describe(
    mode: Mode,
    range: Option<[u64; 2]>,
    budget: Option<u32>,
    resources: bool,
    events: &[u32],
) -> String {
    let mut text = mode.label().to_owned();
    if mode.0 == Some(GpuCapture::Draws) {
        let range = range.map_or("any draw".into(), |[first, last]| {
            format!("IDs {first}–{last}")
        });
        text.push_str(&format!(
            " · {range} · {} snapshots · resources {}",
            budget.unwrap_or(8),
            if resources { "on" } else { "off" }
        ));
    }
    if !events.is_empty() && mode.takes_events() {
        text.push_str(&format!(
            " · stop events {}",
            events
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    text
}

impl LotusWindow {
    fn gpu_block(&self, target: &SessionTarget) -> Option<&'static str> {
        let Some(summary) = target.resolve(&self.sessions) else {
            return Some("Session or guest changed");
        };
        if !summary.telemetry.state.is_active() {
            Some("The worker is inactive")
        } else if !summary
            .telemetry
            .worker_features
            .contains(GPU_CAPTURE_LEVELS)
        {
            Some("Unsupported · this worker does not advertise GPU capture levels")
        } else if !self.control {
            Some("Control is disabled for this host")
        } else if summary.telemetry.desktop_debug_lease_blocked {
            Some("A different client holds the debugger lease")
        } else if self.gpu_capture.request.as_ref().is_some_and(|request| {
            request.target == *target && matches!(request.status, DebugStatus::Running)
        }) {
            Some("Applying capture configuration…")
        } else {
            None
        }
    }

    fn apply_gpu(&mut self, target: &SessionTarget, command: D, cx: &mut Context<Self>) {
        if self.inspection_panel == Panel::Gpu
            && self.inspection_target().as_ref() == Some(target)
            && self.gpu_block(target).is_none()
        {
            self.gpu_capture.form = None;
            self.debug_command(&Some(target.clone()), Panel::Gpu, command);
            cx.notify();
        }
    }

    fn open_gpu_form(
        &mut self,
        target: &SessionTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.inspection_panel != Panel::Gpu
            || self.inspection_target().as_ref() != Some(target)
            || self.gpu_block(target).is_some()
        {
            return;
        }
        let accepted = target
            .resolve(&self.sessions)
            .and_then(|summary| summary.telemetry.gpu_capture.clone())
            .filter(|accepted| accepted.epoch == target.epoch);
        let first = command_palette::field(window, cx, "First draw ID", "Any", false);
        let last = command_palette::field(window, cx, "Last draw ID", "Any", false);
        let budget =
            command_palette::field(window, cx, "Snapshot budget", "8 (worker default)", false);
        if let Some(accepted) = &accepted {
            if let Some([low, high]) = accepted.draw_range {
                first.update(cx, |field, cx| field.set_text(low.to_string(), cx));
                last.update(cx, |field, cx| field.set_text(high.to_string(), cx));
            }
            if accepted.capture == GpuCapture::Draws {
                budget.update(cx, |field, cx| {
                    field.set_text(accepted.max_draws.to_string(), cx)
                });
            }
        }
        let fields =
            [&first, &last, &budget].map(|field| cx.subscribe(field, |_, _, _, cx| cx.notify()));
        self.gpu_capture.form = Some(CaptureForm {
            target: target.clone(),
            mode: accepted
                .as_ref()
                .map_or(Mode(Some(GpuCapture::Draws)), |ack| {
                    Mode(ack.enabled.then_some(ack.capture))
                }),
            first,
            last,
            budget,
            resources: accepted
                .as_ref()
                .is_some_and(|ack| ack.capture == GpuCapture::Draws && ack.resources),
            events: accepted.as_ref().map_or_else(BTreeSet::new, |ack| {
                ack.stop_events.iter().copied().collect()
            }),
            _fields: fields,
        });
        cx.notify();
    }

    pub(super) fn render_gpu_capture_form(
        &self,
        target: &SessionTarget,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let form = self
            .gpu_capture
            .form
            .as_ref()
            .filter(|form| &form.target == target)?;
        let palette = theme::palette(cx);
        let mode = form.mode;
        let blocked = self.gpu_block(target);
        let command = mode.command(
            form.first.read(cx).text(),
            form.last.read(cx).text(),
            form.budget.read(cx).text(),
            form.resources,
            &form.events,
        );
        let choose = target.clone();
        let mut body = div()
            .id("gpu-capture-form")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .text_sm()
            .child(
                PopUpButton::new(
                    "gpu-capture-mode",
                    "Capture level",
                    MODES.map(|mode| SharedString::from(mode.label())).to_vec(),
                )
                .selected(MODES.iter().position(|item| *item == mode))
                .size(ControlSize::Small)
                .on_choose(cx.listener(move |view, index: &usize, _, cx| {
                    if let Some(form) = view
                        .gpu_capture
                        .form
                        .as_mut()
                        .filter(|form| form.target == choose)
                    {
                        if let Some(mode) = MODES.get(*index) {
                            form.mode = *mode;
                            cx.notify();
                        }
                    }
                })),
            )
            .child(div().text_color(palette.text_secondary).child(mode.cost()));
        if mode.0 == Some(GpuCapture::Draws) {
            let owner = target.clone();
            body = body
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child("First draw ID")
                                .child(form.first.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child("Last draw ID")
                                .child(form.last.clone()),
                        ),
                )
                .child(
                    div()
                        .child("Snapshot budget (1–4096)")
                        .child(form.budget.clone()),
                )
                .child(
                    Checkbox::new("gpu-copy-resources", "Copy texture, index and vertex bytes")
                        .checked(form.resources)
                        .on_change(cx.listener(move |view, state: &CheckState, _, cx| {
                            if let Some(form) = view
                                .gpu_capture
                                .form
                                .as_mut()
                                .filter(|form| form.target == owner)
                            {
                                form.resources = *state == CheckState::On;
                                cx.notify();
                            }
                        })),
                );
        }
        if mode.takes_events() {
            body = body.child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Pause on PICA events"),
            );
            for (index, name) in EVENTS.into_iter().enumerate() {
                let id = index as u32;
                let owner = target.clone();
                body = body.child(
                    Checkbox::new(format!("gpu-event-{id}"), format!("{id} · {name}"))
                        .checked(form.events.contains(&id))
                        .on_change(cx.listener(move |view, state: &CheckState, _, cx| {
                            if let Some(form) = view
                                .gpu_capture
                                .form
                                .as_mut()
                                .filter(|form| form.target == owner)
                            {
                                if *state == CheckState::On {
                                    form.events.insert(id);
                                } else {
                                    form.events.remove(&id);
                                }
                                cx.notify();
                            }
                        })),
                );
            }
        }
        let error = blocked.or_else(|| command.as_ref().err().copied());
        let enabled = blocked.is_none() && command.is_ok();
        let owner = target.clone();
        Some(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(body)
                .child(
                    div()
                        .flex_none()
                        .px_3()
                        .py_2()
                        .border_t_1()
                        .border_color(palette.border)
                        .text_sm()
                        .children(error)
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    small_button(
                                        palette,
                                        "gpu-capture-apply",
                                        "Apply",
                                        Tone::Primary,
                                        enabled,
                                    )
                                    .on_click(cx.listener(
                                        move |view, _, _, cx| {
                                            if let Ok(command) = &command {
                                                view.apply_gpu(&owner, command.clone(), cx);
                                            }
                                        },
                                    )),
                                )
                                .child(
                                    small_button(
                                        palette,
                                        "gpu-capture-cancel",
                                        "Cancel",
                                        Tone::Plain,
                                        true,
                                    )
                                    .on_click(cx.listener(
                                        |view, _, _, cx| {
                                            view.gpu_capture.form = None;
                                            cx.notify();
                                        },
                                    )),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    pub(super) fn render_gpu_capture(
        &self,
        target: &SessionTarget,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = theme::palette(cx);
        let blocked = self.gpu_block(target);
        let configure = target.clone();
        let mut buttons = div().flex().flex_wrap().gap_1();
        for (index, mode) in MODES.into_iter().enumerate() {
            let owner = target.clone();
            let command = mode
                .command("", "", "", false, &BTreeSet::new())
                .expect("valid GPU preset");
            buttons = buttons.child(
                small_button(
                    palette,
                    format!("gpu-preset-{index}"),
                    mode.label(),
                    Tone::Plain,
                    blocked.is_none(),
                )
                .on_click(
                    cx.listener(move |view, _, _, cx| view.apply_gpu(&owner, command.clone(), cx)),
                ),
            );
        }
        let mut status = div()
            .flex()
            .flex_none()
            .flex_col()
            .gap_1()
            .p_2()
            .text_xs()
            .border_b_1()
            .border_color(palette.border)
            .child(
                buttons.child(
                    small_button(
                        palette,
                        "gpu-configure",
                        "Configure…",
                        Tone::Plain,
                        blocked.is_none(),
                    )
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.open_gpu_form(&configure, window, cx)
                    })),
                ),
            );
        if let Some(blocked) = blocked {
            status = status.child(blocked);
        }
        if let Some(request) = self
            .gpu_capture
            .request
            .as_ref()
            .filter(|request| &request.target == target)
        {
            if let Some(D::GpuConfigure {
                enabled,
                stop_events,
                capture,
                draw_range,
                max_draws,
                resources,
            }) = request.native_command()
            {
                let mode = Mode(enabled.then(|| capture.unwrap_or(GpuCapture::Full)));
                let outcome = match &request.status {
                    DebugStatus::Running => "pending",
                    DebugStatus::Finished { .. } => "acknowledged",
                    DebugStatus::Failed { .. } => "refused",
                    DebugStatus::Cancelled { .. } => "cancelled",
                    DebugStatus::Expired => "expired",
                };
                status = status.child(format!(
                    "Requested ({outcome}) · {}",
                    describe(
                        mode,
                        *draw_range,
                        *max_draws,
                        resources.unwrap_or(false),
                        stop_events
                    )
                ));
                if self
                    .debug_request
                    .as_ref()
                    .is_none_or(|current| current.id != request.id)
                {
                    if let DebugStatus::Failed { message, .. } = &request.status {
                        status = status.child(message.clone());
                    }
                }
            }
        }
        if let Some(ack) = target
            .resolve(&self.sessions)
            .and_then(|summary| summary.telemetry.gpu_capture.as_ref())
            .filter(|ack| ack.epoch == target.epoch)
        {
            status = status.child(format!(
                "Last accepted · {}",
                describe(
                    Mode(ack.enabled.then_some(ack.capture)),
                    ack.draw_range,
                    Some(ack.max_draws),
                    ack.resources,
                    &ack.stop_events
                )
            ));
        }
        if let Some((_, activity)) = self
            .gpu_capture
            .activity
            .as_ref()
            .filter(|(owner, _)| owner == target)
        {
            status = status.child(match activity.command_path.as_str() {
                "per_command" => format!(
                    "Current · {} on the per-command path",
                    Mode(Some(activity.capture)).label()
                ),
                "batched" => "Current · Batched PICA path".into(),
                _ => "Current · Worker path unknown".into(),
            });
            if let Some(counters) = &activity.counters {
                status = status.child(format!(
                    "{} lists · {} draws · {} fast-path commands",
                    counters.command_lists, counters.draw_calls, counters.fast_path_commands
                ));
            }
            let measured = [
                (activity.snapshots_taken, "snapshots"),
                (activity.snapshots_skipped_budget, "skipped by budget"),
                (activity.lost_draws, "draws lost"),
                (activity.lost_vertices, "vertices lost"),
                (activity.lost_resource_versions, "resource versions lost"),
            ]
            .into_iter()
            .filter_map(|(count, label)| count.map(|count| format!("{count} {label}")))
            .collect::<Vec<_>>();
            if !measured.is_empty() {
                status = status.child(measured.join(" · "));
            }
        }
        status.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Focusable, TestAppContext};
    use lotus_session::{supervisor::SessionInfo, telemetry::Telemetry};

    #[test]
    fn capture_form_sends_only_applicable_fields_and_validates_protocol_bounds() {
        let events = BTreeSet::from([0, 2, 7]);
        let draws = Mode(Some(GpuCapture::Draws));
        for (first, last, budget) in [
            ("1", "", ""),
            ("2", "1", ""),
            ("", "", "0"),
            ("", "", "4097"),
            ("0", "18446744073709551616", ""),
        ] {
            assert!(draws.command(first, last, budget, true, &events).is_err());
        }
        assert!(matches!(
            draws.command("0", "18446744073709551615", "4096", true, &events).unwrap(),
            D::GpuConfigure { enabled: true, capture: Some(GpuCapture::Draws), draw_range: Some([0, u64::MAX]),
                max_draws: Some(4096), resources: Some(true), stop_events } if stop_events == [0, 2, 7]
        ));
        for mode in [Mode(None), Mode(Some(GpuCapture::Counters))] {
            assert!(matches!(
                mode.command("9", "1", "0", true, &events).unwrap(),
                D::GpuConfigure { enabled, capture: Some(GpuCapture::Counters), draw_range: None,
                    max_draws: None, resources: None, stop_events } if enabled == mode.0.is_some() && stop_events.is_empty()
            ));
        }
    }

    #[test]
    fn mounted_capture_form_accepts_typed_draw_ids_and_discards_an_old_epoch() {
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
            telemetry.worker_features = Arc::new(BTreeSet::from([GPU_CAPTURE_LEVELS.into()]));
            view.sessions = Arc::new(vec![SessionSummary {
                info: SessionInfo {
                    id: "fixture".into(),
                    kind: SessionKind::Secondary { parent: None },
                    asset: Some("fixture.3dsx".into()),
                    created_unix_ms: 1,
                },
                telemetry,
            }]);
            view.selected = "fixture".into();
            view.inspected = "fixture".into();
            view.control = true;
            view.show_panel(Panel::Gpu);
            view
        });
        cx.simulate_resize(gpui::size(px(1400.), px(900.)));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let configure = cx.debug_bounds("gpu-configure").expect("configure button");
        cx.simulate_click(configure.center(), gpui::Modifiers::default());
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let form = view.read(cx).gpu_capture.form.as_ref().unwrap();
            window.focus(&form.first.read(cx).focus_handle(cx), cx);
            window.draw(cx).clear(cx);
        });
        cx.simulate_keystrokes("1 0");
        cx.update(|window, cx| {
            let form = view.read(cx).gpu_capture.form.as_ref().unwrap();
            assert_eq!(form.first.read(cx).text(), "10");
            window.focus(&form.last.read(cx).focus_handle(cx), cx);
            window.draw(cx).clear(cx);
        });
        cx.simulate_keystrokes("2 0");
        cx.update(|window, cx| {
            let mounted = view.read(cx);
            let form = mounted.gpu_capture.form.as_ref().unwrap();
            assert_eq!(form.last.read(cx).text(), "20");
            assert!(mounted.keys.is_empty());
            view.update(cx, |view, cx| {
                Arc::make_mut(&mut view.sessions)[0].telemetry.epoch += 1;
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
        assert!(cx.debug_bounds("gpu-capture-form").is_none());
    }
}
