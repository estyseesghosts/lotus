//! lotus#95: the desktop Record Test window. It holds one session, fixed when it opens, so
//! a selection or keyboard focus change never moves a recording. Every operation is
//! `lotus_mcp::server::record_test_operation` with `Actor::Desktop`, the path MCP
//! `lotus_record_test` and `ctr-cli record-test` use (tests/record_test.rs).
use std::collections::BTreeSet;
use std::{io, sync::Arc, time::Duration};

use gpui::{div, prelude::*, px, size, App, Bounds, Context, Entity, Window, WindowHandle};
use lotus_mcp::server::record_test_operation;
use lotus_mcp::tool_arguments::{RecordTestOperation, RecordTestRead, RecordTestRegion};
use lotus_session::{supervisor::Supervisor, telemetry::Actor};
use serde_json::Value;
use silvestris::controls::{Button, TextField};

struct Opened(WindowHandle<RecordTestWindow>);
impl gpui::Global for Opened {}

/// Show the window for `session`. An open window keeps its session while that session has
/// an active recording.
pub fn open(cx: &mut App, host: Arc<Supervisor>, runtime: tokio::runtime::Handle, session: String) {
    if let Some(handle) = cx.try_global::<Opened>().map(|opened| opened.0) {
        if handle
            .update(cx, |view, window, cx| {
                view.retarget(session.clone(), cx);
                window.activate_window();
            })
            .is_ok()
        {
            return;
        }
    }
    let bounds = Bounds::centered(None, size(px(720.), px(640.)), cx);
    let options =
        silvestris::chrome::window_options("Record Test", bounds, size(px(520.), px(440.)), cx);
    match silvestris::chrome::open_window(options, cx, |window, cx| {
        cx.new(|cx| RecordTestWindow::new(host, runtime, session, window, cx))
    }) {
        Ok(handle) => cx.set_global(Opened(handle)),
        Err(error) => tracing::warn!(%error, "cannot open record test"),
    }
}

pub fn close(cx: &mut App) {
    if let Some(handle) = cx.try_global::<Opened>().map(|opened| opened.0) {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }
}

/// `0x00102008, 0x102004:2`: addresses in hex (`0x`) or decimal, optional `:width` of 1, 2
/// or 4 bytes (default 4), separated by commas or spaces.
pub(crate) fn parse_reads(text: &str) -> Result<Vec<RecordTestRead>, String> {
    text.split([',', ' ', '\n'])
        .filter(|token| !token.is_empty())
        .map(|token| {
            let (address, width) = token.split_once(':').unwrap_or((token, "4"));
            let address = number(address).ok_or_else(|| format!("bad address {address}"))?;
            let width = match width {
                "1" => 1,
                "2" => 2,
                "4" => 4,
                _ => return Err(format!("width of {token} must be 1, 2 or 4")),
            };
            Ok(RecordTestRead { address, width })
        })
        .collect()
}

/// `top 0 0 400 120; bottom 0 120 320 120`: screen, x, y, width, height per region.
pub(crate) fn parse_regions(text: &str) -> Result<Vec<RecordTestRegion>, String> {
    text.split(';')
        .map(str::trim)
        .filter(|region| !region.is_empty())
        .map(|region| {
            let fields: Vec<&str> = region.split_whitespace().collect();
            let [screen, x, y, width, height] = fields[..] else {
                return Err(format!("{region}: expected screen x y width height"));
            };
            let value = |field: &str| number(field).ok_or_else(|| format!("bad number {field}"));
            Ok(RecordTestRegion {
                screen: screen.to_ascii_lowercase(),
                x: value(x)?,
                y: value(y)?,
                width: value(width)?,
                height: value(height)?,
            })
        })
        .collect()
}

/// Extra assertions as a YAML sequence, the scenario file's own syntax, e.g.
/// `[{type: reached, address: 0x00100254}]`. Empty text is none.
pub(crate) fn parse_assertions(text: &str) -> Result<Vec<Value>, String> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_yaml::from_str::<Vec<Value>>(text)
        .map_err(|error| format!("extra assertions must be a YAML list: {error}"))
}

