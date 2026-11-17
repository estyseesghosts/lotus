//! Shared native operations used by desktop, CLI and MCP.
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use lotus_protocol::control::{Method, Reply, Status};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::controlled_worker::ControlledWorker;
use crate::{SessionState, WorkerSpec};

/// A file-ring lease. Reading checks identity before exposing owned pixel bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameLease {
    pub screen: String,
    /// Ring file under the session root; empty for shared-memory previews.
    pub path: String,
    pub epoch: u64,
    pub sequence: u64,
    pub slot: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: String,
    pub orientation: String,
    pub bytes: usize,
}

/// Native pixels copied out of a ring slot; independent of subsequent captures.
#[derive(Debug)]
pub struct Frame {
    pub lease: FrameLease,
    pub bgra: Vec<u8>,
}

/// Input is a complete state, so disconnect/release can always send zero.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct InputState {
    pub buttons: u32,
    pub x: u16,
    pub y: u16,
    pub touch: bool,
}

/// Owns one private native worker. Each command completes at an emulation safe point.
#[derive(Debug)]
pub struct NativeSession {
    worker: ControlledWorker,
    root: PathBuf,
    profile_hash: String,
    identity: Value,
    /// Set by [`Self::prefer_vulkan`]: a typed Vulkan failure at load falls back to software.
    vulkan_fallback_armed: bool,
    /// Worker accepts `settings_set` (`debug_api` in its hello reply).
    settings_api: bool,
    /// Why this session runs on software after Vulkan was preferred.
    renderer_fallback: Option<String>,
    /// Worker executable, kept to replace the process on a Vulkan fallback.
    executable: PathBuf,
    /// Every setting applied through [`Self::apply_profile`], replayed on a replacement.
    applied: std::collections::BTreeMap<String, lotus_protocol::debug::SettingValue>,
    /// Worker serves `preview` from shared memory (`preview: shared_memory_v1` in hello).
    shared_preview: bool,
    /// The mapped ring, reopened whenever the worker reports a different section.
    preview_ring: Option<crate::preview_ring::Mapping>,
    /// Shared with the supervisor; follows the process across a Vulkan fallback replacement.
    kill: std::sync::Arc<std::sync::Mutex<crate::controlled_worker::KillSwitch>>,
    /// Frame count at which the current `run` pauses itself, from its reply.
    run_target: Option<u64>,
}

/// Frames one `run` advances before the worker pauses itself (`run` accepts 1-600,
/// `ctr_worker/control.h`). Bounded runs keep a host that stops supervising from leaving a
/// worker running; the supervision task continues them.
pub const RUN_BUDGET: u64 = 600;

