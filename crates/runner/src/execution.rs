//! Bounded execution of the implemented homebrew/software scenario subset.
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;
use std::time::{Duration, Instant};

use lotus_protocol::envelope::SessionUuid;
use lotus_session::native_session::{InputState, NativeSession};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::assertions::{
    self, assertion_filter, AssertionResult, EventFilter, EventLedger, Observations, Outcome,
};
use crate::{InputEvent, Scenario};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub frame: u64,
    /// `frame`, or the event kind of an event-triggered capture.
    #[serde(default)]
    pub trigger: Option<String>,
    pub screen: String,
    pub sha256: String,
    pub bytes: usize,
    pub artifact: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReport {
    pub name: String,
    pub backend: String,
    pub content_sha256: String,
    pub worker_executable_sha256: String,
    pub source: String,
    pub frames: u64,
    pub checkpoints: Vec<Checkpoint>,
    pub final_status: Value,
    pub host_elapsed_ms: u128,
    pub evidence: String,
    /// Movie status at the end of the run; a recording's `.ctm`/`.json` pair
    /// is retained next to the report.
    #[serde(default)]
    pub movie: Option<Value>,
    /// Scenario schema version.
    #[serde(default = "one")]
    pub schema_version: u32,
    /// Per-assertion results (lotus#94).
    #[serde(default)]
    pub assertions: Vec<AssertionResult>,
    /// True only when every assertion passed.
    #[serde(default)]
    pub passed: bool,
    /// Index of the first assertion that did not pass.
    #[serde(default)]
    pub first_failure: Option<usize>,
    /// Event observation window, when the scenario reads worker events.
    #[serde(default)]
    pub event_window: Option<EventLedger>,
}

fn one() -> u32 {
    1
}

