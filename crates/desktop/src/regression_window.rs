use gpui::{div, prelude::*, px, size, App, Bounds, Context, Window, WindowHandle};
use lotus_runner::{
    assertions::AssertionResult,
    assessment::Status,
    history::{self, Comparison, Detail, Entry},
};
use silvestris::controls::Button;
use std::{io, path::PathBuf};

struct Opened(WindowHandle<RegressionWindow>);
impl gpui::Global for Opened {}

pub fn open(cx: &mut App, root: PathBuf) {
    if let Some(handle) = cx.try_global::<Opened>().map(|opened| opened.0) {
        if handle
            .update(cx, |view, window, cx| {
                view.refresh(cx);
                window.activate_window();
            })
            .is_ok()
        {
            return;
        }
    }
    let bounds = Bounds::centered(None, size(px(880.), px(570.)), cx);
    let options = silvestris::chrome::window_options(
        "Regression History",
        bounds,
        size(px(600.), px(390.)),
        cx,
    );
    match silvestris::chrome::open_window(options, cx, |_, cx| {
        cx.new(|cx| {
            let mut view = RegressionWindow {
                root,
                entries: Vec::new(),
                selected: None,
                baseline: None,
                comparison: None,
                busy: false,
                error: None,
            };
            view.refresh(cx);
            view
        })
    }) {
        Ok(handle) => cx.set_global(Opened(handle)),
        Err(error) => tracing::warn!(%error, "cannot open regression history"),
    }
}

pub fn close(cx: &mut App) {
    if let Some(handle) = cx.try_global::<Opened>().map(|opened| opened.0) {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }
}

struct RegressionWindow {
    root: PathBuf,
    entries: Vec<Entry>,
    selected: Option<Detail>,
    baseline: Option<String>,
    comparison: Option<Comparison>,
    busy: bool,
    error: Option<String>,
}

impl RegressionWindow {
    fn read<T: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        work: impl FnOnce(PathBuf) -> io::Result<T> + Send + 'static,
        apply: impl FnOnce(&mut Self, T) + 'static,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        let root = self.root.clone();
        let task = cx.background_executor().spawn(async move { work(root) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                view.busy = false;
                match result {
                    Ok(value) => apply(view, value),
                    Err(error) => view.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.read(
            cx,
            |root| history::list(&root),
            |view, rows| view.entries = rows,
        );
    }

    fn select(&mut self, id: String, cx: &mut Context<Self>) {
        self.read(
            cx,
            move |root| history::detail(&root, &id),
            |view, detail| {
                view.selected = Some(detail);
                view.comparison = None;
            },
        );
    }

    fn compare(&mut self, cx: &mut Context<Self>) {
        let (Some(baseline), Some(selected)) = (&self.baseline, &self.selected) else {
            return;
        };
        let baseline = baseline.clone();
        let candidate = selected.suite.id.clone();
        self.read(
            cx,
            move |root| history::compare(&root, &baseline, &candidate),
            |view, diff| {
                view.comparison = Some(diff);
            },
        );
    }
}

fn status(status: Status) -> &'static str {
    match status {
        Status::Pass => "PASS",
        Status::Fail => "FAIL",
        Status::Incomplete => "INCOMPLETE",
        Status::Flaky => "FLAKY",
    }
}

fn short(value: &str) -> &str {
    value.get(..12).unwrap_or(value)
}

fn failure(assertion: &AssertionResult) -> String {
    format!(
        "Assertion {} ({}) expected {}, observed {}",
        assertion.index + 1,
        assertion.kind,
        assertion.expected,
        assertion.actual
    )
}

impl Render for RegressionWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = crate::theme::palette(cx);
        let selected = self.selected.as_ref().map(|detail| &detail.suite.id);
        let mut list = div()
            .id("regression-list")
            .flex()
            .flex_col()
            .gap_1()
            .overflow_y_scroll();
        for entry in &self.entries {
            let id = entry.id.clone();
            list = list.child(
                Button::new(
                    id.clone(),
                    format!(
                        "{} · {} · {}",
                        status(entry.status),
                        short(&entry.declared_revision),
                        short(&id)
                    ),
                )
                .selected(selected == Some(&id))
                .disabled(self.busy)
                .on_click(cx.listener(move |view, _, _, cx| view.select(id.clone(), cx))),
            );
        }
        let can_compare =
            selected.is_some() && self.baseline.is_some() && selected != self.baseline.as_ref();
        let mut details = div()
            .id("regression-details")
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap_2()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("use-baseline", "Set as baseline")
                            .disabled(self.busy || selected.is_none())
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.baseline =
                                    view.selected.as_ref().map(|detail| detail.suite.id.clone());
                                view.comparison = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("compare-suites", "Compare with baseline")
                            .disabled(self.busy || !can_compare)
                            .on_click(cx.listener(|view, _, _, cx| view.compare(cx))),
                    ),
            );
        if let Some(baseline) = &self.baseline {
            details = details.child(format!("Baseline suite {}", short(baseline)));
        }
        if let Some(detail) = &self.selected {
            details = details.child(format!(
                "{} · revision {} · worker {}",
                status(detail.suite.status),
                short(&detail.suite.declared_revision),
                short(&detail.suite.worker_sha256)
            ));
        }
        if let Some(comparison) = &self.comparison {
            for case in &comparison.cases {
                let old = case.baseline.map(status).unwrap_or("Absent");
                let new = case.candidate.map(status).unwrap_or("Absent");
                let observation = match case.observation {
                    "compared" => "",
                    "scenario_changed" => " · Scenario definition changed",
                    "inconclusive" => " · Comparable evidence unavailable",
                    "added" => " · Added",
                    "removed" => " · Removed",
                    _ => "",
                };
                details = details.child(format!(
                    "{} · {old} → {new}{observation}",
                    case.scenario.display()
                ));
                if let Some(change) = &case.checkpoint {
                    let before = change
                        .baseline_sha256
                        .as_deref()
                        .map(short)
                        .unwrap_or("Absent");
                    let after = change
                        .candidate_sha256
                        .as_deref()
                        .map(short)
                        .unwrap_or("Absent");
                    details = details.child(format!(
                        "{} screen at frame {} · {before} → {after}",
                        change.screen, change.frame
                    ));
                }
                if let Some(change) = &case.assertion_change {
                    details = details.child(format!(
                        "Assertion {} ({}) observed {} → {}",
                        change.candidate.index + 1,
                        change.candidate.kind,
                        change.baseline.actual,
                        change.candidate.actual
                    ));
                }
                if let Some(assertion) = &case.first_failure {
                    details = details.child(failure(assertion));
                }
            }
        } else if let Some(detail) = &self.selected {
            for case in &detail.cases {
                details = details.child(format!(
                    "{} · {}",
                    status(case.status),
                    case.scenario.display()
                ));
                if let Some(assertion) = &case.first_failure {
                    details = details.child(failure(assertion));
                }
                if let Some(error) = &case.error {
                    details = details.child(error.clone());
                }
            }
        }
        if let Some(error) = &self.error {
            details = details.child(error.clone());
        } else if self.busy {
            details = details.child("Reading suite reports…");
        } else if self.entries.is_empty() {
            details = details.child("No completed regression suites in this host's history.");
        }
        div()
            .size_full()
            .bg(theme.window)
            .text_color(theme.text)
            .flex()
            .gap_4()
            .p_4()
            .child(
                div()
                    .w(px(255.))
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        Button::new("regression-refresh", "Refresh")
                            .disabled(self.busy)
                            .on_click(cx.listener(|view, _, _, cx| view.refresh(cx))),
                    )
                    .child(list),
            )
            .child(details)
    }
}
