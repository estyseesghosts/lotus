//! Source-level breakpoints, stepping and excerpts over a verified ELF binding (lotus#77).
//!
//! Source breakpoints are kept by the host as `file:line` and expanded into worker
//! execute breakpoints (`breakpoint_set`, run under the JIT) at every statement start of
//! that line in the bound build. A new binding re-arms them at the new build's
//! addresses; a guest epoch change retires the worker breakpoints of the old build, so a
//! stale executable address never stays armed. The worker clears its breakpoints on
//! `load` and `reset` (`ctr_worker/control.h`, `debug.Clear()`) but keeps them across
//! `load_state` and replay, which is why the host removes them itself.
//!
//! Source stepping runs single-instruction `step` requests inside the current line and
//! stops at the first `is_stmt` row start of another line. In `over` mode a call runs to
//! its return address with `run_to` (a temporary JIT breakpoint) instead of being
//! stepped. Every result reports the instruction count, the stop's statement status and
//! inline depth; a thread switch, another stop reason or an exhausted budget ends the
//! step with that reason instead of a guessed line.
use crate::debug_image::{validate, DebugImage, SymbolSlot};
use crate::native_session::NativeSession;
use lotus_debugger::symbols::ReturnAddress;
use lotus_protocol::debug::{BreakpointKind, DebugCommand as D, SourceStepMode};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug, Clone)]
struct Site {
    address: u32,
    thumb: bool,
    breakpoint: u64,
}

#[derive(Debug, Clone)]
struct Armed {
    epoch: u64,
    generation: u64,
    path: String,
    sites: Vec<Site>,
}

#[derive(Debug)]
struct SourceBreakpoint {
    file: String,
    line: u32,
    armed: Option<Armed>,
    /// Why the breakpoint is not armed.
    pending: Option<String>,
}

/// One session's source breakpoints.
#[derive(Debug, Default)]
pub(crate) struct SourceBreakpoints {
    next: u64,
    entries: BTreeMap<u64, SourceBreakpoint>,
}

impl SourceBreakpoints {
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl SourceBreakpoint {
    fn describe(&self, id: u64) -> Value {
        let mut value = json!({"id": id, "file": self.file, "line": self.line});
        match (&self.armed, &self.pending) {
            (Some(armed), _) => {
                value["state"] = "armed".into();
                value["path"] = armed.path.clone().into();
                value["epoch"] = armed.epoch.into();
                value["generation"] = armed.generation.into();
                value["sites"] = armed
                    .sites
                    .iter()
                    .map(|s| json!({"address": s.address, "thumb": s.thumb, "breakpoint": s.breakpoint}))
                    .collect();
                value["ambiguous"] = (armed.sites.len() > 1).into();
            }
            (None, reason) => {
                value["state"] = "pending".into();
                value["reason"] = reason.clone().into();
            }
        }
        value
    }
}

/// Set worker execute breakpoints at every statement start of `file:line`.
async fn arm(
    native: &mut NativeSession,
    image: (&lotus_debugger::symbols::Symbols, u64, u64),
    file: &str,
    line: u32,
) -> io::Result<Armed> {
    let (index, epoch, generation) = image;
    let (source, statements) = index.source_statements(file, line)?;
    let path = source.path.clone();
    let mut sites = Vec::new();
    for (address, thumb) in statements {
        let set = native
            .debug(&D::BreakpointSet {
                address,
                length: if thumb { 2 } else { 4 },
                kind: BreakpointKind::Execute,
            })
            .await;
        match set.and_then(|reply| {
            reply["id"]
                .as_u64()
                .ok_or_else(|| io::Error::other("breakpoint_set reply has no id"))
        }) {
            Ok(breakpoint) => sites.push(Site {
                address,
                thumb,
                breakpoint,
            }),
            Err(error) => {
                disarm(native, &sites).await;
                return Err(error);
            }
        }
    }
    Ok(Armed {
        epoch,
        generation,
        path,
        sites,
    })
}

/// Remove worker breakpoints; ones the worker already dropped (load, reset, manual
/// removal) are ignored.
async fn disarm(native: &mut NativeSession, sites: &[Site]) {
    for site in sites {
        let _ = native
            .debug(&D::BreakpointRemove {
                id: site.breakpoint,
            })
            .await;
    }
}

fn image_parts(image: &DebugImage) -> (&lotus_debugger::symbols::Symbols, u64, u64) {
    (&image.index, image.epoch, image.generation())
}

/// The validated binding, or why there is none. The caller holds the native lock, so
/// the binding cannot change; the `Arc` lets awaits run without the slot lock.
pub(crate) async fn bound(
    slot: &Mutex<SymbolSlot>,
    native: &mut NativeSession,
) -> io::Result<Arc<DebugImage>> {
    validate(slot, native).await?;
    let slot = lock(slot);
    slot.image.clone().ok_or_else(|| slot.missing())
}

pub(crate) async fn set(
    slot: &Mutex<SymbolSlot>,
    registry: &Mutex<SourceBreakpoints>,
    native: &mut NativeSession,
    file: &str,
    line: u32,
) -> io::Result<Value> {
    let image = bound(slot, native).await?;
    let armed = arm(native, image_parts(&image), file, line).await?;
    let mut registry = lock(registry);
    registry.next += 1;
    let id = registry.next;
    let entry = SourceBreakpoint {
        file: file.to_string(),
        line,
        armed: Some(armed),
        pending: None,
    };
    let mut reply = entry.describe(id);
    reply["image"] = image.describe();
    registry.entries.insert(id, entry);
    Ok(reply)
}

pub(crate) async fn remove(
    registry: &Mutex<SourceBreakpoints>,
    native: &mut NativeSession,
    id: u64,
) -> io::Result<Value> {
    let entry = lock(registry)
        .entries
        .remove(&id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "unknown source breakpoint"))?;
    if let Some(armed) = entry.armed.filter(|armed| armed.epoch == native.epoch()) {
        disarm(native, &armed.sites).await;
    }
    Ok(json!({"removed": id}))
}

