//! Owned inspector rows. Parsing, formatting and trace loading run off the GPUI thread.
use lotus_protocol::debug::DebugCommand as D;
use serde::Deserialize;
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
    #[cfg(feature = "silvestris-ui")]
    Commands,
    #[cfg(feature = "silvestris-ui")]
    Gdb,
}
const SILVESTRIS_PANEL_COUNT: usize = 2 * cfg!(feature = "silvestris-ui") as usize;
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
            #[cfg(feature = "silvestris-ui")]
            Self::Commands => "commands",
            #[cfg(feature = "silvestris-ui")]
            Self::Gdb => "gdb",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.id() == id)
    }
    pub const ALL: [Self; 12 + SILVESTRIS_PANEL_COUNT] = [
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
        #[cfg(feature = "silvestris-ui")]
        Self::Commands,
        #[cfg(feature = "silvestris-ui")]
        Self::Gdb,
    ];
    /// Order of the inspector's view pop-up: grouped as `group` lists them.
    /// Twelve views exceed a tab view's 2 to about 6 tabs, so the panel uses
    /// a pop-up menu at its top instead (HIG pp. 338, 341).
    pub const MENU: [Self; 12 + SILVESTRIS_PANEL_COUNT] = [
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
        #[cfg(feature = "silvestris-ui")]
        Self::Commands,
        #[cfg(feature = "silvestris-ui")]
        Self::Gdb,
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
            #[cfg(feature = "silvestris-ui")]
            Self::Commands => "Commands",
            #[cfg(feature = "silvestris-ui")]
            Self::Gdb => "GDB",
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
    pub const OVERFLOW: [Self; 5 + SILVESTRIS_PANEL_COUNT] = [
        Self::Ipc,
        Self::Timeline,
        Self::Audio,
        Self::Settings,
        #[cfg(feature = "silvestris-ui")]
        Self::Commands,
        #[cfg(feature = "silvestris-ui")]
        Self::Gdb,
        Self::Result,
    ];
    pub fn group(self) -> &'static str {
        match self {
            Self::Cpu | Self::Threads | Self::Breakpoints => "EXECUTION",
            Self::Memory => "MEMORY",
            Self::Gpu | Self::Audio => "MEDIA",
            Self::Logs | Self::Ipc | Self::Timeline => "TRACE",
            Self::States | Self::Settings | Self::Result => "SESSION",
            #[cfg(feature = "silvestris-ui")]
            Self::Commands | Self::Gdb => "SESSION",
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
            #[cfg(feature = "silvestris-ui")]
            Self::Commands => Icon::Result,
            #[cfg(feature = "silvestris-ui")]
            Self::Gdb => Icon::Breakpoints,
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
        #[cfg(feature = "silvestris-ui")]
        if matches!(self, Self::Commands | Self::Gdb) {
            return false;
        }
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
            #[cfg(feature = "silvestris-ui")]
            Self::Commands | Self::Gdb => return None,
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
    pub(crate) selected_draw: Option<u64>,
    pub(crate) draw_a: Option<DrawArtifact>,
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

/// Parse the address prefix emitted by the host's LLVM decoder.
pub(crate) fn instruction_address(text: &str) -> Option<u32> {
    let (address, _) = text.trim_start().split_once(':')?;
    u32::from_str_radix(address, 16).ok()
}
#[derive(Debug, Clone)]
pub struct Inspection {
    pub rows: Arc<Vec<Row>>,
    pub source: String,
    pub next: Option<u64>,
    pub lost: u64,
    pub error: Option<(io::ErrorKind, String)>,
    pub gpu: Option<GpuActivity>,
    pub draws: Option<Arc<Vec<DrawHeader>>>,
    pub draw_artifact: Option<DrawArtifact>,
    pub(crate) draw_textures: Vec<DrawTexture>,
    #[cfg(feature = "silvestris-ui")]
    pub(crate) breakpoints: Option<Arc<BreakpointList>>,
    #[cfg(feature = "silvestris-ui")]
    pub(crate) instruction_width: Option<u32>,
}

#[cfg(feature = "silvestris-ui")]
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct BreakpointList {
    pub(crate) breakpoints: Vec<BreakpointEntry>,
    pub(crate) execution_mode: Option<String>,
    pub(crate) watchpoints: Option<String>,
    pub(crate) maximum: Option<usize>,
}

#[cfg(feature = "silvestris-ui")]
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct BreakpointEntry {
    pub(crate) id: u64,
    pub(crate) address: u32,
    pub(crate) length: u32,
    pub(crate) kind: lotus_protocol::debug::BreakpointKind,
    pub(crate) hits: Option<u64>,
    pub(crate) host_configuration: Option<BreakpointSettings>,
}

#[cfg(feature = "silvestris-ui")]
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct BreakpointSettings {
    pub(crate) process: Option<u32>,
    pub(crate) hit_after: u64,
    pub(crate) condition: Option<lotus_protocol::debug::Condition>,
    pub(crate) log: Option<String>,
}

/// An immutable artifact from a completed GpuDraw request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrawArtifact {
    pub(crate) target: SessionTarget,
    pub(crate) draw: u64,
    pub(crate) uri: String,
}

#[derive(Debug, Clone)]
pub(crate) struct DrawTexture {
    pub(crate) index: usize,
    pub(crate) label: String,
    pub(crate) unavailable: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct DrawHeader {
    pub draw: u64,
    pub command: Option<u64>,
    /// Number of retained vertex inputs reported by GpuDraws.
    pub vertices: Option<u32>,
    pub snapshot: Option<bool>,
}

impl DrawHeader {
    pub(crate) fn status(&self) -> &'static str {
        match self.snapshot {
            Some(true) => "Snapshot",
            Some(false) => "Metadata only",
            None => "Unknown",
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct GpuActivity {
    pub command_path: String,
    pub capture: lotus_protocol::debug::GpuCapture,
    pub counters: Option<GpuCounters>,
    pub lost_draws: Option<u64>,
    pub lost_vertices: Option<u64>,
    pub lost_resource_versions: Option<u64>,
    pub snapshots_taken: Option<u64>,
    pub snapshots_skipped_budget: Option<u64>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct GpuCounters {
    pub fast_path_commands: u64,
    pub command_lists: u64,
    pub draw_calls: u64,
}
const ROW_CAP: usize = 100_000;
const TEXT_BUDGET: usize = 32 << 20;
const SNAPSHOT_ROW_CAP: usize = 8192;
const EVENT_PAGE_SIZE: usize = 128;
const DRAW_ROW_CAP: usize = 4096;
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

fn json_preview(value: &Value, limit: usize) -> String {
    // The slice writer stops serialization at the display limit, including
    // nested data in a capture. A whole artifact need not be encoded again.
    let mut buffer = vec![0; limit];
    let mut remaining = buffer.as_mut_slice();
    let truncated = serde_json::to_writer(&mut remaining, value).is_err();
    let written = limit - remaining.len();
    buffer.truncate(written);
    if !truncated {
        return String::from_utf8(buffer).expect("JSON is UTF-8");
    }
    if let Err(incomplete) = std::str::from_utf8(&buffer) {
        buffer.truncate(incomplete.valid_up_to());
    }
    let mut preview = String::from_utf8(buffer).expect("JSON preview ends at a UTF-8 boundary");
    preview.push('…');
    preview
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
        let mut draw_headers = None;
        let mut budget = TEXT_BUDGET;
        let mut omitted = 0;
        fn flatten(
            value: &Value,
            key: String,
            rows: &mut Vec<Row>,
            budget: &mut usize,
            depth: u32,
        ) {
            if rows.len() >= SNAPSHOT_ROW_CAP || *budget < 2048 {
                return;
            }
            if depth < 10 {
                match value {
                    Value::Object(map) => {
                        for (name, value) in map {
                            if rows.len() >= SNAPSHOT_ROW_CAP || *budget < 2048 {
                                break;
                            }
                            flatten(
                                value,
                                format!("{key}.{}", clipped(name, 256)),
                                rows,
                                budget,
                                depth + 1,
                            );
                        }
                        return;
                    }
                    Value::Array(values) => {
                        for (i, value) in values.iter().enumerate() {
                            if rows.len() >= SNAPSHOT_ROW_CAP || *budget < 2048 {
                                break;
                            }
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
            } else if key.starts_with(".capture.resources[") && key.ends_with(".data_hex") {
                value.as_str().map_or_else(
                    || "Unavailable".into(),
                    |hex| format!("{} encoded bytes", hex.len() / 2),
                )
            } else {
                json_preview(value, 1024)
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
            if let Some(cpsr) = value["cpsr"].as_u64() {
                let mode = if cpsr & 32 != 0 { "Thumb" } else { "ARM" };
                rows.push(Row {
                    lane: "CPU",
                    key: "CPSR".into(),
                    value: format!("0x{cpsr:08x} · {mode}"),
                    action: None,
                });
            }
            if let Some(error) = value["disassembly_error"].as_str() {
                rows.push(Row {
                    lane: "CPU",
                    key: "Disassembly unavailable".into(),
                    value: error.into(),
                    action: None,
                });
            }
            if let Some(assembly) = value["disassembly"]["assembly"].as_str() {
                let pc = value["registers"][15].as_u64();
                for line in assembly
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .take(100)
                {
                    rows.push(Row {
                        lane: if instruction_address(line)
                            .map(u64::from)
                            .is_some_and(|address| Some(address) == pc)
                        {
                            "PC"
                        } else {
                            "CPU"
                        },
                        key: "instruction".into(),
                        value: line.into(),
                        action: None,
                    });
                }
            }
        }
        #[cfg(feature = "silvestris-ui")]
        let breakpoints = (panel == Panel::Breakpoints)
            .then(|| {
                let entries = value["breakpoints"].as_array()?;
                if entries.len() > 64 {
                    return None;
                }
                BreakpointList::deserialize(value).ok().map(Arc::new)
            })
            .flatten();
        if matches!(panel, Panel::Timeline | Panel::Logs | Panel::Ipc) {
            if let Some(events) = value["events"].as_array() {
                for event in events.iter().take(EVENT_PAGE_SIZE) {
                    if panel == Panel::Ipc && event["kind"] != "ipc" {
                        continue;
                    }
                    rows.push(Self::event(event));
                }
                omitted = events.len().saturating_sub(EVENT_PAGE_SIZE) as u64;
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
                let headers: Vec<_> = draws
                    .iter()
                    .take(DRAW_ROW_CAP)
                    .filter_map(|draw| DrawHeader::deserialize(draw).ok())
                    .collect();
                omitted = draws.len().saturating_sub(headers.len()) as u64;
                draw_headers = Some(Arc::new(headers));
            }
        } else {
            // A captured address is metadata. Surface reads guest memory at the
            // time it runs, so a capture must never link to that operation.
            let capture = &value["capture"];
            if let (Some(draw), Some(vertex)) =
                (capture["draw"].as_u64(), capture["vertex"].as_u64())
            {
                let instruction_count = capture["instructions"].as_array().map_or(0, Vec::len);
                let status = match capture["truncated"].as_bool() {
                    Some(true) => "truncated by the worker",
                    Some(false) => "worker trace complete",
                    None => "completion unknown",
                };
                rows.push(Row {
                    lane: "shader",
                    key: format!("Draw {draw} · vertex {vertex}"),
                    value: format!("{instruction_count} instructions · {status}"),
                    action: None,
                });
            }
            if let Some(draw) = capture["draw"]
                .as_u64()
                .filter(|_| capture.get("vertex").is_none())
            {
                let vertex_count = capture["vertex_count"].as_u64().map_or_else(
                    || "Primitive vertex count unknown".into(),
                    |count| format!("{count} primitive vertices"),
                );
                rows.push(Row {
                    lane: "GPU",
                    key: format!("Draw {draw}"),
                    value: vertex_count,
                    action: None,
                });
                for (name, field) in [
                    ("Registers", "registers"),
                    ("Float uniforms", "uniforms_f32_bits"),
                    ("Boolean uniforms", "bool_uniforms"),
                    ("Vertex program", "vertex_program"),
                    ("Swizzle words", "swizzle"),
                    ("Saved resources", "resources"),
                ] {
                    let value = capture[field].as_array().map_or_else(
                        || "Unavailable in this capture".into(),
                        |values| format!("{} entries", values.len()),
                    );
                    rows.push(Row {
                        lane: "GPU",
                        key: name.into(),
                        value,
                        action: None,
                    });
                }
                if capture["resource_budget_exhausted"] == true {
                    rows.push(Row {
                        lane: "GPU",
                        key: "Resources incomplete".into(),
                        value: "The worker exhausted the resource capture budget".into(),
                        action: None,
                    });
                }
            }
            if let (Some(draw), Some(vertices)) =
                (capture["draw"].as_u64(), capture["vertices"].as_array())
            {
                // The worker retains at most eight shader inputs per draw.
                for (index, input) in vertices.iter().take(8).enumerate() {
                    if input["input_f32_bits"].is_array() {
                        rows.push(Row {
                            lane: "shader",
                            key: format!("Vertex {index}"),
                            value: "Trace captured vertex input".into(),
                            action: Some(D::ShaderTrace {
                                draw,
                                vertex: index as u32,
                            }),
                        });
                    }
                }
                if !rows.iter().any(|row| row.action.is_some()) && capture["registers"].is_array() {
                    rows.push(Row {
                        lane: "shader",
                        key: "Vertex inputs".into(),
                        value: "Unavailable · capture with hardware shaders disabled".into(),
                        action: None,
                    });
                }
            }
            flatten(value, String::new(), &mut rows, &mut budget, 0);
            if rows.len() == SNAPSHOT_ROW_CAP || budget < 2048 {
                rows.insert(
                    0,
                    Row {
                        lane: "state",
                        key: "Display truncated".into(),
                        value: "The inspector reached its display limit".into(),
                        action: None,
                    },
                );
            }
        }
        let draw_textures = if value["capture"]["registers"].is_array() {
            value["capture"]["resources"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
                .take(32)
                .filter(|(_, resource)| resource["kind"] == "texture")
                .map(|(index, resource)| DrawTexture {
                    index,
                    label: format!(
                        "Texture {} · {}×{}",
                        crate::lotus_window::hex_string(&resource["address"]),
                        resource["width"],
                        resource["height"]
                    ),
                    unavailable: crate::pica_texture::Layout::from_resource(resource)
                        .and_then(|layout| layout.checked_hex(resource))
                        .err()
                        .map(|err| err.to_string()),
                })
                .collect()
        } else {
            Vec::new()
        };
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
            lost: value["lost_total"]
                .as_u64()
                .or_else(|| value["lost_draws"].as_u64())
                .unwrap_or(0)
                .saturating_add(omitted),
            error: None,
            gpu: if panel == Panel::Gpu {
                GpuActivity::deserialize(value).ok()
            } else {
                None
            },
            draws: draw_headers,
            draw_artifact: None,
            draw_textures,
            #[cfg(feature = "silvestris-ui")]
            breakpoints,
            #[cfg(feature = "silvestris-ui")]
            instruction_width: value["disassembly"]["thumb"].as_bool().map(|thumb| {
                if thumb {
                    2
                } else {
                    4
                }
            }),
        }
    }

    pub(crate) fn failed(error: io::Error) -> Self {
        Self {
            rows: Arc::default(),
            source: String::new(),
            next: None,
            lost: 0,
            error: Some((error.kind(), error.to_string())),
            gpu: None,
            draws: None,
            draw_artifact: None,
            draw_textures: Vec::new(),
            #[cfg(feature = "silvestris-ui")]
            breakpoints: None,
            #[cfg(feature = "silvestris-ui")]
            instruction_width: None,
        }
    }
    pub(crate) fn compare_draws(first: &Value, second: &Value, epoch: u64) -> Self {
        let a = &first["draw"];
        let b = &second["draw"];
        let changes = Self::draw_diff(first, second);
        let mut rows = vec![Row {
            lane: "GPU",
            key: format!("A {a} → B {b}"),
            value: match changes.len() {
                0 => "No differences in available captured state".into(),
                128 => "Showing up to 128 changes and unavailable fields".into(),
                _ => "Captured state differences and unavailable fields".into(),
            },
            action: None,
        }];
        rows.extend(changes.into_iter().map(|change| {
            let (key, value) = change.split_once(": ").unwrap_or(("", &change));
            Row {
                lane: "diff",
                key: key.into(),
                value: value.into(),
                action: None,
            }
        }));
        Self {
            rows: Arc::new(rows),
            source: format!("Captured draws {a} and {b} · epoch {epoch}"),
            next: None,
            lost: 0,
            error: None,
            gpu: None,
            draws: None,
            draw_artifact: None,
            draw_textures: Vec::new(),
            #[cfg(feature = "silvestris-ui")]
            breakpoints: None,
            #[cfg(feature = "silvestris-ui")]
            instruction_width: None,
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
            let a = before[key].as_array();
            let b = after[key].as_array();
            if let (Some(a), Some(b)) = (a, b) {
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
                            json_preview(a.get(i).unwrap_or(&Value::Null), 256),
                            json_preview(b.get(i).unwrap_or(&Value::Null), 256)
                        ));
                        if changes.len() == 128 {
                            return changes;
                        }
                    }
                }
            } else {
                let missing = match (a, b) {
                    (None, None) => "A and B",
                    (None, Some(_)) => "A",
                    (Some(_), None) => "B",
                    (Some(_), Some(_)) => unreachable!(),
                };
                changes.push(format!("{key}: Unavailable for {missing}"));
                if changes.len() == 128 {
                    return changes;
                }
            }
        }
        changes
    }
    fn event(event: &Value) -> Row {
        Row {
            lane: lane(event),
            key: format!(
                "#{} · tick {}",
                json_preview(&event["sequence"], 24),
                json_preview(&event["guest_ticks"], 24)
            ),
            value: json_preview(event, 1024),
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
            gpu: None,
            draws: None,
            draw_artifact: None,
            draw_textures: Vec::new(),
            #[cfg(feature = "silvestris-ui")]
            breakpoints: None,
            #[cfg(feature = "silvestris-ui")]
            instruction_width: None,
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
    fn live_inspections_bound_retained_rows_and_json_previews() {
        let draw = serde_json::json!({"draw": 42, "command": 11, "vertices": 8});
        let gpu = Inspection::from_value(
            &serde_json::json!({"draws": vec![draw; DRAW_ROW_CAP + 1], "lost_draws": 3}),
            Panel::Gpu,
        );
        let draws = gpu.draws.unwrap();
        assert_eq!(draws.len(), DRAW_ROW_CAP);
        assert_eq!(gpu.lost, 4);
        assert_eq!(draws[0].draw, 42);
        assert_eq!(draws[0].status(), "Unknown");

        let event = serde_json::json!({"sequence": u64::from(u32::MAX) + 1});
        let events = Inspection::from_value(
            &serde_json::json!({"events": vec![event; 1024], "next": u64::from(u32::MAX) + 1}),
            Panel::Timeline,
        );
        assert_eq!(events.rows.len(), EVENT_PAGE_SIZE);
        assert_eq!(events.lost, (1024 - EVENT_PAGE_SIZE) as u64);
        assert_eq!(events.next, Some(u64::from(u32::MAX) + 1));
        assert_eq!(json_preview(&serde_json::json!("αβ"), 4), "\"α…");
        assert_eq!(json_preview(&serde_json::json!("αβ"), 6), "\"αβ\"");
    }
    #[test]
    fn a_frozen_resource_cannot_open_a_live_surface() {
        let view = Inspection::from_value(
            &serde_json::json!({"capture":{"draw": 42, "resources": [{
                "address": 1234, "width": 8, "height": 8, "format": 0, "data_hex": "abc123"
            }]}}),
            Panel::Result,
        );
        assert!(view.rows.iter().all(|row| row.action.is_none()));
        assert!(view.rows.iter().any(|row| row.value == "3 encoded bytes"));
    }
    #[test]
    fn shader_trace_actions_identify_each_retained_input() {
        let view = Inspection::from_value(
            &serde_json::json!({"capture":{"draw": 42, "vertices": [
                {"input_f32_bits": [[1, 2, 3, 4]]},
                {},
                {"input_f32_bits": [[4, 3, 2, 1]]}
            ]}}),
            Panel::Result,
        );
        let vertices: Vec<_> = view
            .rows
            .iter()
            .filter_map(|row| match row.action {
                Some(D::ShaderTrace { draw: 42, vertex }) => Some(vertex),
                _ => None,
            })
            .collect();
        assert_eq!(vertices, [0, 2]);
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
        assert!(changes[0].starts_with("registers[1]"));
        assert!(changes
            .iter()
            .any(|change| change == "swizzle: Unavailable for A and B"));
        assert!(!changes.iter().any(|change| change.starts_with("resources")));
    }
}