fn number(text: &str) -> Option<u32> {
    match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

struct RecordTestWindow {
    host: Arc<Supervisor>,
    runtime: tokio::runtime::Handle,
    session: String,
    /// `status` summary of the session's recording; `Null` when it has none.
    summary: Value,
    mark_label: Entity<TextField>,
    mark_memory: Entity<TextField>,
    mark_regions: Entity<TextField>,
    export_name: Entity<TextField>,
    export_stop: Entity<TextField>,
    export_assertions: Entity<TextField>,
    omit: BTreeSet<usize>,
    screens_omitted: BTreeSet<usize>,
    busy: bool,
    polling: bool,
    status: String,
}

impl RecordTestWindow {
    fn new(
        host: Arc<Supervisor>,
        runtime: tokio::runtime::Handle,
        session: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut field = |label, placeholder| {
            cx.new(|cx| {
                let mut field = TextField::new(window, cx);
                field.set_label(label, cx);
                field.set_placeholder(placeholder, cx);
                field
            })
        };
        let view = Self {
            host,
            runtime,
            session,
            summary: Value::Null,
            mark_label: field("Checkpoint label", "Checkpoint label"),
            mark_memory: field("Memory words", "0x00102008, 0x00102004:2"),
            mark_regions: field("Screen regions", "top 0 0 400 120; bottom 0 120 320 120"),
            export_name: field("Test name", "letters, digits, _ or -"),
            export_stop: field("Stop frame", "Recorded stop frame"),
            export_assertions: field("Extra assertions", "[{type: reached, address: 0x00100254}]"),
            omit: BTreeSet::new(),
            screens_omitted: BTreeSet::new(),
            busy: false,
            polling: false,
            status: String::new(),
        };
        // Follow input counts and frames while recording; stops when the window closes.
        cx.spawn(async move |this, cx| loop {
            if this
                .update(cx, |view, cx| {
                    if view.is_active() || view.summary.is_null() {
                        view.refresh(cx);
                    }
                })
                .is_err()
            {
                break;
            }
            cx.background_executor().timer(Duration::from_secs(1)).await;
        })
        .detach();
        view
    }

    fn is_active(&self) -> bool {
        self.summary["active"].as_bool() == Some(true)
    }

    fn retarget(&mut self, session: String, cx: &mut Context<Self>) {
        if session == self.session {
            return;
        }
        if self.is_active() {
            self.status = format!(
                "Still recording {}. Stop or discard it before recording another session.",
                self.session
            );
        } else {
            self.session = session;
            self.summary = Value::Null;
            self.omit.clear();
            self.screens_omitted.clear();
            self.status.clear();
            self.refresh(cx);
        }
        cx.notify();
    }

    fn spawn(
        &self,
        operation: RecordTestOperation,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, io::Result<Value>, &mut Context<Self>) + 'static,
    ) {
        let (host, session) = (self.host.clone(), self.session.clone());
        let task = self.runtime.spawn(async move {
            record_test_operation(&host, &session, operation, Actor::Desktop).await
        });
        let session = self.session.clone();
        cx.spawn(async move |this, cx| {
            let result = task.await.map_err(io::Error::other).and_then(|r| r);
            let _ = this.update(cx, |view, cx| {
                // A reply for a session the window no longer shows is dropped.
                if view.session == session {
                    done(view, result, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.polling {
            return;
        }
        self.polling = true;
        self.spawn(RecordTestOperation::Status, cx, |view, result, _| {
            view.polling = false;
            match result {
                Ok(summary) => view.summary = summary,
                Err(error) => view.status = error.to_string(),
            }
        });
    }

    /// One user operation: refused while another runs, then the status is re-read.
    fn run(
        &mut self,
        operation: RecordTestOperation,
        cx: &mut Context<Self>,
        apply: impl FnOnce(&mut Self, Value, &mut Context<Self>) + 'static,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.status.clear();
        self.spawn(operation, cx, |view, result, cx| {
            view.busy = false;
            match result {
                Ok(value) => apply(view, value, cx),
                Err(error) => view.status = error.to_string(),
            }
            view.polling = false;
            view.refresh(cx);
        });
        cx.notify();
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.omit.clear();
        self.screens_omitted.clear();
        self.run(RecordTestOperation::Start, cx, |view, _, _| {
            view.status = "Recording from a fresh boot. The session is paused at frame 0.".into();
        });
    }

    fn mark(&mut self, cx: &mut Context<Self>) {
        let label = self.mark_label.read(cx).text().trim().to_string();
        let parsed = parse_reads(self.mark_memory.read(cx).text())
            .and_then(|memory| Ok((memory, parse_regions(self.mark_regions.read(cx).text())?)));
        let (memory, regions) = match parsed {
            Ok(parsed) => parsed,
            Err(error) => {
                self.status = error;
                cx.notify();
                return;
            }
        };
        let label = if label.is_empty() {
            format!(
                "checkpoint {}",
                self.summary["marks"].as_array().map_or(0, Vec::len) + 1
            )
        } else {
            label
        };
        self.run(
            RecordTestOperation::Mark {
                label,
                memory,
                regions,
            },
            cx,
            |view, mark, _| {
                view.status = format!("Checkpoint at frame {}.", mark["frame"]);
            },
        );
    }

    fn export(&mut self, cx: &mut Context<Self>) {
        let name = self.export_name.read(cx).text().trim().to_string();
        let stop = self.export_stop.read(cx).text().trim().to_string();
        let stop_frame = if stop.is_empty() {
            None
        } else if let Ok(frame) = stop.parse::<u64>() {
            Some(frame)
        } else {
            self.status = format!("stop frame {stop} is not a number");
            cx.notify();
            return;
        };
        let assertions = match parse_assertions(self.export_assertions.read(cx).text()) {
            Ok(assertions) => assertions,
            Err(error) => {
                self.status = error;
                cx.notify();
                return;
            }
        };
        self.run(
            RecordTestOperation::Export {
                name,
                stop_frame,
                omit_marks: self.omit.iter().copied().collect(),
                omit_screen_hashes: self.screens_omitted.iter().copied().collect(),
                assertions,
            },
            cx,
            |view, exported, cx| {
                let directory = exported["directory"].as_str().unwrap_or_default();
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(directory.into()));
                let manifest = &exported["manifest"];
                let mut status = format!(
                    "Exported to {directory} (path copied). Place {} next to scenario.yaml.",
                    manifest["content"]["name"].as_str().unwrap_or("the 3DSX")
                );
                for condition in manifest["conditions"].as_array().into_iter().flatten() {
                    status.push_str(&format!(
                        "\nNot replayed: {}",
                        condition["detail"].as_str().unwrap_or_default()
                    ));
                }
                view.status = status;
            },
        );
    }

    fn toggle(set: &mut BTreeSet<usize>, index: usize) {
        if !set.remove(&index) {
            set.insert(index);
        }
    }
}

impl Render for RecordTestWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = crate::theme::palette(cx);
        let control = self.host.allows_control() && !self.busy;
        let summary = &self.summary;
        let recorded = !summary.is_null();
        let active = self.is_active();
        let invalid = summary["invalid"].as_str();
        let stopped = summary["stopped_at_frame"].as_u64();

        let state = match (recorded, invalid, stopped) {
            (false, _, _) => "No recording. Reload and Record restarts the content from frame 0 and records live input.".to_string(),
            (true, Some(reason), _) => format!("Invalid: {reason}"),
            (true, None, Some(frame)) => format!("Stopped at frame {frame}"),
            (true, None, None) => "Recording".to_string(),
        };
        let mut header = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(format!("Session {}", self.session))
            .child(state);
        if recorded {
            header = header.child(div().text_color(theme.text_secondary).child(format!(
                    "{} inputs, last at frame {} · {} · {} · determinism {}",
                    summary["inputs"],
                    summary["last_input_frame"]
                        .as_u64()
                        .map_or("-".into(), |frame| frame.to_string()),
                    summary["model"].as_str().unwrap_or_default(),
                    summary["renderer"].as_str().unwrap_or_default(),
                    if summary["determinism"].as_bool() == Some(true) {
                        "on"
                    } else {
                        "off"
                    },
                )));
            let interventions = summary["interventions"].as_array().map_or(0, Vec::len) as u64
                + summary["interventions_dropped"].as_u64().unwrap_or(0);
            if interventions > 0 {
                header = header.child(div().text_color(theme.warning).child(format!(
                    "{interventions} debugger change(s) while recording will not be replayed."
                )));
            }
            if summary["card_storage"]
                .as_array()
                .is_some_and(|files| !files.is_empty())
            {
                header = header.child(div().text_color(theme.warning).child(
                    "Guest storage held files a fresh run will not have; see the manifest.",
                ));
            }
        }

        let recording = div()
            .flex()
            .gap_2()
            .flex_wrap()
            .child(
                Button::new("record-start", "Reload and Record")
                    .disabled(!control || active)
                    .on_click(cx.listener(|view, _, _, cx| view.start(cx))),
            )
            .child(
                Button::new("record-stop", "Stop Recording")
                    .disabled(!control || !active)
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.run(RecordTestOperation::Stop, cx, |_, _, _| {})
                    })),
            )
            .child(
                Button::new("record-discard", "Discard")
                    .disabled(!control || !recorded)
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.run(RecordTestOperation::Discard, cx, |view, _, _| {
                            view.summary = Value::Null;
                        })
                    })),
            );

        let checkpoint = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(self.mark_label.clone())
            .child(self.mark_memory.clone())
            .child(self.mark_regions.clone())
            .child(
                Button::new("record-mark", "Add Checkpoint")
                    .disabled(!control || !active)
                    .on_click(cx.listener(|view, _, _, cx| view.mark(cx))),
            );

        let mut marks = div().flex().flex_col().gap_1();
        for (index, mark) in summary["marks"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let regions = mark["regions"].as_array().map_or(0, Vec::len);
            let memory = mark["memory"].as_array().map_or(0, Vec::len);
            marks = marks.child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .flex_wrap()
                    .child(format!(
                        "{} · frame {} · {} · {regions} region(s), {memory} value(s)",
                        mark["label"].as_str().unwrap_or_default(),
                        mark["frame"],
                        mark["source"].as_str().unwrap_or_default(),
                    ))
                    .child(
                        Button::new(("record-omit", index), "Omit")
                            .selected(self.omit.contains(&index))
                            .disabled(!control)
                            .on_click(cx.listener(move |view, _, _, cx| {
                                Self::toggle(&mut view.omit, index);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(("record-screens", index), "Regions and Memory Only")
                            .selected(self.screens_omitted.contains(&index))
                            .disabled(!control)
                            .on_click(cx.listener(move |view, _, _, cx| {
                                Self::toggle(&mut view.screens_omitted, index);
                                cx.notify();
                            })),
                    ),
            );
        }

        let exportable = recorded && invalid.is_none() && stopped.is_some();
        let export = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(self.export_name.clone())
            .child(self.export_stop.clone())
            .child(self.export_assertions.clone())
            .child(
                Button::new("record-export", "Export Test")
                    .disabled(!control || !exportable)
                    .on_click(cx.listener(|view, _, _, cx| view.export(cx))),
            );

        div()
            .id("record-test")
            .size_full()
            .bg(theme.window)
            .text_color(theme.text)
            .flex()
            .flex_col()
            .gap_4()
            .p_4()
            .overflow_y_scroll()
            .child(header)
            .child(recording)
            .child(checkpoint)
            .child(marks)
            .child(export)
            .child(self.status.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_fields_parse_reads_regions_and_assertions() {
        let reads = parse_reads("0x00102008, 0x102004:2 4096:1").unwrap();
        assert_eq!(
            reads
                .iter()
                .map(|read| (read.address, read.width))
                .collect::<Vec<_>>(),
            [(0x0010_2008, 4), (0x0010_2004, 2), (4096, 1)]
        );
        assert!(parse_reads("0x10:3").is_err());
        assert!(parse_reads("zz").is_err());
        assert!(parse_reads("").unwrap().is_empty());

        let regions = parse_regions("top 0 0 400 120; Bottom 0x10 120 320 120;").unwrap();
        assert_eq!(regions.len(), 2);
        assert_eq!(
            (regions[1].screen.as_str(), regions[1].x, regions[1].height),
            ("bottom", 16, 120)
        );
        assert!(parse_regions("top 0 0 400").is_err());

        let assertions = parse_assertions("[{type: reached, address: 0x00100254}]").unwrap();
        assert_eq!(assertions[0]["type"], "reached");
        assert_eq!(assertions[0]["address"], 0x0010_0254);
        assert!(parse_assertions("  ").unwrap().is_empty());
        assert!(parse_assertions("type: reached").is_err());
    }

    #[test]
    fn the_window_renders_a_recording_and_edits_its_export_in_every_theme() {
        use gpui::{Modifiers, TestAppContext};
        use lotus_session::supervisor::HostConfig;
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let root = std::env::temp_dir().join(format!(
            "lotus-record-window-{}",
            lotus_protocol::envelope::SessionUuid::generate()
        ));
        // Never started: the window's operations here are refused before the host runs one.
        std::fs::create_dir_all(&root).expect("root");
        std::fs::write(root.join("unused-worker.exe"), b"").expect("worker placeholder");
        let mut config = HostConfig::new(root.join("unused-worker.exe"), root.join("store"));
        config.allow_control = true;
        let host = Arc::new(Supervisor::new(config).expect("host"));
        let stopped = serde_json::json!({
            "active": false, "invalid": null, "stopped_at_frame": 120, "inputs": 12,
            "last_input_frame": 100, "model": "old3ds", "renderer": "software",
            "determinism": true, "interventions": [{"observed_frame": 30,
            "operation": "write_memory", "source": "mcp"}], "interventions_dropped": 0,
            "card_storage": ["sdmc/save.bin"],
            "marks": [{"label": "midway", "frame": 60, "source": "desktop", "memory": [],
                "regions": [{"screen": "top", "x": 0, "y": 0, "width": 40, "height": 20,
                "sha256": "0"}]}],
        });
        for theme in silvestris::ThemeId::ALL {
            let mut app = TestAppContext::single();
            app.update(|cx| {
                silvestris::test_support::initialize(
                    silvestris::ThemePreference::explicit(theme),
                    cx,
                )
            });
            let (host, handle) = (host.clone(), runtime.handle().clone());
            let (view, cx) = app.add_window_view(|window, cx| {
                RecordTestWindow::new(host, handle, "s1".into(), window, cx)
            });
            cx.simulate_resize(gpui::size(px(720.), px(900.)));
            let set = |cx: &mut gpui::VisualTestContext, summary: Value| {
                cx.update(|_, cx| {
                    view.update(cx, |view, cx| {
                        view.summary = summary;
                        view.polling = true; // no status poll overwrites the fixture
                        cx.notify();
                    })
                });
                cx.run_until_parked();
                cx.update(|window, cx| window.draw(cx).clear(cx));
            };
            set(cx, stopped.clone());
            for (button, _) in [("record-omit-0", ()), ("record-screens-0", ())] {
                let bounds = cx
                    .debug_bounds(button)
                    .unwrap_or_else(|| panic!("{button}"));
                cx.simulate_click(bounds.center(), Modifiers::default());
            }
            cx.update(|_, cx| {
                let view = view.read(cx);
                assert!(view.omit.contains(&0), "{theme:?}");
                assert!(view.screens_omitted.contains(&0), "{theme:?}");
            });

            // Checkpoint fields are checked before the host is asked.
            let mut active = stopped.clone();
            active["active"] = true.into();
            active["stopped_at_frame"] = Value::Null;
            set(cx, active);
            cx.update(|_, cx| {
                view.update(cx, |view, cx| {
                    view.mark_memory
                        .update(cx, |field, cx| field.set_text("0x10:3", cx));
                })
            });
            let mark = cx.debug_bounds("record-mark").expect("record-mark");
            cx.simulate_click(mark.center(), Modifiers::default());
            cx.update(|_, cx| {
                let view = view.read(cx);
                assert!(view.status.contains("1, 2 or 4"), "{}", view.status);
                assert!(!view.busy);
            });
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