impl NativeSession {
    pub async fn spawn(executable: &Path, root: &Path) -> io::Result<Self> {
        fs::create_dir_all(root)?;
        let root = native_path(root)?;
        let mut worker =
            ControlledWorker::spawn(&WorkerSpec::new(executable, &root), Duration::from_secs(30))
                .await?;
        let reply = worker.status().await?;
        if reply.backend != "azahar_native_software" {
            worker.stop().await?;
            return Err(io::Error::other("worker lacks native software capability"));
        }
        fs::write(
            root.join("native-manifest.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "protocol": lotus_protocol::protocol_id(), "backend": reply.backend,
                "source": reply.data["source"], "backend_name": reply.data["backend_name"],
                "session": reply.session, "pid": worker.pid(), "epoch": reply.epoch,
                "lifecycle": "active",
                "profile": "old3ds/software/hle/dsp-muted/ticks-0/time-2000-01-01",
            }))
            .map_err(io::Error::other)?,
        )?;
        let mut session = Self {
            worker,
            root,
            profile_hash: String::new(),
            identity: serde_json::json!({"source":reply.data["source"], "backend":reply.data["backend_name"],
            "worker_sha256":file_hash(executable)?}),
            vulkan_fallback_armed: false,
            settings_api: reply.data["debug_api"] == true,
            renderer_fallback: None,
            executable: executable.to_path_buf(),
            applied: std::collections::BTreeMap::new(),
            shared_preview: reply.data["preview"] == "shared_memory_v1",
            preview_ring: None,
            kill: std::sync::Arc::default(),
            run_target: None,
        };
        *session
            .kill
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = session.worker.kill_switch();
        if reply.data["debug_api"] == true {
            session.refresh_profile().await?;
        }
        Ok(session)
    }

    pub async fn debug(
        &mut self,
        command: &lotus_protocol::debug::DebugCommand,
    ) -> io::Result<Value> {
        command.validate().map_err(io::Error::other)?;
        if matches!(
            command,
            lotus_protocol::debug::DebugCommand::SaveState { .. }
        ) {
            self.refresh_profile().await?;
        }
        let mut result = self
            .operation(
                Method::Debug,
                serde_json::to_value(command).map_err(io::Error::other)?,
            )
            .await?;
        if matches!(
            command,
            lotus_protocol::debug::DebugCommand::SettingsSet { .. }
                | lotus_protocol::debug::DebugCommand::InputState { .. }
                | lotus_protocol::debug::DebugCommand::AssetMount { .. }
                | lotus_protocol::debug::DebugCommand::SystemRegenerate { .. }
        ) {
            self.refresh_profile().await?;
        }
        result["profile_hash"] = self.profile_hash.clone().into();
        Ok(result)
    }

    pub fn profile_hash(&self) -> &str {
        &self.profile_hash
    }
    pub fn metadata(&self) -> Value {
        serde_json::json!({"identity":self.identity,"profile_hash":self.profile_hash,"epoch":self.epoch(),
            "renderer":self.identity["renderer"],"renderer_fallback":self.renderer_fallback,
            "paused":self.state()!=SessionState::Running,
            "safe_point":true,"rasterizer_cache_flush":false})
    }
    pub fn decorate(&self, result: &mut Value) {
        if let (Some(result), Some(metadata)) =
            (result.as_object_mut(), self.metadata().as_object())
        {
            for (key, value) in metadata {
                result.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
    }

    /// Startup owns the worker exclusively; hash once after applying the complete profile.
    pub async fn apply_profile(
        &mut self,
        profile: &std::collections::BTreeMap<String, lotus_protocol::debug::SettingValue>,
    ) -> io::Result<()> {
        for (name, value) in profile {
            let command = lotus_protocol::debug::DebugCommand::SettingsSet {
                name: name.clone(),
                value: value.clone(),
            };
            command.validate().map_err(io::Error::other)?;
            self.operation(
                Method::Debug,
                serde_json::to_value(command).map_err(io::Error::other)?,
            )
            .await?;
            self.applied.insert(name.clone(), value.clone());
        }
        if !profile.is_empty() {
            self.refresh_profile().await?;
        }
        Ok(())
    }

    #[must_use]
    pub fn has_settings(&self) -> bool {
        self.settings_api
    }

    /// Select Vulkan (`Settings::GraphicsAPI::Vulkan` = 2) for an interactive session whose
    /// profile names no renderer. If the worker then refuses a load as `unsupported` (typed
    /// Vulkan instance/device failure, see tools/vulkan_acceptance.py), [`Self::load`]
    /// switches to software and records the reason instead of failing the session.
    pub async fn prefer_vulkan(&mut self) -> io::Result<()> {
        self.apply_profile(&std::collections::BTreeMap::from([(
            "graphics_api".to_string(),
            lotus_protocol::debug::SettingValue::Integer(2),
        )]))
        .await?;
        self.vulkan_fallback_armed = true;
        Ok(())
    }

    async fn refresh_profile(&mut self) -> io::Result<()> {
        use sha2::{Digest, Sha256};
        let mut values = std::collections::BTreeMap::new();
        let mut offset = 0u32;
        loop {
            let schema = self.operation(Method::Debug,
                serde_json::json!({"operation": "settings_list", "offset": offset, "limit": 32})).await?;
            for setting in schema["settings"]
                .as_array()
                .ok_or_else(|| io::Error::other("invalid settings schema"))?
            {
                let name = setting["name"]
                    .as_str()
                    .ok_or_else(|| io::Error::other("invalid setting name"))?;
                values.insert(name.to_string(), setting["value"].clone());
            }
            match schema["next"].as_u64() {
                Some(next) if next > u64::from(offset) && next <= 512 => offset = next as u32,
                None => break,
                _ => return Err(io::Error::other("invalid settings cursor")),
            }
        }
        self.profile_hash = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&values).map_err(io::Error::other)?)
        );
        self.identity["renderer"] = values.get("graphics_api").cloned().unwrap_or(Value::Null);
        let path = self.root.join("native-manifest.json");
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&path)?).map_err(io::Error::other)?;
        manifest["profile_hash"] = self.profile_hash.clone().into();
        manifest["settings"] = serde_json::to_value(values).map_err(io::Error::other)?;
        manifest["identity"] = self.identity.clone();
        fs::write(
            path,
            serde_json::to_vec_pretty(&manifest).map_err(io::Error::other)?,
        )?;
        Ok(())
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Value> {
        self.worker.subscribe()
    }

    pub async fn load(&mut self, path: &Path) -> io::Result<Value> {
        let params = serde_json::json!({"path": native_path(path)?});
        if !self.vulkan_fallback_armed {
            return self.operation(Method::Load, params).await;
        }
        let reply = self.worker.command(Method::Load, params.clone()).await?;
        if reply.status != Status::Unsupported {
            return self.finish(Method::Load, reply);
        }
        // Only reached with graphics_api = Vulkan chosen by prefer_vulkan, never an explicit one.
        let reason = reply.data["reason"]
            .as_str()
            .unwrap_or("Vulkan renderer unavailable")
            .to_string();
        // A second load in the same worker after a failed Vulkan load trips a kernel assertion
        // (core/hle/kernel/kernel.cpp, KernelSystem::SetCPUs; verified emulator), so replace
        // the process: same root, same applied settings, software renderer.
        let mut profile = self.applied.clone();
        profile.insert(
            "graphics_api".to_string(),
            lotus_protocol::debug::SettingValue::Integer(0),
        );
        self.stop().await?;
        let mut replacement = Self::spawn(&self.executable, &self.root).await?;
        replacement.renderer_fallback = Some(reason);
        if let Err(error) = replacement.apply_profile(&profile).await {
            let _ = replacement.stop().await;
            return Err(error);
        }
        *self
            .kill
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = replacement.worker.kill_switch();
        replacement.kill = self.kill.clone();
        *self = replacement;
        self.operation(Method::Load, params).await
    }

    /// Advance 1–600 presented frames, with a native 20s and supervisor 30s deadline.
    pub async fn advance(&mut self, frames: u16) -> io::Result<Value> {
        if frames == 0 || frames > 600 {
            return Err(io::Error::other("frame batch must be in 1..=600"));
        }
        self.operation(Method::RunUntil, serde_json::json!({"frames": frames}))
            .await
    }

    pub async fn run(&mut self) -> io::Result<Value> {
        let reply = self
            .operation(Method::Run, serde_json::json!({"frames": RUN_BUDGET}))
            .await?;
        self.run_target = reply["frames"].as_u64().map(|frames| frames + RUN_BUDGET);
        Ok(reply)
    }

    /// Frame count at which the last `run` pauses itself, if its reply reported frames.
    #[must_use]
    pub fn run_target(&self) -> Option<u64> {
        self.run_target
    }

    pub async fn pause(&mut self) -> io::Result<Value> {
        self.operation(Method::Pause, Value::Null).await
    }

    pub async fn reset(&mut self) -> io::Result<Value> {
        self.operation(Method::Reset, Value::Null).await
    }

    /// Make the card on disk match the guest's writes; requires a paused worker. Fails with
    /// `ResourceBusy` while a guest FS operation is pending on a host thread, and with
    /// `Unsupported` on workers without `flush_storage`. An idle worker has no open guest files.
    pub async fn flush_storage(&mut self) -> io::Result<()> {
        if self.state() == SessionState::Idle {
            return Ok(());
        }
        let reply = self
            .worker
            .command(Method::FlushStorage, Value::Null)
            .await?;
        match (reply.status, reply.data["reason"].as_str()) {
            (Status::Ok, _) => Ok(()),
            (Status::NotReady, Some(reason)) => Err(io::Error::new(
                io::ErrorKind::ResourceBusy,
                format!("guest storage is not quiescent: {reason}"),
            )),
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "worker cannot flush guest storage (flush_storage); update the worker",
            )),
        }
    }

    pub async fn input(&mut self, state: InputState, owner: &str) -> io::Result<Value> {
        let mut params = serde_json::to_value(state).map_err(io::Error::other)?;
        params["owner"] = owner.into();
        self.operation(Method::Input, params).await
    }

    pub async fn status(&mut self) -> io::Result<Value> {
        self.operation(Method::Status, Value::Null).await
    }

    pub async fn registers(&mut self, core: u32) -> io::Result<Value> {
        self.operation(Method::Registers, serde_json::json!({"core": core}))
            .await
    }

    pub async fn memory(&mut self, address: u32, length: u16) -> io::Result<Value> {
        self.operation(
            Method::ReadMemory,
            serde_json::json!({"address": address, "length": length}),
        )
        .await
    }

    pub async fn capture(&mut self) -> io::Result<Vec<Frame>> {
        self.capture_scaled(1).await
    }
    pub async fn capture_scaled(&mut self, scale: u32) -> io::Result<Vec<Frame>> {
        if !(1..=4).contains(&scale) {
            return Err(io::Error::other("capture scale 1..4"));
        }
        let data = self
            .operation(Method::Capture, serde_json::json!({"scale":scale}))
            .await?;
        let leases: Vec<FrameLease> =
            serde_json::from_value(data["frames"].clone()).map_err(io::Error::other)?;
        if leases.len() != 2 {
            return Err(io::Error::other(
                "capture must contain exactly two native screens",
            ));
        }
        leases
            .into_iter()
            .map(|lease| {
                let bgra = self.read_lease(&lease)?;
                Ok(Frame { lease, bgra })
            })
            .collect()
    }

    /// Screens for live display. Uses the worker's shared-memory ring when offered (no disk
    /// writes; on Vulkan possibly the previous request's frame, see `preview_ring`), else an
    /// explicit capture. Also returns the presented frame the pixels belong to, when known.
    /// Frames from an earlier epoch are refused.
    pub async fn preview(&mut self) -> io::Result<(Vec<Frame>, Option<u64>)> {
        if !self.shared_preview {
            return Ok((self.capture().await?, None));
        }
        let data = self.operation(Method::Preview, Value::Null).await?;
        let reply: crate::preview_ring::PreviewReply =
            serde_json::from_value(data).map_err(io::Error::other)?;
        if reply.frames.len() != 2 {
            return Err(io::Error::other("preview must contain exactly two screens"));
        }
        if self
            .preview_ring
            .as_ref()
            .is_none_or(|ring| ring.name != reply.name || ring.size() != reply.bytes)
        {
            self.preview_ring = None;
            self.preview_ring = Some(crate::preview_ring::Mapping::open(
                &reply.name,
                reply.bytes,
            )?);
        }
        let ring = self.preview_ring.as_ref().expect("opened above");
        let epoch = self.epoch();
        let mut frames = Vec::with_capacity(2);
        for lease in reply.frames {
            crate::preview_ring::validate(&lease, ring.size())?;
            if lease.epoch != epoch {
                return Err(io::Error::other("preview from an earlier epoch"));
            }
            if ring.bytes(lease.offset, crate::preview_ring::HEADER)
                != crate::preview_ring::header(&lease)
            {
                return Err(io::Error::other("stale or malformed preview slot"));
            }
            let bgra = ring
                .bytes(lease.offset + crate::preview_ring::HEADER, lease.bytes)
                .to_vec();
            frames.push(Frame {
                lease: FrameLease {
                    screen: lease.screen,
                    path: String::new(),
                    epoch: lease.epoch,
                    sequence: lease.sequence,
                    slot: lease.slot,
                    width: lease.width,
                    height: lease.height,
                    stride: lease.stride,
                    format: lease.format,
                    orientation: lease.orientation,
                    bytes: lease.bytes,
                },
                bgra,
            });
        }
        Ok((frames, Some(reply.frame)))
    }

    pub async fn stop(&mut self) -> io::Result<()> {
        let result = self.worker.stop().await;
        if result.is_err() {
            self.worker.abort().await;
        }
        self.mark_closed(if result.is_ok() { "closed" } else { "crashed" })?;
        result
    }

    /// Explicit failure injection and emergency termination, limited to this job.
    pub async fn abort(&mut self) {
        self.worker.abort().await;
        let _ = self.mark_closed("crashed");
    }

    fn mark_closed(&self, lifecycle: &str) -> io::Result<()> {
        let path = self.root.join("native-manifest.json");
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&path)?).map_err(io::Error::other)?;
        manifest["lifecycle"] = lifecycle.into();
        manifest["pid"] = Value::Null;
        fs::write(
            path,
            serde_json::to_vec_pretty(&manifest).map_err(io::Error::other)?,
        )
    }

    /// Terminates whichever process currently backs this session, without its lock.
    #[must_use]
    pub fn kill_switch(
        &self,
    ) -> std::sync::Arc<std::sync::Mutex<crate::controlled_worker::KillSwitch>> {
        self.kill.clone()
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn state(&self) -> SessionState {
        self.worker.state()
    }

    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.worker.epoch()
    }

    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.worker.pid()
    }

    #[must_use]
    pub fn working_set(&self) -> Option<u64> {
        self.worker.working_set()
    }

    pub fn read_lease(&self, lease: &FrameLease) -> io::Result<Vec<u8>> {
        if !matches!(
            self.worker.state(),
            SessionState::Paused | SessionState::Running
        ) || self.worker.epoch() != lease.epoch
        {
            return Err(io::Error::other("frame lease invalidated by lifecycle"));
        }
        read_frame(&self.root, lease)
    }

    async fn operation(&mut self, method: Method, params: Value) -> io::Result<Value> {
        let reply = self.worker.command(method, params).await?;
        self.finish(method, reply)
    }

    fn finish(&mut self, method: Method, reply: Reply) -> io::Result<Value> {
        let Reply {
            status, mut data, ..
        } = reply;
        if status != Status::Ok {
            return Err(io::Error::other(format!("{method:?}: {status:?}: {data}")));
        }
        if data.is_object() {
            data["profile_hash"] = self.profile_hash.clone().into();
            data["identity"] = self.identity.clone();
            data["epoch"] = self.epoch().into();
            self.decorate(&mut data);
        }
        Ok(data)
    }
}

