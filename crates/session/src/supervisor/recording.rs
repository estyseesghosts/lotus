//! Supervisor side of live test recording (lotus#95); data model in
//! [`crate::test_recording`]. Every operation takes the session's `native` lock, so a
//! start, mark or stop is ordered with the inputs and debugger commands around it.
use std::io;

use sha2::{Digest, Sha256};

use super::{
    actor_client, check_lease, lock, observe, pause_if_running, unix_ms, SessionHandle, Supervisor,
};
use crate::native_session::{InputState, NativeSession};
use crate::telemetry::Actor;
use crate::test_recording::{
    InputSource, Intervention, MarkRead, MarkValue, RecordedInput, RecordedMark, TestRecording,
    MAX_MARKS, MAX_MARK_READS,
};

/// Settings a scenario profile states itself (`lotus_runner::Profile`, runner `prepare`).
const PROFILE_SETTINGS: [&str; 4] = [
    "is_new_3ds",
    "graphics_api",
    "deterministic_async_operations",
    "async_fs_operations",
];

/// Files both pinned workers (AZAHAR a78f7f3, CITnLO 638c9f4) create in an empty root when
/// content loads: the HLE system save data, CEC, news, certificate and MCU/MAC defaults.
/// Measured with the input fixture and t03; the runner's fresh root gets them too.
const BOOT_CREATED: [&str; 11] = [
    "nand/data/00000000000000000000000000000000/extdata/00048000/F000000B/metadata",
    "nand/data/00000000000000000000000000000000/extdata/00048000/F000000B/user/gamecoin.dat",
    "nand/data/00000000000000000000000000000000/sysdata/00010017/00000000/config",
    "nand/data/00000000000000000000000000000000/sysdata/00010026/00000000/CEC/MBoxList____",
    "nand/data/00000000000000000000000000000000/sysdata/00010026/00000000/eventlog.dat",
    "nand/data/00000000000000000000000000000000/sysdata/00010035/00000000/news.db",
    "nand/dbs/certs.db/Root-CA00000003-CP0000000b.cert",
    "nand/dbs/certs.db/Root-CA00000003-XS0000000c.cert",
    "nand/dbs/certs.db/Root-CA00000003.cert",
    "sysdata/mac.txt",
    "sysdata/mcu.dat",
];
/// Most storage paths named in a recording; the rest are counted.
const MAX_STORAGE_PATHS: usize = 16;

/// Guest files at the start that a fresh runner root would not have, as `/` paths.
fn guest_storage(root: &std::path::Path) -> (Vec<String>, u64) {
    let mut found = Vec::new();
    let mut extra = 0u64;
    let mut pending: Vec<std::path::PathBuf> = crate::card_store::GUEST_DIRS
        .iter()
        .map(|name| root.join(name))
        .collect();
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => pending.push(path),
                Ok(kind) if kind.is_file() => {
                    let Ok(relative) = path.strip_prefix(root) else {
                        continue;
                    };
                    let relative = relative
                        .components()
                        .map(|part| part.as_os_str().to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("/");
                    if BOOT_CREATED.contains(&relative.as_str()) {
                        continue;
                    }
                    if found.len() < MAX_STORAGE_PATHS {
                        found.push(relative);
                    } else {
                        extra += 1;
                    }
                }
                _ => {}
            }
        }
    }
    found.sort();
    (found, extra)
}

fn renderer_name(value: &serde_json::Value) -> String {
    // Azahar `Settings::GraphicsAPI`: Software 0, OpenGL 1, Vulkan 2.
    match value.as_u64() {
        Some(0) => "software",
        Some(1) => "opengl",
        Some(2) => "vulkan",
        _ => "unknown",
    }
    .to_string()
}

fn not_recording() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        "no test recording on this session; start one first",
    )
}

/// Record an input the worker accepted, at the frame boundary its reply names: the worker
/// installs it there (`ctr_worker/control.h` `input`, feature `input_boundary`).
pub(super) fn note_input(
    handle: &SessionHandle,
    native: &NativeSession,
    state: InputState,
    owner: &str,
    actor: Actor,
    reply: &serde_json::Value,
) {
    let mut recording = lock(&handle.recording);
    let Some(recording) = recording.as_mut() else {
        return;
    };
    let Some(frame) = reply["input_frame"].as_u64() else {
        recording.invalid = Some("an input reply carried no input_frame".into());
        return;
    };
    recording.note_input(
        native.epoch(),
        RecordedInput {
            frame,
            state,
            owner: owner.to_string(),
            source: actor.into(),
        },
    );
}