pub(crate) async fn list(
    registry: &Mutex<SourceBreakpoints>,
    native: &mut NativeSession,
) -> io::Result<Value> {
    let worker = native.debug(&D::Breakpoints).await?;
    let present: Vec<u64> = worker["breakpoints"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|bp| bp["id"].as_u64())
        .collect();
    let epoch = native.epoch();
    let registry = lock(registry);
    let entries: Vec<Value> = registry
        .entries
        .iter()
        .map(|(id, entry)| {
            let mut value = entry.describe(*id);
            if let Some(armed) = &entry.armed {
                let missing = armed
                    .sites
                    .iter()
                    .filter(|site| armed.epoch != epoch || !present.contains(&site.breakpoint))
                    .count();
                if missing > 0 {
                    value["state"] = "incomplete".into();
                    value["missing_sites"] = missing.into();
                }
            }
            value
        })
        .collect();
    Ok(json!({"source_breakpoints": entries, "epoch": epoch}))
}

/// Re-arm every source breakpoint for a new binding. Called with the guest paused.
pub(crate) async fn rearm(
    slot: &Mutex<SymbolSlot>,
    registry: &Mutex<SourceBreakpoints>,
    native: &mut NativeSession,
) -> Value {
    let Some(image) = lock(slot).image.clone() else {
        return Value::Array(Vec::new());
    };
    let ids: Vec<u64> = lock(registry).entries.keys().copied().collect();
    let mut results = Vec::new();
    for id in ids {
        let Some((file, line, previous)) = lock(registry)
            .entries
            .get_mut(&id)
            .map(|entry| (entry.file.clone(), entry.line, entry.armed.take()))
        else {
            continue;
        };
        if let Some(previous) = previous.filter(|p| p.epoch == native.epoch()) {
            disarm(native, &previous.sites).await;
        }
        let armed = arm(native, image_parts(&image), &file, line).await;
        let mut registry = lock(registry);
        let Some(entry) = registry.entries.get_mut(&id) else {
            continue;
        };
        match armed {
            Ok(armed) => {
                entry.armed = Some(armed);
                entry.pending = None;
            }
            Err(error) => entry.pending = Some(error.to_string()),
        }
        results.push(entry.describe(id));
    }
    Value::Array(results)
}