/// Length and timestamps that identify one version of a file for [`file_hash`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
    created: Option<std::time::SystemTime>,
}

fn stamp(path: &Path) -> io::Result<Stamp> {
    let metadata = fs::metadata(path)?;
    Ok(Stamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        created: metadata.created().ok(),
    })
}

type HashCache = std::sync::Mutex<std::collections::HashMap<PathBuf, (Stamp, String)>>;

/// Worker binary hashes for this process, keyed by canonical path (#30). Every spawn of 4–16
/// matching workers used to read the whole ~86 MB executable. An entry is reused only while
/// length, modification and creation time are unchanged, and stored only if they did not
/// change during hashing, so a rebuilt worker is always rehashed. A rewrite that keeps the
/// length within one tick of the file system's timestamp resolution is not detected.
fn hash_cache() -> &'static HashCache {
    static CACHE: std::sync::OnceLock<HashCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(HashCache::default)
}

/// SHA-256 of a worker executable, cached per [`hash_cache`].
pub(crate) fn worker_hash(path: &Path) -> io::Result<String> {
    file_hash(path)
}

pub(crate) fn file_hash(path: &Path) -> io::Result<String> {
    let canonical = path.canonicalize()?;
    let before = stamp(&canonical)?;
    if let Some((cached, hash)) = hash_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&canonical)
    {
        if *cached == before {
            return Ok(hash.clone());
        }
    }
    let hash = hash_file(&canonical)?;
    if stamp(&canonical)? == before {
        hash_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(canonical, (before, hash.clone()));
    }
    Ok(hash)
}

