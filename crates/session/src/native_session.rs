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
}

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
        };
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
            "renderer":self.identity["renderer"],"paused":self.state()!=SessionState::Running,
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
        }
        if !profile.is_empty() {
            self.refresh_profile().await?;
        }
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
        self.operation(
            Method::Load,
            serde_json::json!({"path": native_path(path)?}),
        )
        .await
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
        self.operation(Method::Run, serde_json::json!({"frames": 600}))
            .await
    }

    pub async fn pause(&mut self) -> io::Result<Value> {
        self.operation(Method::Pause, Value::Null).await
    }

    pub async fn reset(&mut self) -> io::Result<Value> {
        self.operation(Method::Reset, Value::Null).await
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
        let Reply {
            status, mut data, ..
        } = self.worker.command(method, params).await?;
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

pub(crate) fn file_hash(path: &Path) -> io::Result<String> {
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