/// After a guest epoch change: remove the old build's worker breakpoints and mark every
/// source breakpoint pending until the next binding.
pub(crate) async fn retire(registry: &Mutex<SourceBreakpoints>, native: &mut NativeSession) {
    let epoch = native.epoch();
    let stale: Vec<Site> = {
        let mut registry = lock(registry);
        let mut stale = Vec::new();
        for entry in registry.entries.values_mut() {
            if entry
                .armed
                .as_ref()
                .is_some_and(|armed| armed.epoch != epoch)
            {
                let armed = entry.armed.take().expect("checked");
                stale.extend(armed.sites);
                entry.pending = Some(format!(
                    "guest epoch {epoch} replaced the armed build; load the matching ELF to re-arm"
                ));
            }
        }
        stale
    };
    disarm(native, &stale).await;
}

/// Record why unarmed source breakpoints stay pending.
pub(crate) fn defer(registry: &Mutex<SourceBreakpoints>, reason: &str) {
    for entry in lock(registry).entries.values_mut() {
        if entry.armed.is_none() {
            entry.pending = Some(reason.to_string());
        }
    }
}

/// ARM ARM (DDI 0100I) A4.1.10 BL, A4.1.12 BLX(1) and BLX(2); Thumb BL/BLX pairs and
/// BLX(2) (A7.1.18, A7.1.25). Returns the return address of a call at `pc`.
fn call_return(pc: u32, thumb: bool, bytes: &[u8]) -> Option<u32> {
    let word = u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?);
    if thumb {
        let first = word & 0xffff;
        let second = word >> 16;
        if first & 0xf800 == 0xf000 && matches!(second & 0xd000, 0xd000 | 0xc000) {
            return Some(pc + 4);
        }
        (first & 0xff87 == 0x4780).then_some(pc + 2)
    } else {
        let bl = word >> 28 != 0xf && word & 0x0f00_0000 == 0x0b00_0000;
        let blx_immediate = word & 0xfe00_0000 == 0xfa00_0000;
        let blx_register = word >> 28 != 0xf && word & 0x0fff_fff0 == 0x012f_ff30;
        (bl || blx_immediate || blx_register).then_some(pc + 4)
    }
}

struct Pc {
    pc: u32,
    sp: u32,
    lr: u32,
    thumb: bool,
    thread: Value,
}

async fn pc(native: &mut NativeSession, core: u32) -> io::Result<Pc> {
    let registers = native.debug(&D::Registers { core }).await?;
    let pc = registers["registers"][15]
        .as_u64()
        .ok_or_else(|| io::Error::other("registers reply has no pc"))? as u32;
    let cpsr = registers["cpsr"]
        .as_u64()
        .ok_or_else(|| io::Error::other("registers reply has no cpsr"))?;
    let sp = registers["registers"][13]
        .as_u64()
        .ok_or_else(|| io::Error::other("registers reply has no sp"))? as u32;
    let lr = registers["registers"][14]
        .as_u64()
        .ok_or_else(|| io::Error::other("registers reply has no lr"))? as u32;
    Ok(Pc {
        pc,
        sp,
        lr,
        thumb: cpsr & 0x20 != 0,
        thread: registers["thread"].clone(),
    })
}

/// Wait for the worker's next `stopped` notification after a resume.
async fn stopped(
    events: &mut tokio::sync::broadcast::Receiver<Value>,
    native: &mut NativeSession,
) -> io::Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, events.recv()).await {
            Err(_) => {
                native.pause().await?;
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "guest did not stop within 25 s; paused it",
                ));
            }
            Ok(Ok(event)) if event["data"]["kind"] == "stopped" => {
                native.status().await?;
                return Ok(event["data"].clone());
            }
            Ok(Ok(_)) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                let status = native.status().await?;
                if status["paused"] == true && status["stop"].is_object() {
                    return Ok(status["stop"].clone());
                }
            }
            Ok(Err(error)) => return Err(io::Error::other(error)),
        }
    }
}

/// What a stop shows. At the entry of inlined subroutines the inlined frames are hidden
/// and the stop is shown at their call site in the caller, like gdb's
/// `skip_inline_frames`; `hidden` says how many are hidden.
#[derive(Debug, Clone)]
struct View {
    file: String,
    line: u32,
    column: u32,
    depth: usize,
    hidden: u32,
    statement: bool,
}