fn hash_file(path: &Path) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

impl Drop for NativeSession {
    fn drop(&mut self) {
        if self.worker.state().is_active() {
            let _ = self.mark_closed("crashed");
        }
    }
}

/// A stale sequence, reset epoch, malformed header or path cannot expose pixels.
fn read_frame(root: &Path, lease: &FrameLease) -> io::Result<Vec<u8>> {
    let expected_width = match lease.screen.as_str() {
        "top" => 400,
        "bottom" => 320,
        _ => 0,
    };
    let expected_path = format!("frames/{}-{}.bgra", lease.screen, lease.sequence % 3);
    let scale = lease.height / 240;
    if expected_width == 0
        || !(1..=4).contains(&scale)
        || lease.width != expected_width * scale
        || lease.height != 240 * scale
        || lease.stride != lease.width * 4
        || lease.bytes != (lease.stride * lease.height) as usize
        || lease.slot != lease.sequence % 3
        || lease.path != expected_path
        || lease.format != "bgra8"
        || lease.orientation != "top_left_row_major"
    {
        return Err(io::Error::other("invalid frame lease"));
    }
    // Bound allocation before reading. The worker is serialized by mutable ownership.
    let path = root.join(expected_path);
    if fs::metadata(&path)?.len() != lease.bytes as u64 + 32 {
        return Err(io::Error::other("invalid frame size"));
    }
    let mut data = Vec::with_capacity(lease.bytes + 32);
    File::open(path)?
        .take(lease.bytes as u64 + 33)
        .read_to_end(&mut data)?;
    let mut expected = Vec::with_capacity(32);
    expected.extend_from_slice(b"LTRF");
    expected.extend_from_slice(&1u32.to_le_bytes());
    expected.extend_from_slice(&lease.epoch.to_le_bytes());
    expected.extend_from_slice(&lease.sequence.to_le_bytes());
    expected.extend_from_slice(&lease.width.to_le_bytes());
    expected.extend_from_slice(&lease.height.to_le_bytes());
    if data.len() != lease.bytes + 32 || data[..32] != expected {
        return Err(io::Error::other("stale or malformed frame slot"));
    }
    Ok(data[32..].to_vec())
}

