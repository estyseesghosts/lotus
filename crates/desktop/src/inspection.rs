//! Owned inspector rows. Parsing, formatting and trace loading run off the GPUI thread.
use lotus_protocol::debug::DebugCommand as D;
use serde_json::Value;
use std::{
    collections::{BTreeMap, VecDeque},
    io,
    path::Path,
    sync::Arc,
};

use lotus_session::supervisor::SessionSummary;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionTarget {
    pub id: String,
    pub created_unix_ms: u64,
    pub epoch: u64,
}
impl SessionTarget {
    pub(crate) fn from_summary(summary: &SessionSummary) -> Self {
        Self {
            id: summary.info.id.clone(),
            created_unix_ms: summary.info.created_unix_ms,
            epoch: summary.telemetry.epoch,
        }
    }
    /// Reject missing/replaced sessions and responses from a previous guest epoch.
    /// Availability/permission is command-specific and must also be checked.
    pub fn resolve<'a>(&self, sessions: &'a [SessionSummary]) -> Option<&'a SessionSummary> {
        sessions.iter().find(|s| {
            s.info.id == self.id
                && s.info.created_unix_ms == self.created_unix_ms
                && s.telemetry.epoch == self.epoch
        })
    }

    pub fn is_current(&self, inspected: &str, sessions: &[SessionSummary]) -> bool {
        self.id == inspected && self.resolve(sessions).is_some()
    }

    /// NativeSession::finish tags worker replies with their actual epoch. Host
    /// metadata/errors may omit it; an explicit tag must match the request.
    pub fn accepts_reply(&self, value: &Value) -> bool {
        value
            .get("epoch")
            .is_none_or(|epoch| epoch.as_u64() == Some(self.epoch))
            && value.get("disassembly").is_none_or(|assembly| {
                assembly
                    .get("epoch")
                    .is_none_or(|epoch| epoch.as_u64() == Some(self.epoch))
            })
    }
}

#[derive(Clone, Debug)]
pub struct InspectionSnapshot {
    /// Imported traces have no worker target.
    pub target: Option<SessionTarget>,
    pub panel: Panel,
    /// Present only for background polls. Explicit results have their own request ID.
    pub offset: Option<u64>,
    pub view: Arc<Inspection>,
}