/// The view at `pc`, hiding `hidden` inlined frames (default: all that begin at `pc`).
fn view(
    index: &lotus_debugger::symbols::Symbols,
    pc: u32,
    hidden: Option<u32>,
) -> io::Result<Option<View>> {
    let hidden = hidden.unwrap_or_else(|| index.inline_entries_at(pc));
    let Some((file, at)) = index.line_row(pc) else {
        return Ok(None);
    };
    if hidden == 0 {
        return Ok(Some(View {
            file: file.path.clone(),
            line: at.line,
            column: at.column,
            depth: index.inline_depth(pc)?,
            hidden,
            statement: at.statement_start,
        }));
    }
    // A call site is where execution of its line's inlined call begins.
    Ok(index.frame_view(pc, hidden)?.map(|frame| View {
        file: frame.file,
        line: frame.line,
        column: frame.column,
        depth: frame.depth,
        hidden,
        statement: true,
    }))
}

/// Where `pc` is in source. `certainty` says how much the line table supports: a
/// statement start, an inlined call site, an address inside a statement, or no line
/// information at all.
fn location(
    index: &lotus_debugger::symbols::Symbols,
    pc: u32,
    hidden: Option<u32>,
) -> io::Result<Value> {
    let row = index.line_row(pc);
    let shown = view(index, pc, hidden)?;
    let certainty = match (&shown, row) {
        (Some(shown), _) if shown.hidden > 0 => "inline_call_site",
        (_, Some((_, at))) if at.statement_start => "statement_start",
        (_, Some(_)) => "inside_statement",
        _ => "no_line_information",
    };
    Ok(json!({
        "pc": pc,
        "thumb": index.thumb_at(pc),
        "file": shown.as_ref().map(|v| v.file.clone()),
        "line": shown.as_ref().map(|v| v.line),
        "column": shown.as_ref().map(|v| v.column).filter(|column| *column != 0),
        "discriminator": row.map(|(_, at)| at.discriminator).filter(|d| *d != 0),
        "range": row.map(|(_, at)| [at.range.0, at.range.1]),
        "certainty": certainty,
        "function": index.function_at(pc).map(|(_, _, name)| name.to_string()),
        "inline_depth": shown.as_ref().map(|v| v.depth),
        "hidden_inline_frames": shown.as_ref().map_or(0, |v| v.hidden),
    }))
}

/// A stop's inline view, kept so the next source step starts from what was shown.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Presented {
    epoch: u64,
    pc: u32,
    /// The worker's stop sequence; any execution or new stop replaces it.
    sequence: Value,
    hidden: u32,
}

/// Inlined frames the last source step hid at this stop, so variable inspection counts
/// frames as that step's location showed them; 0 for any other stop.
pub(crate) fn presented_hidden(
    slot: &Mutex<SymbolSlot>,
    epoch: u64,
    pc: u32,
    sequence: &Value,
) -> u32 {
    lock(slot)
        .presented
        .as_ref()
        .filter(|p| p.epoch == epoch && p.pc == pc && &p.sequence == sequence)
        .map_or(0, |p| p.hidden)
}