/// SHA-256 without loading an entire executable/content asset into memory.
pub fn file_hash(path: &Path) -> io::Result<String> {
    let mut input = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

/// Reject unsupported promises before creating a worker or an artifact directory.
pub fn validate_execution(scenario: &Scenario) -> io::Result<()> {
    scenario
        .validate()
        .map_err(|error| invalid(error.to_string()))?;
    if scenario
        .profile
        .worker_sha256
        .as_ref()
        .is_some_and(|hash| hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(invalid(
            "profile.worker_sha256 must be a SHA-256 hex digest",
        ));
    }
    if scenario.content.kind != "homebrew_3dsx"
        || !matches!(scenario.profile.model.as_str(), "old3ds" | "new3ds")
        || scenario.profile.renderer != "software"
        || scenario.profile.audio_output != "discard"
        || scenario.profile.deterministic_initial_ticks.unwrap_or(0) != 0
        || scenario.storage.mode != "isolated_copy"
        || scenario.storage.base_snapshot.is_some()
        || scenario.input.owner != "scenario"
    {
        return Err(invalid("executor requires homebrew_3dsx, old3ds|new3ds/software, discard, ticks 0, empty isolated_copy, scenario input"));
    }
    if let Some(movie) = &scenario.movie {
        let named = !movie.name.is_empty()
            && movie.name.len() <= 32
            && movie
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !named || scenario.profile.worker_flavor != crate::WorkerFlavor::Experimental {
            return Err(invalid(
                "movies need a valid name and the experimental worker",
            ));
        }
        match movie.mode.as_str() {
            "record" if movie.path_handle.is_none() => {}
            "play" if movie.path_handle.is_some() && scenario.input.schedule.is_empty() => {}
            _ => {
                return Err(invalid(
                    "movie mode is record, or play with a path_handle and no scenario input",
                ))
            }
        }
    }
    if scenario.input.schedule.len() > 4096 || scenario.captures.len() > 64 {
        return Err(invalid("scenario event/capture count limit"));
    }
    for event in &scenario.input.schedule {
        validate_event(event)?;
    }
    for capture in &scenario.captures {
        let when = capture
            .when
            .as_ref()
            .ok_or_else(|| invalid("capture needs a trigger"))?;
        if when
            .frame
            .is_some_and(|frame| frame == 0 || frame > scenario.stop.max_frames)
            || capture
                .include
                .iter()
                .any(|item| item != "top_raw_frame" && item != "bottom_raw_frame")
            || capture.max_bytes.is_some_and(|bytes| bytes < 691200)
        {
            return Err(invalid(
                "capture requires bounded native top/bottom screens at a frame in 1..=max_frames",
            ));
        }
        if when.event.is_some() && scenario.version < 2 {
            return Err(invalid("event capture triggers need scenario version 2"));
        }
        if when.frame.is_none() && when.event.is_none() {
            return Err(invalid("capture needs a frame or event trigger"));
        }
    }
    if scenario.version < 2
        && scenario
            .assertions
            .iter()
            .any(|assertion| !matches!(assertion.kind.as_str(), "no_worker_crash" | "frame_count"))
    {
        return Err(invalid(
            "version 1 executes only no_worker_crash and frame_count; use scenario version 2",
        ));
    }
    let experimental = scenario.profile.worker_flavor == crate::WorkerFlavor::Experimental;
    let event_capture = scenario.captures.iter().any(|capture| {
        capture
            .when
            .as_ref()
            .is_some_and(|when| when.event.is_some())
    });
    if !experimental
        && (event_capture
            || scenario
                .assertions
                .iter()
                .any(|assertion| assertions::needs_experimental(&assertion.kind)))
    {
        return Err(invalid(
            "event, coverage and event-capture checks need worker_flavor experimental (CITnLO event history and breakpoint counters)",
        ));
    }
    Ok(())
}

fn button_mask(button: &str) -> Option<u32> {
    let index = match button.to_ascii_lowercase().as_str() {
        "a" => 0,
        "b" => 1,
        "x" => 2,
        "y" => 3,
        "up" => 4,
        "down" => 5,
        "left" => 6,
        "right" => 7,
        "l" => 8,
        "r" => 9,
        "start" => 10,
        "select" => 11,
        "zl" => 14,
        "zr" => 15,
        _ => return None,
    };
    Some(1 << index)
}

fn validate_event(event: &InputEvent) -> io::Result<()> {
    if !matches!(event.action.as_str(), "down" | "up" | "press") {
        return Err(invalid("unknown input action"));
    }
    if event.button == "touch" {
        if event.x.is_none_or(|x| x >= 320) || event.y.is_none_or(|y| y >= 240) {
            return Err(invalid("touch requires native x/y coordinates"));
        }
    } else if button_mask(&event.button).is_none() {
        return Err(invalid("unknown button"));
    }
    Ok(())
}

fn apply_input(state: &mut InputState, event: &InputEvent) {
    let pressed = event.action != "up";
    if event.button == "touch" {
        state.x = event.x.expect("validated touch x");
        state.y = event.y.expect("validated touch y");
        state.touch = pressed;
    } else {
        let mask = button_mask(&event.button).expect("validated button");
        if pressed {
            state.buttons |= mask;
        } else {
            state.buttons &= !mask;
        }
    }
}

/// Applies the model/determinism profile and stages the movie before loading.
async fn prepare(
    session: &mut NativeSession,
    scenario: &Scenario,
    directory: &Path,
) -> io::Result<()> {
    use lotus_protocol::debug::{DebugCommand, SettingValue};
    let mut settings = vec![(
        "is_new_3ds",
        SettingValue::Boolean(scenario.profile.model == "new3ds"),
    )];
    if scenario.profile.determinism {
        settings.push((
            "deterministic_async_operations",
            SettingValue::Boolean(true),
        ));
        settings.push(("async_fs_operations", SettingValue::Boolean(false)));
    }
    for (name, value) in settings {
        session
            .debug(&DebugCommand::SettingsSet {
                name: name.into(),
                value,
            })
            .await?;
    }
    let Some(movie) = &scenario.movie else {
        return Ok(());
    };
    if movie.mode == "record" {
        session
            .debug(&DebugCommand::MovieRecord {
                name: movie.name.clone(),
            })
            .await?;
        return Ok(());
    }
    let source = directory
        .join(movie.path_handle.as_deref().expect("validated play path"))
        .canonicalize()?;
    if fs::metadata(&source)?.len() > (32 << 20) + 4096 {
        return Err(invalid("movie exceeds 32 MiB budget"));
    }
    let movies = session.root().join("movies");
    fs::create_dir_all(&movies)?;
    fs::copy(&source, movies.join(format!("{}.ctm", movie.name)))?;
    fs::copy(
        source.with_extension("json"),
        movies.join(format!("{}.json", movie.name)),
    )?;
    session
        .debug(&DebugCommand::MoviePlay {
            name: movie.name.clone(),
            read_only: movie.read_only,
        })
        .await?;
    Ok(())
}

/// Stops the movie; a recording is kept next to the report for later playback.
async fn finish_movie(
    session: &mut NativeSession,
    movie: &crate::Movie,
    output: &Path,
) -> io::Result<Value> {
    use lotus_protocol::debug::DebugCommand;
    let status = session.debug(&DebugCommand::MovieStatus).await?;
    let stopped = session.debug(&DebugCommand::MovieStop).await?;
    if movie.mode == "play" && status["input_index"].as_u64() != status["total_inputs"].as_u64() {
        return Err(io::Error::other(
            "movie playback did not consume every recorded input",
        ));
    }
    if movie.mode == "record" {
        let movies = session.root().join("movies");
        for extension in ["ctm", "json"] {
            let name = format!("{}.{extension}", movie.name);
            fs::copy(movies.join(&name), output.join(&name))?;
        }
    }
    Ok(serde_json::json!({
        "mode": movie.mode,
        "name": movie.name,
        "status": status,
        "stopped": stopped,
    }))
}

/// Filters the run must count: event assertions plus event capture triggers.
fn event_filters(scenario: &Scenario) -> Vec<EventFilter> {
    let mut filters: Vec<EventFilter> = Vec::new();
    let triggers = scenario.captures.iter().filter_map(|capture| {
        let when = capture.when.as_ref()?;
        Some(EventFilter {
            kind: when.event.clone()?,
            text: when.text_contains.clone(),
        })
    });
    for filter in scenario
        .assertions
        .iter()
        .filter_map(assertion_filter)
        .chain(triggers)
    {
        if !filters.contains(&filter) {
            filters.push(filter);
        }
    }
    filters
}

/// Drains the worker's event history into the ledger. The stream returns at most 128 events per
/// page and the history holds 1024 (`debug.h` `Stream`/`Record`), so nine pages always empty it;
/// needing more means the worker is still recording faster than we read, which counts as loss.
async fn drain(
    session: &mut NativeSession,
    ledger: &mut EventLedger,
    frame: u64,
) -> io::Result<()> {
    use lotus_protocol::debug::DebugCommand;
    for _ in 0..9 {
        let reply = session
            .debug(&DebugCommand::Events {
                after: ledger.after,
                limit: 128,
            })
            .await?;
        if !ledger.absorb(&reply, frame) {
            return Ok(());
        }
    }
    ledger
        .loss
        .get_or_insert_with(|| format!("event history did not drain at frame {frame}"));
    Ok(())
}

/// Memory/register reads due at `frame`.
async fn read_values(
    session: &mut NativeSession,
    scenario: &Scenario,
    frame: u64,
    seen: &mut Observations,
) -> io::Result<()> {
    use lotus_protocol::debug::DebugCommand;
    for (index, assertion) in scenario.assertions.iter().enumerate() {
        if assertion.at_frame != Some(frame) {
            continue;
        }
        match assertion.kind.as_str() {
            "memory" => {
                let width = assertion.width.unwrap_or(4);
                let reply = session
                    .debug(&DebugCommand::ReadMemory {
                        address: assertion.address.expect("validated address"),
                        length: width,
                    })
                    .await?;
                let bytes: Vec<u8> = serde_json::from_value(reply["bytes"].clone())
                    .map_err(|_| io::Error::other("read_memory reply has no bytes"))?;
                if bytes.len() != width as usize {
                    return Err(io::Error::other("read_memory returned a short read"));
                }
                let value = bytes
                    .iter()
                    .rev()
                    .fold(0u64, |value, byte| value << 8 | u64::from(*byte));
                seen.values.insert(index, value);
            }
            "register" => {
                let reply = session
                    .debug(&DebugCommand::Registers {
                        core: assertion.core.unwrap_or(0),
                    })
                    .await?;
                let register = assertion.register.expect("validated register");
                // `debug.h` `Registers`: `registers` is cpu_registers r0..r15; cpsr is separate.
                let value = if register == 16 {
                    reply["cpsr"].as_u64()
                } else {
                    reply["registers"][register as usize].as_u64()
                };
                let value =
                    value.ok_or_else(|| io::Error::other("registers reply is incomplete"))?;
                seen.values.insert(index, value);
            }
            _ => {}
        }
    }
    Ok(())
}

async fn capture_screens(
    session: &mut NativeSession,
    output: &Path,
    frame: u64,
    trigger: Option<String>,
    checkpoints: &mut Vec<Checkpoint>,
) -> io::Result<Vec<lotus_session::native_session::Frame>> {
    let frames = session.capture().await?;
    for captured in &frames {
        let prefix = trigger
            .as_ref()
            .map_or(String::new(), |trigger| format!("{trigger}-"));
        let filename = format!("{prefix}{frame}-{}.bgra", captured.lease.screen);
        let hash = format!("{:x}", Sha256::digest(&captured.bgra));
        fs::write(output.join(&filename), &captured.bgra)?;
        checkpoints.push(Checkpoint {
            frame,
            trigger: trigger.clone(),
            screen: captured.lease.screen.clone(),
            sha256: hash,
            bytes: captured.bgra.len(),
            artifact: filename,
        });
    }
    Ok(frames)
}

/// Hashes the regions of `frame_hash` assertions at `frame` from that frame's capture.
fn region_hashes(
    scenario: &Scenario,
    frame: u64,
    frames: &[lotus_session::native_session::Frame],
    seen: &mut Observations,
) -> io::Result<()> {
    for (index, assertion) in scenario.assertions.iter().enumerate() {
        let (Some(region), Some(screen)) = (&assertion.region, &assertion.screen) else {
            continue;
        };
        if assertion.kind != "frame_hash" || assertion.at_frame != Some(frame) {
            continue;
        }
        if let Some(captured) = frames.iter().find(|f| &f.lease.screen == screen) {
            seen.region_hashes
                .insert(index, captured.region_sha256(region)?);
        }
    }
    Ok(())
}

/// Sets one non-stopping execute breakpoint per coverage assertion.
async fn arm_coverage(
    session: &mut NativeSession,
    scenario: &Scenario,
) -> io::Result<BTreeMap<usize, u64>> {
    use lotus_protocol::debug::{BreakpointKind, DebugCommand};
    let mut coverage = BTreeMap::new();
    for (index, assertion) in scenario.assertions.iter().enumerate() {
        if !matches!(assertion.kind.as_str(), "reached" | "not_reached") {
            continue;
        }
        let address = assertion.address.expect("validated address");
        // Bit 0 marks a Thumb target; Thumb instructions are 2 bytes.
        let (address, length) = if address & 1 == 1 {
            (address & !1, 2)
        } else {
            (address, 4)
        };
        let set = session
            .debug(&DebugCommand::BreakpointSet {
                address,
                kind: BreakpointKind::Execute,
                length,
            })
            .await?;
        let id = set["id"]
            .as_u64()
            .ok_or_else(|| io::Error::other("breakpoint_set returned no id"))?;
        session
            .debug(&DebugCommand::BreakpointConfigure {
                id,
                process: None,
                hit_after: u64::MAX,
                condition: None,
                log: None,
            })
            .await?;
        coverage.insert(index, id);
    }
    Ok(coverage)
}

/// Restricts history to the categories the run reads, before load. Returns whether SVC/IPC
/// tracing is needed.
async fn configure_events(
    session: &mut NativeSession,
    filters: &[EventFilter],
) -> io::Result<bool> {
    use lotus_protocol::debug::{DebugCommand, EventCategory};
    let mut history: Vec<EventCategory> = Vec::new();
    let mut trace = false;
    for filter in filters {
        let name = assertions::category(&filter.kind).expect("validated kind");
        trace |= matches!(name, "svc" | "ipc");
        let category: EventCategory =
            serde_json::from_value(Value::from(name)).map_err(io::Error::other)?;
        if !history.contains(&category) {
            history.push(category);
        }
    }
    session
        .debug(&DebugCommand::EventsConfigure {
            notify: None,
            history: Some(history),
        })
        .await?;
    Ok(trace)
}

/// One fresh root, one worker, one bounded run. Always attempt stop; Drop kills on failure.
///
/// The run writes `report.json` and `report.txt` whenever the worker completed the schedule; a
/// run with any assertion that is not `pass` also writes `failure.json` and returns an error.
pub async fn run(
    scenario: &Scenario,
    scenario_directory: &Path,
    executable: &Path,
    output: &Path,
) -> io::Result<RunReport> {
    let report = run_report(scenario, scenario_directory, executable, output, None).await?;
    if let Some(failure) = first_assertion_failure(&report) {
        return Err(io::Error::other(failure_message(failure)));
    }
    Ok(report)
}

pub(crate) async fn run_report(
    scenario: &Scenario,
    scenario_directory: &Path,
    executable: &Path,
    output: &Path,
    fixture: Option<(&Path, &str)>,
) -> io::Result<RunReport> {
    use lotus_protocol::debug::DebugCommand;
    validate_execution(scenario)?;
    let content = match fixture {
        Some((path, _)) => path.canonicalize()?,
        None => scenario_directory
            .join(&scenario.content.path_handle)
            .canonicalize()?,
    };
    let content_hash = file_hash(&content)?;
    if fixture.is_some_and(|(_, expected)| expected != content_hash)
        || scenario
            .content
            .sha256
            .as_ref()
            .is_some_and(|expected| expected.to_ascii_lowercase() != content_hash)
    {
        return Err(invalid("fixture SHA-256 mismatch"));
    }
    let executable_hash = file_hash(executable)?;
    if scenario
        .profile
        .worker_sha256
        .as_ref()
        .is_some_and(|expected| !expected.eq_ignore_ascii_case(&executable_hash))
    {
        return Err(invalid("worker executable SHA-256 mismatch"));
    }
    // create_dir rejects accidental overwrite/reuse of a prior run.
    fs::create_dir_all(output.parent().unwrap_or(Path::new(".")))?;
    fs::create_dir(output)?;
    let started = Instant::now();
    let mut session = NativeSession::spawn(executable, &output.join("worker")).await?;
    let playing = scenario
        .movie
        .as_ref()
        .is_some_and(|movie| movie.mode == "play");
    let filters = event_filters(scenario);
    let result = tokio::time::timeout(
        Duration::from_secs(scenario.stop.wall_timeout_seconds),
        async {
            prepare(&mut session, scenario, scenario_directory).await?;
            let mut ledger = None;
            let mut trace = false;
            if !filters.is_empty() {
                trace = configure_events(&mut session, &filters).await?;
                ledger = Some(EventLedger::new(filters.clone()));
            }
            session.load(&content).await?;
            if trace {
                // SVC and IPC events are only built while tracing (`debug.h` category note).
                session
                    .debug(&DebugCommand::Trace { enabled: true })
                    .await?;
            }
            let coverage = arm_coverage(&mut session, scenario).await?;
            if !playing {
                session.input(InputState::default(), "scenario").await?;
            }
            let mut schedule: BTreeMap<u64, Vec<InputEvent>> = BTreeMap::new();
            for event in &scenario.input.schedule {
                schedule
                    .entry(event.at_frame)
                    .or_default()
                    .push(event.clone());
                if event.action == "press" && event.at_frame < scenario.stop.max_frames {
                    let mut release = event.clone();
                    release.action = "up".into();
                    release.at_frame += 1;
                    schedule.entry(release.at_frame).or_default().push(release);
                }
            }
            let stops: Vec<u64> = scenario
                .captures
                .iter()
                .filter_map(|capture| capture.when.as_ref().and_then(|when| when.frame))
                .chain(
                    scenario
                        .assertions
                        .iter()
                        .filter_map(|assertion| assertion.at_frame),
                )
                .collect();
            let mut pending: Vec<(usize, EventFilter)> = scenario
                .captures
                .iter()
                .enumerate()
                .filter_map(|(index, capture)| {
                    let when = capture.when.as_ref()?;
                    Some((
                        index,
                        EventFilter {
                            kind: when.event.clone()?,
                            text: when.text_contains.clone(),
                        },
                    ))
                })
                .collect();
            let mut seen = Observations::default();
            let mut checkpoints = Vec::new();
            let mut frame = 0;
            let mut input = InputState::default();
            loop {
                if let Some(events) = schedule.get(&frame) {
                    for event in events {
                        apply_input(&mut input, event);
                    }
                    session.input(input, "scenario").await?;
                }
                if frame > 0 {
                    if let Some(ledger) = ledger.as_mut() {
                        drain(&mut session, ledger, frame).await?;
                        let mut fired = Vec::new();
                        pending.retain(|(index, filter)| {
                            let hit = ledger.count(filter).is_some_and(|count| count.count > 0);
                            if hit {
                                fired.push((*index, filter.kind.clone()));
                            }
                            !hit
                        });
                        for (index, kind) in fired {
                            capture_screens(
                                &mut session,
                                output,
                                frame,
                                Some(format!("capture{index}-{kind}")),
                                &mut checkpoints,
                            )
                            .await?;
                        }
                    }
                    read_values(&mut session, scenario, frame, &mut seen).await?;
                    if frame == scenario.stop.max_frames || stops.contains(&frame) {
                        let before = checkpoints.len();
                        let frames =
                            capture_screens(&mut session, output, frame, None, &mut checkpoints)
                                .await?;
                        seen.hashes
                            .extend(checkpoints[before..].iter().map(|point| {
                                (point.frame, point.screen.clone(), point.sha256.clone())
                            }));
                        region_hashes(scenario, frame, &frames, &mut seen)?;
                    }
                }
                if frame == scenario.stop.max_frames {
                    break;
                }
                let next_input = schedule
                    .range((frame + 1)..)
                    .next()
                    .map(|(key, _)| *key)
                    .unwrap_or(scenario.stop.max_frames);
                let next_stop = stops
                    .iter()
                    .copied()
                    .filter(|next| *next > frame)
                    .min()
                    .unwrap_or(scenario.stop.max_frames);
                // An event-triggered capture is taken at the end of the frame its event is
                // recorded in, so the run advances one frame at a time while one is pending.
                let step = if pending.is_empty() { 60 } else { 1 };
                let next = (frame + step)
                    .min(next_input)
                    .min(next_stop)
                    .min(scenario.stop.max_frames);
                let status = session.advance((next - frame) as u16).await?;
                if status["frames"].as_u64() != Some(next) {
                    return Err(io::Error::other("worker missed exact frame checkpoint"));
                }
                frame = next;
            }
            seen.frames = frame;
            if !coverage.is_empty() {
                let reply = session.debug(&DebugCommand::Breakpoints).await?;
                let listed = reply["breakpoints"].as_array().cloned().unwrap_or_default();
                for (index, id) in &coverage {
                    if let Some(hits) = listed
                        .iter()
                        .find(|entry| entry["id"].as_u64() == Some(*id))
                        .and_then(|entry| entry["hits"].as_u64())
                    {
                        seen.hits.insert(*index, hits);
                    }
                }
            }
            seen.events = ledger;
            let mut results = assertions::evaluate(scenario, &seen);
            let lost = seen
                .events
                .as_ref()
                .is_some_and(|ledger| ledger.loss.is_some());
            results.extend(pending.iter().map(|(index, filter)| {
                assertions::unfired_trigger(scenario.assertions.len() + index, *index, filter, lost)
            }));
            let movie = match &scenario.movie {
                Some(movie) => Some(finish_movie(&mut session, movie, output).await?),
                None => None,
            };
            let status = session.status().await?;
            let first_failure = results
                .iter()
                .find(|result| result.outcome != Outcome::Pass)
                .map(|result| result.index);
            Ok(RunReport {
                name: scenario.name.clone(),
                backend: status["backend_name"].as_str().unwrap_or("unknown").into(),
                content_sha256: content_hash,
                worker_executable_sha256: executable_hash,
                source: status["source"].as_str().unwrap_or("unknown").into(),
                frames: frame,
                checkpoints,
                final_status: status,
                host_elapsed_ms: started.elapsed().as_millis(),
                evidence: "verified emulator; no hardware claim".into(),
                movie,
                schema_version: scenario.version,
                passed: first_failure.is_none(),
                first_failure,
                assertions: results,
                event_window: seen.events,
            })
        },
    )
    .await;
    let result = match result {
        Ok(result) => result,
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "scenario host deadline",
        )),
    };
    let stop = session.stop().await;
    let report = match result {
        Ok(report) => report,
        Err(error) => {
            fs::write(
                output.join("failure.json"),
                serde_json::json!({"error": error.to_string()}).to_string(),
            )?;
            return Err(error);
        }
    };
    stop?;
    write_report(&report, output)?;
    Ok(report)
}

