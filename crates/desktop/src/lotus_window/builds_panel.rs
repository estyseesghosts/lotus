use super::*;
use serde_json::Value;

impl LotusWindow {
    pub(super) fn render_builds(
        &self,
        target: &SessionTarget,
        data: Option<&Value>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        let content = div()
            .id("build-jobs")
            .flex()
            .flex_col()
            .min_h_0()
            .overflow_y_scroll()
            .gap_2()
            .p_3()
            .text_sm();
        let Some(data) = data else {
            return content.into_any_element();
        };
        let button = |id: String, label: &'static str, command: D, cx: &mut Context<Self>| {
            let target = target.clone();
            small_button(
                colors,
                id,
                label,
                Tone::Plain,
                self.control || !command.mutates(),
            )
            .on_click(cx.listener(move |view, _, _, _| {
                if view.control || !command.mutates() {
                    view.debug_command(&Some(target.clone()), Panel::Builds, command.clone());
                }
            }))
        };
        let profiles = data["profiles"].as_array().into_iter().flatten();
        let mut content = content;
        if data["profiles"].as_array().is_some_and(Vec::is_empty) {
            content = content.child("Configure a build profile in the Lotus host configuration.");
        }
        for name in profiles.filter_map(|profile| profile["name"].as_str()) {
            content = content.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(name.to_owned())
                    .child(button(
                        format!("build-{name}"),
                        "Build",
                        D::Build {
                            profile: name.into(),
                        },
                        cx,
                    )),
            );
        }
        let jobs = data["jobs"].as_array().into_iter().flatten().rev();
        let mut previous = None;
        for job in jobs {
            let Some(id) = job["job"].as_u64() else {
                continue;
            };
            let state = job["state"].as_str().unwrap_or_default();
            let profile = job["profile"].as_str().unwrap_or_default();
            let loaded = job["build"]["output"]["asset"].is_string()
                && job["build"]["output"]["asset"] == data["active_asset"];
            let mut actions = div().flex().flex_wrap().gap_1().child(button(
                format!("build-log-{id}"),
                "Log",
                D::BuildStatus {
                    job: Some(id),
                    log_from: 0,
                },
                cx,
            ));
            if state == "running" {
                actions = actions.child(button(
                    format!("build-cancel-{id}"),
                    "Cancel",
                    D::BuildCancel { job: id },
                    cx,
                ));
            }
            if state == "succeeded" {
                if job["build"]["kind"] == "3dsx" {
                    actions = actions.child(button(
                        format!("build-relaunch-{id}"),
                        "Relaunch",
                        D::RelaunchBuild { job: id },
                        cx,
                    ));
                }
                if let Some(newer) = previous {
                    actions = actions.child(button(
                        format!("build-compare-{id}"),
                        "Compare with next",
                        D::CompareBuilds { a: id, b: newer },
                        cx,
                    ));
                }
                previous = Some(id);
            }
            content = content.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .border_t_1()
                    .border_color(colors.border)
                    .pt_2()
                    .child(format!(
                        "#{id} {profile} / {}",
                        if loaded { "loaded" } else { state }
                    ))
                    .children(job["build"]["output"]["sha256"].as_str().map(str::to_owned))
                    .children(job["reason"].as_str().map(str::to_owned))
                    .child(actions),
            );
        }
        content.into_any_element()
    }
}