pub(crate) async fn step(
    slot: &Mutex<SymbolSlot>,
    native: &mut NativeSession,
    core: u32,
    mode: SourceStepMode,
    max_instructions: u32,
    check: &(dyn Fn() -> io::Result<()> + Sync),
) -> io::Result<Value> {
    let image = bound(slot, native).await?;
    let epoch = native.epoch();
    let before = native.status().await?["stop"]["sequence"].clone();
    let start = pc(native, core).await?;
    let shown = lock(slot)
        .presented
        .as_ref()
        .filter(|p| p.epoch == epoch && p.pc == start.pc && p.sequence == before)
        .map(|p| p.hidden);
    let result = walk(
        &image,
        native,
        core,
        mode,
        max_instructions,
        check,
        start,
        shown,
    )
    .await?;
    let after = native.status().await?["stop"]["sequence"].clone();
    let location = &result["location"];
    lock(slot).presented = location["pc"].as_u64().map(|pc| Presented {
        epoch,
        pc: pc as u32,
        sequence: after,
        hidden: location["hidden_inline_frames"].as_u64().unwrap_or(0) as u32,
    });
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
async fn walk(
    image: &DebugImage,
    native: &mut NativeSession,
    core: u32,
    mode: SourceStepMode,
    max_instructions: u32,
    check: &(dyn Fn() -> io::Result<()> + Sync),
    start: Pc,
    shown: Option<u32>,
) -> io::Result<Value> {
    let index = &image.index;
    let Some(from) = view(index, start.pc, shown)? else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "no DWARF line information at {:#010x}; use instruction stepping",
                start.pc
            ),
        ));
    };
    let start_function = index.function_at(start.pc).map(|(s, e, _)| (s, e));
    let start_pc = start.pc;
    let epoch = native.epoch();
    let mut instructions = 0u32;
    let mut calls = 0u32;
    let finish =
        |reason: &str, at: &Pc, hidden: Option<u32>, instructions: u32, calls: u32, stop: Value| {
            let mut result = json!({
                "reason": reason, "mode": mode, "core": core, "thread": at.thread,
                "epoch": epoch,
                "start": {"file": from.file, "line": from.line, "pc": start_pc,
                          "inline_depth": from.depth},
                "instructions": instructions, "calls_stepped_over": calls,
                "stop": stop, "image": image.describe(),
            });
            result["location"] = location(index, at.pc, hidden)?;
            Ok::<_, io::Error>(result)
        };
    // Into from an inlined call site enters the inlined frame without executing, as
    // gdb's `step_into_inline_frame` does.
    if mode == SourceStepMode::Into && from.hidden > 0 {
        return finish(
            "entered_inline",
            &start,
            Some(from.hidden - 1),
            0,
            0,
            Value::Null,
        );
    }
    // Out of a concrete function runs to the return address that `.debug_frame` gives,
    // checking SP against the CFA; without call-frame information it uses the worker's
    // backtrace-based `step_out`. Out of an inlined frame steps until the depth drops.
    if mode == SourceStepMode::Out && from.depth == 0 {
        if let Some(rule) = index.frame_rule(start.pc)? {
            let registers = native.debug(&D::Registers { core }).await?;
            let register = |number: u16| {
                registers["registers"][usize::from(number)]
                    .as_u64()
                    .filter(|_| number < 16)
                    .map(|value| value as u32)
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::Unsupported,
                            format!("call-frame rule uses register {number}"),
                        )
                    })
            };
            let cfa = (i64::from(register(rule.cfa_register)?) + rule.cfa_offset) as u32;
            let target = match rule.return_address {
                ReturnAddress::Register(number) => register(number)?,
                ReturnAddress::Saved(offset) => {
                    let reply = native
                        .debug(&D::ReadMemory {
                            address: (i64::from(cfa) + offset) as u32,
                            length: 4,
                        })
                        .await?;
                    let bytes: Vec<u8> =
                        serde_json::from_value(reply["bytes"].clone()).map_err(io::Error::other)?;
                    u32::from_le_bytes(bytes.get(..4).and_then(|b| b.try_into().ok()).ok_or_else(
                        || io::Error::other("short read of the saved return address"),
                    )?)
                }
            };
            let mut result = match return_to(native, core, target & !1, cfa).await? {
                Ok(stop) => {
                    let now = pc(native, core).await?;
                    finish("returned", &now, None, 0, 0, stop)?
                }
                Err((at, stop)) => finish("stopped", &at, None, 0, 0, stop)?,
            };
            result["unwind"] = "debug_frame".into();
            return Ok(result);
        }
        let mut events = native.subscribe();
        let reply = native.debug(&D::StepOut { core }).await?;
        let stop = stopped(&mut events, native).await?;
        let now = pc(native, core).await?;
        let reason = if stop["reason"] == "breakpoint"
            && stop["breakpoint"] == reply["temporary_breakpoint"]
        {
            "returned"
        } else {
            "stopped"
        };
        let mut result = finish(reason, &now, None, 0, 0, stop)?;
        result["unwind"] = "worker_backtrace".into();
        return Ok(result);
    }
    let mut current = start;
    // Into stopped at a function entry: (post-prologue target, function start, end).
    let mut prologue: Option<(u32, u32, u32)> = None;
    loop {
        check()?;
        if instructions >= max_instructions {
            return finish("budget", &current, None, instructions, calls, Value::Null);
        }
        let bytes = native
            .debug(&D::ReadMemory {
                address: current.pc,
                length: 4,
            })
            .await
            .ok()
            .and_then(|r| serde_json::from_value::<Vec<u8>>(r["bytes"].clone()).ok())
            .unwrap_or_default();
        let call = call_return(current.pc, current.thumb, &bytes);
        instructions += 1;
        let mut stop = match (mode, call) {
            (SourceStepMode::Over | SourceStepMode::Out, Some(address)) => {
                calls += 1;
                match return_to(native, core, address, current.sp).await? {
                    Ok(stop) => stop,
                    Err((at, stop)) => {
                        return finish("stopped", &at, None, instructions, calls, stop)
                    }
                }
            }
            _ => {
                let reply = native.debug(&D::Step { core, count: 1 }).await?;
                let stop = reply["stop"].clone();
                if stop["reason"] != "step" || reply["instructions"] != 1 {
                    let now = pc(native, core).await?;
                    return finish("stopped", &now, None, instructions, calls, stop);
                }
                stop
            }
        };
        let mut next = pc(native, core).await?;
        // gdb's `step` only enters functions that have line information.
        if let (SourceStepMode::Into, Some(address)) = (mode, call) {
            if next.thread == current.thread && index.line_at(next.pc).is_none() {
                next = follow_veneer(
                    native,
                    core,
                    index,
                    next,
                    &current,
                    address,
                    &mut instructions,
                )
                .await?;
            }
            if next.thread == current.thread && index.line_at(next.pc).is_none() {
                calls += 1;
                stop = match return_to(native, core, address, current.sp).await? {
                    Ok(stop) => stop,
                    Err((at, stop)) => {
                        return finish("stopped", &at, None, instructions, calls, stop)
                    }
                };
                next = pc(native, core).await?;
            }
        }
        if next.thread != current.thread {
            return finish("thread_switch", &next, None, instructions, calls, stop);
        }
        current = next;
        let address = current.pc & !1;
        if let Some((target, low, high)) = prologue {
            if address == target {
                return finish("statement", &current, None, instructions, calls, stop);
            }
            if (low..high).contains(&address) {
                continue;
            }
            prologue = None;
        }
        let Some(here) = view(index, current.pc, None)? else {
            continue;
        };
        if mode == SourceStepMode::Out {
            if here.depth < from.depth {
                return finish("returned", &current, None, instructions, calls, stop);
            }
            continue;
        }
        if !here.statement {
            continue;
        }
        let same_line = here.file == from.file && here.line == from.line;
        let in_start = start_function.is_some_and(|(s, e)| address >= s && address < e);
        match mode {
            SourceStepMode::Into => {
                // Stepping from a call site into its inlined body shows the inlined frame.
                if here.hidden > 0 && same_line && in_start {
                    let hidden = Some(here.hidden - 1);
                    return finish("statement", &current, hidden, instructions, calls, stop);
                }
                if same_line && in_start {
                    continue;
                }
                if let (Some(target), Some((low, high, _))) = (
                    index.after_prologue(current.pc),
                    index.function_at(current.pc),
                ) {
                    prologue = Some((target, low, high));
                    continue;
                }
            }
            SourceStepMode::Over => {
                if in_start && (same_line || here.depth > from.depth) {
                    continue;
                }
            }
            SourceStepMode::Out => unreachable!("handled above"),
        }
        return finish("statement", &current, None, instructions, calls, stop);
    }
}