pub fn snapshot_current(
    snapshot: &InspectionSnapshot,
    inspected: &str,
    sessions: &[SessionSummary],
) -> bool {
    snapshot
        .target
        .as_ref()
        .is_none_or(|target| target.is_current(inspected, sessions))
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(usize)]
pub enum Panel {
    Cpu,
    Threads,
    Breakpoints,
    Memory,
    Settings,
    Ipc,
    Logs,
    Gpu,
    Audio,
    States,
    Timeline,
    Result,
}
impl Panel {
    /// Stable preference IDs (handbook 19); independent of display/order/index.
    pub fn id(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Threads => "threads",
            Self::Breakpoints => "breakpoints",
            Self::Memory => "memory",
            Self::Settings => "settings",
            Self::Ipc => "ipc",
            Self::Logs => "logs",
            Self::Gpu => "gpu",
            Self::Audio => "audio",
            Self::States => "states",
            Self::Timeline => "timeline",
            Self::Result => "result",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.id() == id)
    }
    pub const ALL: [Self; 12] = [
        Self::Cpu,
        Self::Threads,
        Self::Breakpoints,
        Self::Memory,
        Self::Settings,
        Self::Ipc,
        Self::Logs,
        Self::Gpu,
        Self::Audio,
        Self::States,
        Self::Timeline,
        Self::Result,
    ];
    /// Order of the inspector's view pop-up: grouped as `group` lists them.
    /// Twelve views exceed a tab view's 2 to about 6 tabs, so the panel uses
    /// a pop-up menu at its top instead (HIG pp. 338, 341).
    pub const MENU: [Self; 12] = [
        Self::Cpu,
        Self::Threads,
        Self::Breakpoints,
        Self::Memory,
        Self::Gpu,
        Self::Audio,
        Self::Logs,
        Self::Ipc,
        Self::Timeline,
        Self::States,
        Self::Settings,
        Self::Result,
    ];
    pub fn at(index: usize) -> Self {
        Self::ALL.get(index).copied().unwrap_or(Self::Cpu)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Threads => "Threads",
            Self::Breakpoints => "Breakpoints",
            Self::Memory => "Memory",
            Self::Settings => "Settings",
            Self::Ipc => "IPC",
            Self::Logs => "Logs",
            Self::Gpu => "GPU",
            Self::Audio => "Audio",
            Self::States => "Snapshots",
            Self::Timeline => "Timeline",
            Self::Result => "Result",
        }
    }
    /// Destinations on the inspector rail, most used first; Ctrl+1..7 select them.
    /// Execution views lead, then memory, GPU, logs and snapshots.
    pub const RAIL: [Self; 7] = [
        Self::Cpu,
        Self::Breakpoints,
        Self::Threads,
        Self::Memory,
        Self::Gpu,
        Self::Logs,
        Self::States,
    ];
    /// The rest, behind the rail's overflow menu, grouped by `group`.
    pub const OVERFLOW: [Self; 5] = [
        Self::Ipc,
        Self::Timeline,
        Self::Audio,
        Self::Settings,
        Self::Result,
    ];
    pub fn group(self) -> &'static str {
        match self {
            Self::Cpu | Self::Threads | Self::Breakpoints => "EXECUTION",
            Self::Memory => "MEMORY",
            Self::Gpu | Self::Audio => "MEDIA",
            Self::Logs | Self::Ipc | Self::Timeline => "TRACE",
            Self::States | Self::Settings | Self::Result => "SESSION",
        }
    }
    pub fn icon(self) -> crate::icons::Icon {
        use crate::icons::Icon;
        match self {
            Self::Cpu => Icon::Cpu,
            Self::Threads => Icon::Threads,
            Self::Breakpoints => Icon::Breakpoints,
            Self::Memory => Icon::Memory,
            Self::Settings => Icon::Settings,
            Self::Ipc => Icon::Ipc,
            Self::Logs => Icon::Logs,
            Self::Gpu => Icon::Gpu,
            Self::Audio => Icon::Audio,
            Self::States => Icon::Snapshots,
            Self::Timeline => Icon::Timeline,
            Self::Result => Icon::Result,
        }
    }
    /// Next or previous panel in rail-then-overflow order (Ctrl+] / Ctrl+[).
    pub fn cycle(self, forward: bool) -> Self {
        let order: Vec<Self> = Self::RAIL.into_iter().chain(Self::OVERFLOW).collect();
        let here = order.iter().position(|p| *p == self).unwrap_or(0);
        let len = order.len();
        order[if forward {
            (here + 1) % len
        } else {
            (here + len - 1) % len
        }]
    }
    /// Refreshed on a cadence while shown. The others (settings, memory map) are fetched when
    /// the view, the session or the paused frame changes.
    pub fn live(self) -> bool {
        !matches!(self, Self::Settings | Self::Memory | Self::Result)
    }
    pub fn command(self, offset: u64) -> Option<D> {
        Some(match self {
            Self::Cpu => D::Registers { core: 0 },
            Self::Threads => D::Threads,
            Self::Breakpoints => D::Breakpoints,
            Self::Memory => D::MemoryMap,
            Self::Settings => D::SettingsList {
                offset: offset.try_into().ok()?,
                limit: 32,
            },
            Self::Ipc | Self::Timeline => D::Events {
                after: offset,
                limit: 128,
            },
            Self::Logs => D::Logs {
                after: offset,
                limit: 128,
            },
            Self::Gpu => D::GpuDraws,
            Self::Audio => D::DspInspect,
            Self::States => D::States,
            Self::Result => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InspectionSelection {
    pub panel: Panel,
    page: Option<(SessionTarget, u64)>,
}

impl InspectionSelection {
    pub fn first_page(panel: Panel) -> Self {
        Self { panel, page: None }
    }

    pub fn with_offset(panel: Panel, target: &SessionTarget, offset: u64) -> Self {
        Self {
            panel,
            page: (offset != 0).then(|| (target.clone(), offset)),
        }
    }

    pub(crate) fn offset_for(&self, target: &SessionTarget) -> u64 {
        self.page
            .as_ref()
            .filter(|(owner, _)| owner == target)
            .map_or(0, |(_, offset)| *offset)
    }
}

#[derive(Default)]
pub(crate) struct PanelState {
    offset: u64,
    back: VecDeque<u64>,
}

impl PanelState {
    pub(crate) fn offset(&self) -> u64 {
        self.offset
    }

    pub(crate) fn advance(&mut self, next: u64) {
        if next <= self.offset {
            return;
        }
        if self.back.len() == 64 {
            self.back.pop_front();
        }
        self.back.push_back(self.offset);
        self.offset = next;
    }

    pub(crate) fn back(&mut self) {
        self.offset = self.back.pop_back().unwrap_or(0);
    }
}

#[derive(Default)]
pub(crate) struct InspectorState {
    panels: BTreeMap<(SessionTarget, Panel), PanelState>,
}

impl InspectorState {
    pub(crate) fn panel(&self, target: &SessionTarget, panel: Panel) -> Option<&PanelState> {
        self.panels.get(&(target.clone(), panel))
    }

    pub(crate) fn panel_mut(&mut self, target: &SessionTarget, panel: Panel) -> &mut PanelState {
        self.panels.entry((target.clone(), panel)).or_default()
    }

    pub(crate) fn retain(&mut self, sessions: &[SessionSummary]) {
        self.panels
            .retain(|(target, _), _| target.resolve(sessions).is_some());
    }
}
#[derive(Debug, Clone)]
pub struct Row {
    pub lane: &'static str,
    pub key: String,
    pub value: String,
    pub action: Option<D>,
}
#[derive(Debug, Clone)]
pub struct Inspection {
    pub rows: Arc<Vec<Row>>,
    pub source: String,
    pub next: Option<u64>,
    pub lost: u64,
    pub error: Option<(io::ErrorKind, String)>,
}
const ROW_CAP: usize = 100_000;
const TEXT_BUDGET: usize = 32 << 20;
fn clipped(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}
fn lane(event: &Value) -> &'static str {
    match event["kind"].as_str().unwrap_or("") {
        "pica" => "GPU",
        "ipc" | "svc_enter" | "svc_exit" => "kernel",
        "guest_log" | "hio_stdout" | "native_log" => "log",
        _ => "CPU",
    }
}
impl Inspection {
    pub fn from_value(value: &Value, panel: Panel) -> Self {
        let mut rows = Vec::new();
        let mut budget = TEXT_BUDGET;
        fn flatten(
            value: &Value,
            key: String,
            rows: &mut Vec<Row>,
            budget: &mut usize,
            depth: u32,
        ) {
            if rows.len() >= 8192 || *budget < 2048 {
                return;
            }
            if depth < 10 {
                match value {
                    Value::Object(map) => {
                        for (name, value) in map {
                            flatten(value, format!("{key}.{name}"), rows, budget, depth + 1);
                        }
                        return;
                    }
                    Value::Array(values) => {
                        for (i, value) in values.iter().enumerate() {
                            flatten(value, format!("{key}[{i}]"), rows, budget, depth + 1);
                        }
                        return;
                    }
                    _ => {}
                }
            }
            let key = clipped(&key, 256);
            let text = if key.contains("registers[") || key.ends_with(".cpsr") {
                crate::lotus_window::hex_string(value)
            } else {
                clipped(&value.to_string(), 1024)
            };
            *budget -= key.len() + text.len();
            rows.push(Row {
                lane: "state",
                key,
                value: text,
                action: None,
            });
        }
        if panel == Panel::Cpu {
            if let Some(registers) = value["registers"].as_array() {
                for (i, r) in registers.iter().enumerate() {
                    rows.push(Row {
                        lane: "CPU",
                        key: format!("r{i}"),
                        value: crate::lotus_window::hex_string(r),
                        action: None,
                    });
                }
            }
            if let Some(assembly) = value["disassembly"]["assembly"].as_str() {
                for line in assembly
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .take(100)
                {
                    rows.push(Row {
                        lane: "CPU",
                        key: "instruction".into(),
                        value: line.into(),
                        action: None,
                    });
                }
            }
        }
        if matches!(panel, Panel::Timeline | Panel::Logs | Panel::Ipc) {
            if let Some(events) = value["events"].as_array() {
                for event in events {
                    if panel == Panel::Ipc && event["kind"] != "ipc" {
                        continue;
                    }
                    rows.push(Self::event(event));
                }
            }
        } else if panel == Panel::Settings {
            if let Some(settings) = value["settings"].as_array() {
                for setting in settings {
                    let name = setting["name"].as_str().unwrap_or("");
                    let v = &setting["value"];
                    rows.push(Row {
                        lane: "setting",
                        key: name.into(),
                        value: format!(
                            "{} · {} · {}",
                            v, setting["apply"], setting["consumer_sources"]
                        ),
                        action: v.as_bool().map(|v| D::SettingsSet {
                            name: name.into(),
                            value: lotus_protocol::debug::SettingValue::Boolean(!v),
                        }),
                    });
                }
            }
        } else if panel == Panel::States {
            if let Some(names) = value["states"].as_array() {
                for name in names.iter().filter_map(Value::as_str) {
                    rows.push(Row {
                        lane: "snapshot",
                        key: name.into(),
                        value: "Restore checkpoint (pauses execution)".into(),
                        action: Some(D::LoadState { name: name.into() }),
                    });
                }
            }
        } else if panel == Panel::Gpu {
            if let Some(draws) = value["draws"].as_array() {
                for draw in draws {
                    rows.push(Row {
                        lane: "GPU",
                        key: format!("draw {}", draw["draw"]),
                        value: format!(
                            "command {} · {} vertices · inspect frozen state",
                            draw["command"], draw["vertices"]
                        ),
                        action: draw["draw"].as_u64().map(|draw| D::GpuDraw { draw }),
                    });
                }
            }
        } else {
            if let Some(resources) = value["capture"]["resources"].as_array() {
                for resource in resources {
                    if let (Some(address), Some(width), Some(height), Some(format)) = (
                        resource["address"].as_u64(),
                        resource["width"].as_u64(),
                        resource["height"].as_u64(),
                        resource["format"].as_u64(),
                    ) {
                        rows.push(Row {
                            lane: "texture",
                            key: format!("0x{address:08x}"),
                            value: format!("{width}x{height} format {format} · view surface"),
                            action: Some(D::Surface {
                                address: address as u32,
                                width: width as u32,
                                height: height as u32,
                                format: format as u32,
                            }),
                        });
                    }
                }
                if let Some(draw) = value["capture"]["draw"].as_u64() {
                    rows.push(Row {
                        lane: "shader",
                        key: format!("draw {draw}"),
                        value: "Trace vertex 0 from frozen inputs".into(),
                        action: Some(D::ShaderTrace { draw, vertex: 0 }),
                    });
                }
            }
            flatten(value, String::new(), &mut rows, &mut budget, 0);
        }
        Self {
            rows: Arc::new(rows),
            source: format!(
                "{} · pin {} · profile {} · epoch {} · paused {} · cache flush {}",
                value["identity"]["backend"],
                value["identity"]["source"],
                value["profile_hash"],
                value["epoch"],
                value["paused"],
                value["rasterizer_cache_flush"]
            ),
            next: value["next"].as_u64(),
            lost: value["lost_total"].as_u64().unwrap_or(0),
            error: None,
        }
    }

    pub(crate) fn failed(error: io::Error) -> Self {
        Self {
            rows: Arc::default(),
            source: String::new(),
            next: None,
            lost: 0,
            error: Some((error.kind(), error.to_string())),
        }
    }
    pub fn draw_diff(before: &Value, after: &Value) -> Vec<String> {
        let mut changes = Vec::new();
        for key in [
            "registers",
            "uniforms_f32_bits",
            "bool_uniforms",
            "vertex_program",
            "swizzle",
            "resources",
        ] {
            if let (Some(a), Some(b)) = (before[key].as_array(), after[key].as_array()) {
                for i in 0..a.len().max(b.len()) {
                    let same = if key == "resources" {
                        a.get(i).zip(b.get(i)).is_some_and(|(a, b)| {
                            [
                                "address",
                                "bytes",
                                "kind",
                                "content_xxh3_64",
                                "format",
                                "width",
                                "height",
                                "stride",
                            ]
                            .iter()
                            .all(|key| a[*key] == b[*key])
                        })
                    } else {
                        a.get(i) == b.get(i)
                    };
                    if !same {
                        changes.push(format!(
                            "{key}[{i}]: {} -> {}",
                            clipped(&a.get(i).unwrap_or(&Value::Null).to_string(), 256),
                            clipped(&b.get(i).unwrap_or(&Value::Null).to_string(), 256)
                        ));
                        if changes.len() == 128 {
                            return changes;
                        }
                    }
                }
            }
        }
        changes
    }
    fn event(event: &Value) -> Row {
        Row {
            lane: lane(event),
            key: format!("#{} · tick {}", event["sequence"], event["guest_ticks"]),
            value: clipped(&event.to_string(), 1024),
            action: None,
        }
    }
    pub fn load_trace(path: &Path) -> io::Result<Self> {
        let file = std::fs::File::open(path)?;
        if file.metadata()?.len() > 64 << 20 {
            return Err(io::Error::other("trace exceeds 64 MiB"));
        }
        Self::parse_trace(std::io::BufReader::new(file))
    }
    fn parse_trace(reader: impl io::BufRead) -> io::Result<Self> {
        let mut rows = Vec::new();
        let mut bytes = 0;
        let mut lost = 0;
        for line in reader.lines() {
            let line = line?;
            if line.len() > 65536 {
                return Err(io::Error::other("trace row exceeds 64 KiB"));
            }
            if line.trim().is_empty() {
                continue;
            }
            let event: Value = serde_json::from_str(&line).map_err(io::Error::other)?;
            let event = event.get("data").unwrap_or(&event);
            let row = Self::event(event);
            bytes += row.key.len() + row.value.len();
            if rows.len() == ROW_CAP || bytes > TEXT_BUDGET {
                lost += 1;
                continue;
            }
            rows.push(row);
        }
        Ok(Self {
            source: "Imported JSONL · guest ticks kept separate from host timestamps".into(),
            rows: Arc::new(rows),
            next: None,
            lost,
            error: None,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_reply_epochs_override_a_lagging_telemetry_snapshot() {
        let target = SessionTarget {
            id: "a".into(),
            created_unix_ms: 1,
            epoch: 4,
        };
        assert!(target.accepts_reply(&serde_json::json!({"epoch":4})));
        assert!(target.accepts_reply(&serde_json::json!({"host_metadata":true})));
        for reply in [
            serde_json::json!({"epoch":5}),
            serde_json::json!({"epoch":null}),
            serde_json::json!({"epoch":"4"}),
            serde_json::json!({"epoch":4,"disassembly":{"epoch":5}}),
        ] {
            assert!(!target.accepts_reply(&reply), "{reply}");
        }
    }
    #[test]
    fn rail_and_overflow_cover_every_panel_once() {
        let ids: std::collections::BTreeSet<_> = Panel::ALL.into_iter().map(Panel::id).collect();
        assert_eq!(ids.len(), Panel::ALL.len());
        for panel in Panel::ALL {
            assert_eq!(Panel::from_id(panel.id()), Some(panel));
        }
        assert_eq!(Panel::from_id("future"), None);
        let mut seen: Vec<Panel> = Panel::RAIL.into_iter().chain(Panel::OVERFLOW).collect();
        assert_eq!(seen.len(), Panel::ALL.len());
        seen.sort_by_key(|p| *p as usize);
        assert_eq!(seen, Panel::ALL);
        assert_eq!(Panel::Cpu.cycle(false), Panel::Result);
        assert_eq!(Panel::Result.cycle(true), Panel::Cpu);
    }
    #[test]
    fn large_trace_is_bounded_and_preserves_lanes() {
        let line = b"{\"kind\":\"ipc\",\"guest_ticks\":123,\"sequence\":4}\n";
        let mut data = Vec::new();
        for _ in 0..100001 {
            data.extend_from_slice(line);
        }
        let trace = Inspection::parse_trace(io::Cursor::new(data)).unwrap();
        assert_eq!(trace.rows.len(), 100000);
        assert_eq!(trace.lost, 1);
        assert_eq!(trace.rows[0].lane, "kernel");
    }
    #[test]
    fn settings_only_offer_typed_boolean_edits() {
        let view = Inspection::from_value(
            &serde_json::json!({"settings":[{"name":"use_cpu_jit","value":true}]}),
            Panel::Settings,
        );
        assert!(matches!(view.rows[0].action, Some(D::SettingsSet { .. })));
    }
    #[test]
    fn draw_diff_uses_content_and_registers_instead_of_capture_versions() {
        let a = serde_json::json!({"registers":[1,2],"resources":[{"address":16,"content_xxh3_64":5,"version":1}]});
        let b = serde_json::json!({"registers":[1,3],"resources":[{"address":16,"content_xxh3_64":5,"version":2}]});
        let changes = Inspection::draw_diff(&a, &b);
        assert_eq!(changes.len(), 1);
        assert!(changes[0].starts_with("registers[1]"));
    }
}