/// Record a debugger command that changes the session while recording.
pub(super) fn note_intervention(
    handle: &SessionHandle,
    epoch: u64,
    command: &lotus_protocol::debug::DebugCommand,
    client: &str,
) {
    let mut recording = lock(&handle.recording);
    let Some(recording) = recording.as_mut() else {
        return;
    };
    let operation = serde_json::to_value(command)
        .ok()
        .and_then(|value| value["operation"].as_str().map(str::to_string))
        .unwrap_or_else(|| "debug".into());
    let source = match client {
        "desktop" => InputSource::Desktop,
        "mcp" => InputSource::Mcp,
        _ => InputSource::Host,
    };
    let observed_frame = handle.telemetry().frames;
    recording.note_intervention(
        epoch,
        Intervention {
            observed_frame,
            operation,
            source,
        },
    );
}

impl Supervisor {
    /// Reload the session's content and start recording from frame 0. Replaces a stopped
    /// or invalid recording; refuses while one is active.
    pub async fn record_test_start(&self, id: &str, actor: Actor) -> io::Result<TestRecording> {
        self.require_control()?;
        let handle = self.handle(id)?;
        let info = handle.info();
        let asset = info
            .asset
            .clone()
            .ok_or_else(|| io::Error::other("load approved content before recording a test"))?;
        let content = self.resolve_asset(&asset)?;
        let mut native = handle.native.lock().await;
        check_lease(&handle, actor_client(actor))?;
        // Without it an input lands between CPU slices, mid-frame, and cannot be replayed at
        // a frame (lotus#95).
        native.require(lotus_protocol::capabilities::feature::INPUT_BOUNDARY)?;
        if lock(&handle.recording)
            .as_ref()
            .is_some_and(TestRecording::is_active)
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "a test recording is already active on this session",
            ));
        }
        handle.set_playing(false);
        pause_if_running(&mut native).await?;
        handle.cancel_input(true);
        let status = self.load_with_gpu_admission(&mut native, &content).await?;
        observe(&handle, &native, &status);
        self.rebind_source_breakpoints(&handle, &mut native).await;
        self.budgets.clear_recordings(id);
        self.settings_revision
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        // Release everything at frame 0, as the runner does before its first frame. A
        // scenario owner keeps the worker's input until the next load, so try live first.
        let released = native.input(InputState::default(), "live").await;
        let owner = match released {
            Ok(_) => "live",
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                native.input(InputState::default(), "scenario").await?;
                "scenario"
            }
            Err(error) => return Err(error),
        };
        let status = native.status().await?;
        observe(&handle, &native, &status);
        if status["frames"].as_u64() != Some(0) {
            return Err(io::Error::other(
                "the reloaded guest did not start at frame 0",
            ));
        }
        let values = native.settings_values();
        let defaults = native.startup_defaults();
        let other_settings = values
            .iter()
            .filter(|(name, value)| {
                !PROFILE_SETTINGS.contains(&name.as_str()) && defaults.get(*name) != Some(value)
            })
            .map(|(name, value)| {
                (
                    name.clone(),
                    serde_json::json!({"default": defaults.get(name), "value": value}),
                )
            })
            .collect();
        let metadata = native.metadata();
        let (card_storage, card_storage_more) = guest_storage(&handle.root);
        let recording = TestRecording {
            id: lotus_protocol::envelope::SessionUuid::generate().to_string(),
            session: info.id.clone(),
            session_created_unix_ms: info.created_unix_ms,
            epoch: native.epoch(),
            started_unix_ms: unix_ms(),
            content_name: content
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            asset,
            content: native.content_identity().clone(),
            worker: serde_json::json!({
                "source": metadata["identity"]["source"],
                "backend": metadata["identity"]["backend"],
                "worker_sha256": metadata["identity"]["worker_sha256"],
            }),
            model: if values.get("is_new_3ds") == Some(&serde_json::Value::Bool(true)) {
                "new3ds".into()
            } else {
                "old3ds".into()
            },
            renderer: renderer_name(&metadata["identity"]["renderer"]),
            determinism: values.get("deterministic_async_operations")
                == Some(&serde_json::Value::Bool(true))
                && values.get("async_fs_operations") == Some(&serde_json::Value::Bool(false)),
            other_settings,
            card_storage,
            card_storage_more,
            inputs: vec![RecordedInput {
                frame: 0,
                state: InputState::default(),
                owner: owner.into(),
                source: actor.into(),
            }],
            marks: Vec::new(),
            interventions: Vec::new(),
            interventions_dropped: 0,
            inputs_dropped: 0,
            invalid: None,
            stopped_at_frame: None,
        };
        *lock(&handle.recording) = Some(recording.clone());
        handle.record(actor, "record_test_start");
        Ok(recording)
    }

    /// Pause at a frame boundary and keep a checkpoint: both screen hashes and `reads`.
    /// A playing session continues afterwards.
    pub async fn record_test_mark(
        &self,
        id: &str,
        label: &str,
        reads: &[MarkRead],
        actor: Actor,
    ) -> io::Result<RecordedMark> {
        self.require_control()?;
        let label = label.trim();
        if label.is_empty() || label.chars().count() > 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a mark needs a label of 1 to 64 characters",
            ));
        }
        if reads.len() > MAX_MARK_READS || reads.iter().any(|read| !matches!(read.width, 1 | 2 | 4))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("a mark reads at most {MAX_MARK_READS} values of 1, 2 or 4 bytes"),
            ));
        }
        let handle = self.handle(id)?;
        let mut native = handle.native.lock().await;
        check_lease(&handle, actor_client(actor))?;
        {
            let mut recording = lock(&handle.recording);
            let recording = recording.as_mut().ok_or_else(not_recording)?;
            if !recording.same_guest(native.epoch()) || !recording.is_active() {
                return Err(io::Error::other(
                    recording
                        .invalid
                        .clone()
                        .unwrap_or_else(|| "the test recording has stopped".into()),
                ));
            }
            if recording.marks.len() >= MAX_MARKS {
                return Err(io::Error::other(format!(
                    "a recording holds at most {MAX_MARKS} marks"
                )));
            }
        }
        let playing = handle.is_playing();
        handle.set_playing(false);
        pause_if_running(&mut native).await?;
        let status = native.status().await?;
        observe(&handle, &native, &status);
        let frame = status["frames"]
            .as_u64()
            .ok_or_else(|| io::Error::other("status carried no frame count"))?;
        let mut top = None;
        let mut bottom = None;
        for captured in native.capture().await? {
            let hash = format!("{:x}", Sha256::digest(&captured.bgra));
            match captured.lease.screen.as_str() {
                "top" => top = Some(hash),
                "bottom" => bottom = Some(hash),
                _ => {}
            }
        }
        let (Some(top_sha256), Some(bottom_sha256)) = (top, bottom) else {
            return Err(io::Error::other("capture lacked a top or bottom screen"));
        };
        let mut memory = Vec::with_capacity(reads.len());
        for read in reads {
            let reply = native.memory(read.address, u16::from(read.width)).await?;
            let bytes: Vec<u8> = serde_json::from_value(reply["bytes"].clone())
                .map_err(|_| io::Error::other("read_memory reply has no bytes"))?;
            if bytes.len() != usize::from(read.width) {
                return Err(io::Error::other(format!(
                    "0x{:08X} is not readable for {} bytes",
                    read.address, read.width
                )));
            }
            let value = bytes
                .iter()
                .rev()
                .fold(0u32, |value, byte| (value << 8) | u32::from(*byte));
            memory.push(MarkValue {
                address: read.address,
                width: read.width,
                value,
            });
        }
        let mark = RecordedMark {
            frame,
            label: label.to_string(),
            top_sha256,
            bottom_sha256,
            memory,
            source: actor.into(),
        };
        {
            let mut recording = lock(&handle.recording);
            let recording = recording.as_mut().ok_or_else(not_recording)?;
            if recording
                .marks
                .last()
                .is_some_and(|last| last.frame > frame)
            {
                return Err(io::Error::other("marks must not go back in time"));
            }
            recording.marks.push(mark.clone());
        }
        if playing {
            native.run().await?;
            handle.set_playing(true);
        }
        handle.record(actor, "record_test_mark");
        Ok(mark)
    }

    /// Pause and end the recording at the current frame. The session stays paused.
    pub async fn record_test_stop(&self, id: &str, actor: Actor) -> io::Result<TestRecording> {
        self.require_control()?;
        let handle = self.handle(id)?;
        let mut native = handle.native.lock().await;
        check_lease(&handle, actor_client(actor))?;
        if lock(&handle.recording).is_none() {
            return Err(not_recording());
        }
        handle.set_playing(false);
        pause_if_running(&mut native).await?;
        let status = native.status().await?;
        observe(&handle, &native, &status);
        let mut recording = lock(&handle.recording);
        let recording = recording.as_mut().ok_or_else(not_recording)?;
        if recording.same_guest(native.epoch()) && recording.stopped_at_frame.is_none() {
            recording.stopped_at_frame = status["frames"].as_u64();
        }
        handle.record(actor, "record_test_stop");
        Ok(recording.clone())
    }

    /// The session's current or last recording.
    pub fn record_test(&self, id: &str) -> io::Result<Option<TestRecording>> {
        let handle = self.handle(id)?;
        let epoch = handle.telemetry().epoch;
        let mut recording = lock(&handle.recording);
        if let Some(recording) = recording.as_mut() {
            if recording.is_active() {
                recording.same_guest(epoch);
            }
        }
        Ok(recording.clone())
    }

    /// Discard the session's recording.
    pub fn record_test_discard(&self, id: &str) -> io::Result<()> {
        self.require_control()?;
        *lock(&self.handle(id)?.recording) = None;
        Ok(())
    }
}