/// Retain observations and the first unsuccessful assertion for every completed attempt.
fn write_report(report: &RunReport, output: &Path) -> io::Result<()> {
    fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(report).map_err(io::Error::other)?,
    )?;
    fs::write(
        output.join("report.txt"),
        format!(
            "{} {} worker {} content {}\n{}",
            report.name,
            if report.passed { "PASS" } else { "FAIL" },
            report.worker_executable_sha256,
            report.content_sha256,
            assertions::render(&report.assertions)
        ),
    )?;
    if let Some(failure) = first_assertion_failure(report) {
        fs::write(
            output.join("failure.json"),
            serde_json::json!({"error": failure_message(failure), "assertion": failure})
                .to_string(),
        )?;
    }
    Ok(())
}

fn first_assertion_failure(report: &RunReport) -> Option<&AssertionResult> {
    report
        .assertions
        .iter()
        .find(|result| Some(result.index) == report.first_failure)
}

fn failure_message(failure: &AssertionResult) -> String {
    format!(
        "assertion {} ({}) {:?}: expected {} actual {}",
        failure.index, failure.kind, failure.outcome, failure.expected, failure.actual
    )
}

/// Repeat with fresh storage; compare guest pixel hashes, excluding host telemetry.
pub async fn repeat(
    scenario: &Scenario,
    directory: &Path,
    executable: &Path,
    root: &Path,
    count: u8,
) -> io::Result<Vec<RunReport>> {
    if !(1..=10).contains(&count) {
        return Err(invalid("repeat count must be 1..=10"));
    }
    let mut reports: Vec<RunReport> = Vec::new();
    for _ in 0..count {
        let output = root.join(SessionUuid::generate().to_string());
        let report = run(scenario, directory, executable, &output).await?;
        if let Some(first) = reports.first() {
            if first
                .checkpoints
                .iter()
                .map(|point| (&point.screen, point.frame, &point.sha256))
                .ne(report
                    .checkpoints
                    .iter()
                    .map(|point| (&point.screen, point.frame, &point.sha256)))
            {
                return Err(io::Error::other(
                    "repeated guest frame hashes diverged; inspect retained reports",
                ));
            }
        }
        reports.push(report);
    }
    Ok(reports)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn example() -> Scenario {
        Scenario::load(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs/scenario-example.yaml"),
        )
        .expect("example")
    }

    #[test]
    fn example_executes_only_on_the_experimental_worker() {
        let mut scenario = example();
        assert!(validate_execution(&scenario).is_ok());
        scenario.profile.worker_flavor = crate::WorkerFlavor::Integration;
        let error = validate_execution(&scenario).unwrap_err().to_string();
        assert!(error.contains("experimental"), "{error}");
        // Memory, register and frame checks alone run on AZAHAR.
        scenario
            .assertions
            .retain(|assertion| !assertions::needs_experimental(&assertion.kind));
        scenario.captures.retain(|capture| {
            capture
                .when
                .as_ref()
                .is_some_and(|when| when.frame.is_some())
        });
        assert!(validate_execution(&scenario).is_ok());
    }

    #[test]
    fn version_1_keeps_its_executable_subset() {
        let mut scenario = example();
        scenario.version = 1;
        scenario.captures.retain(|capture| {
            capture
                .when
                .as_ref()
                .is_some_and(|when| when.frame.is_some())
        });
        scenario
            .assertions
            .retain(|assertion| assertion.kind == "no_worker_crash");
        assert!(validate_execution(&scenario).is_ok());
        scenario.assertions.push(example().assertions[2].clone());
        assert!(validate_execution(&scenario).is_err());
    }
}
