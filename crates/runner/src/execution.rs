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

use crate::{InputEvent, Scenario};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub frame: u64,
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
        let frame = capture
            .when
            .as_ref()
            .and_then(|when| when.frame)
            .ok_or_else(|| invalid("only frame capture triggers are supported"))?;
        if frame == 0
            || frame > scenario.stop.max_frames
            || capture
                .when
                .as_ref()
                .is_some_and(|when| when.event.is_some())
            || capture
                .include
                .iter()
                .any(|item| item != "top_raw_frame" && item != "bottom_raw_frame")
            || capture.max_bytes.is_some_and(|bytes| bytes < 691200)
        {
            return Err(invalid(
                "capture requires bounded native top/bottom screens",
            ));
        }
    }
    if scenario
        .assertions
        .iter()
        .any(|assertion| !matches!(assertion.kind.as_str(), "no_worker_crash" | "frame_count"))
    {
        return Err(invalid("event assertions require the future trace backend"));
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

/// One fresh root, one worker, one bounded run. Always attempt stop; Drop kills on failure.
pub async fn run(
    scenario: &Scenario,
    scenario_directory: &Path,
    executable: &Path,
    output: &Path,
) -> io::Result<RunReport> {
    validate_execution(scenario)?;
    let content = scenario_directory
        .join(&scenario.content.path_handle)
        .canonicalize()?;
    let content_hash = file_hash(&content)?;
    if scenario
        .content
        .sha256
        .as_ref()
        .is_some_and(|expected| expected.to_ascii_lowercase() != content_hash)
    {
        return Err(invalid("fixture SHA-256 mismatch"));
    }
    let executable_hash = file_hash(executable)?;
    // create_dir rejects accidental overwrite/reuse of a prior run.
    fs::create_dir_all(output.parent().unwrap_or(Path::new(".")))?;
    fs::create_dir(output)?;
    let started = Instant::now();
    let mut session = NativeSession::spawn(executable, &output.join("worker")).await?;
    let playing = scenario
        .movie
        .as_ref()
        .is_some_and(|movie| movie.mode == "play");
    let result = tokio::time::timeout(
        Duration::from_secs(scenario.stop.wall_timeout_seconds),
        async {
            prepare(&mut session, scenario, scenario_directory).await?;
            session.load(&content).await?;
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
                if frame > 0
                    && (frame == scenario.stop.max_frames
                        || scenario.captures.iter().any(|capture| {
                            capture.when.as_ref().and_then(|when| when.frame) == Some(frame)
                        }))
                {
                    for captured in session.capture().await? {
                        let filename = format!("{frame}-{}.bgra", captured.lease.screen);
                        let hash = format!("{:x}", Sha256::digest(&captured.bgra));
                        fs::write(output.join(&filename), &captured.bgra)?;
                        checkpoints.push(Checkpoint {
                            frame,
                            screen: captured.lease.screen,
                            sha256: hash,
                            bytes: captured.bgra.len(),
                            artifact: filename,
                        });
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
                let next_capture = scenario
                    .captures
                    .iter()
                    .filter_map(|capture| capture.when.as_ref().and_then(|when| when.frame))
                    .filter(|next| *next > frame)
                    .min()
                    .unwrap_or(scenario.stop.max_frames);
                let next = (frame + 60)
                    .min(next_input)
                    .min(next_capture)
                    .min(scenario.stop.max_frames);
                let status = session.advance((next - frame) as u16).await?;
                if status["frames"].as_u64() != Some(next) {
                    return Err(io::Error::other("worker missed exact frame checkpoint"));
                }
                frame = next;
            }
            for assertion in &scenario.assertions {
                if assertion.kind == "frame_count"
                    && assertion.minimum.is_some_and(|minimum| frame < minimum)
                {
                    return Err(io::Error::other("frame count assertion failed"));
                }
            }
            let movie = match &scenario.movie {
                Some(movie) => Some(finish_movie(&mut session, movie, output).await?),
                None => None,
            };
            let status = session.status().await?;
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
    match result {
        Ok(report) => {
            stop?;
            fs::write(
                output.join("report.json"),
                serde_json::to_vec_pretty(&report).map_err(io::Error::other)?,
            )?;
            Ok(report)
        }
        Err(error) => {
            fs::write(
                output.join("failure.json"),
                serde_json::json!({"error": error.to_string()}).to_string(),
            )?;
            Err(error)
        }
    }
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
    #[test]
    fn proposed_trace_scenario_cannot_silently_execute() {
        let scenario = Scenario::load(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs/scenario-example.yaml"),
        )
        .expect("example");
        assert!(validate_execution(&scenario).is_err());
    }
}