/// Linker veneers (GNU ld `elf32-arm.c` stub templates, at most a few instructions)
/// forward a call without touching SP or LR. Follow such code from `next` into a callee
/// with line information; stop at anything that builds a frame or calls.
async fn follow_veneer(
    native: &mut NativeSession,
    core: u32,
    index: &lotus_debugger::symbols::Symbols,
    mut next: Pc,
    caller: &Pc,
    address: u32,
    instructions: &mut u32,
) -> io::Result<Pc> {
    for _ in 0..8 {
        if index.line_at(next.pc).is_some()
            || next.sp != caller.sp
            || next.lr & !1 != address & !1
            || next.thread != caller.thread
        {
            break;
        }
        let bytes = native
            .debug(&D::ReadMemory {
                address: next.pc,
                length: 4,
            })
            .await?;
        let bytes = serde_json::from_value::<Vec<u8>>(bytes["bytes"].clone()).unwrap_or_default();
        if call_return(next.pc, next.thumb, &bytes).is_some() {
            break;
        }
        let reply = native.debug(&D::Step { core, count: 1 }).await?;
        *instructions += 1;
        if reply["stop"]["reason"] != "step" || reply["instructions"] != 1 {
            break;
        }
        next = pc(native, core).await?;
    }
    Ok(next)
}