// MinGW std::filesystem rejects Rust's verbatim canonical path syntax.
fn native_path(path: &Path) -> io::Result<PathBuf> {
    let absolute = path.canonicalize()?;
    #[cfg(windows)]
    {
        let text = absolute
            .to_str()
            .ok_or_else(|| io::Error::other("native path must be UTF-8"))?;
        if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            return Ok(PathBuf::from(format!(r"\\{unc}")));
        }
        if let Some(disk) = text.strip_prefix(r"\\?\") {
            return Ok(PathBuf::from(disk));
        }
    }
    Ok(absolute)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_hashes_follow_rewrites_of_the_same_length() {
        let path = std::env::temp_dir().join(format!(
            "lotus-hash-{}.bin",
            lotus_protocol::envelope::SessionUuid::generate()
        ));
        fs::write(&path, b"first build").expect("write");
        let first = file_hash(&path).expect("hash");
        assert_eq!(file_hash(&path).expect("cached"), first);
        assert_eq!(first, hash_file(&path).expect("direct"));
        std::thread::sleep(Duration::from_millis(50));
        fs::write(&path, b"other build").expect("rebuild");
        let second = file_hash(&path).expect("rehash");
        assert_ne!(second, first);
        assert_eq!(second, hash_file(&path).expect("direct"));
        let _ = fs::remove_file(path);
    }
}