/// Resume until the call made with stack pointer `sp` returns to `address`. ARM `BL` and
/// `BLX` leave SP unchanged (ARM ARM A4.1.10, A4.1.12), so a hit with another SP is a
/// recursive activation and is resumed again. `Err` carries any other stop.
async fn return_to(
    native: &mut NativeSession,
    core: u32,
    address: u32,
    sp: u32,
) -> io::Result<Result<Value, (Pc, Value)>> {
    loop {
        let mut events = native.subscribe();
        let reply = native.debug(&D::RunTo { core, address }).await?;
        let stop = stopped(&mut events, native).await?;
        let now = pc(native, core).await?;
        if stop["reason"] != "breakpoint" || stop["breakpoint"] != reply["temporary_breakpoint"] {
            return Ok(Err((now, stop)));
        }
        if now.sp == sp {
            return Ok(Ok(stop));
        }
    }
}

/// A local source root and an optional build-time prefix it replaces.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceRoot {
    pub path: PathBuf,
    /// DWARF path prefix recorded at build time; replaced by `path`.
    #[serde(default)]
    pub from: Option<String>,
}

const EXCERPT_FILE_LIMIT: u64 = 4 << 20;

fn local_source(roots: &BTreeMap<String, SourceRoot>, dwarf_path: &str) -> io::Result<PathBuf> {
    let normalized = dwarf_path.replace('\\', "/");
    for root in roots.values() {
        let Ok(base) = root.path.canonicalize() else {
            continue;
        };
        let candidate = match &root.from {
            Some(from) => {
                let from = from.replace('\\', "/");
                let Some(rest) = normalized
                    .strip_prefix(from.trim_end_matches('/'))
                    .filter(|rest| rest.starts_with('/'))
                else {
                    continue;
                };
                base.join(rest.trim_start_matches('/'))
            }
            None => PathBuf::from(dwarf_path),
        };
        if let Ok(path) = candidate.canonicalize() {
            if path.starts_with(&base) && path.is_file() {
                return Ok(path);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("{dwarf_path} is not below a configured source root"),
    ))
}

pub(crate) async fn excerpt(
    slot: &Mutex<SymbolSlot>,
    native: &mut NativeSession,
    roots: &BTreeMap<String, SourceRoot>,
    file: &str,
    line: u32,
    context: u32,
) -> io::Result<Value> {
    let image = bound(slot, native).await?;
    let (dwarf_path, md5, describe) = {
        let source = image.index.source_file(file).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{file} does not name exactly one file of the bound ELF"),
            )
        })?;
        (source.path.clone(), source.md5, image.describe())
    };
    let path = local_source(roots, &dwarf_path)?;
    let bytes = tokio::task::spawn_blocking(move || -> io::Result<Vec<u8>> {
        if std::fs::metadata(&path)?.len() > EXCERPT_FILE_LIMIT {
            return Err(io::Error::other("source file exceeds 4 MiB"));
        }
        std::fs::read(&path)
    })
    .await
    .map_err(io::Error::other)??;
    let checksum = match md5 {
        Some(expected) => {
            use md5::{Digest, Md5};
            if Md5::digest(&bytes).as_slice() != expected {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{dwarf_path} changed since the build: its MD5 differs from the ELF's line table"),
                ));
            }
            "md5_verified"
        }
        None => "not_recorded",
    };
    let text = String::from_utf8_lossy(&bytes);
    let first = line.saturating_sub(context).max(1);
    let lines: Vec<Value> = text
        .lines()
        .enumerate()
        .skip(first as usize - 1)
        .take((line - first + context + 1) as usize)
        .map(|(index, content)| {
            let content: String = content.chars().take(512).collect();
            json!({"line": index + 1, "text": content})
        })
        .collect();
    if !lines.iter().any(|l| l["line"] == line) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{dwarf_path} has fewer than {line} lines"),
        ));
    }
    Ok(
        json!({"file": dwarf_path, "line": line, "lines": lines, "checksum": checksum, "image": describe}),
    )
}
