//! Session-bound ELF/DWARF identity (lotus#76).
//!
//! An approved ELF is bound to a guest only after every read-only loaded section
//! (`SHF_ALLOC` without `SHF_WRITE`) reads back byte-for-byte from guest memory at its
//! link address. 3DSX code loads at `PROCESS_IMAGE_VADDR` with relocations resolved
//! against that base (`vendor/azahar/src/core/loader/3dsx.cpp`, `Load3DSXFile`), so a
//! matching ELF linked for that layout compares equal with a load bias of zero; any
//! other ELF, layout or rebuild differs and is refused. Relocated CRO modules are not
//! mapped: their addresses stay raw.
//!
//! A binding names the one guest process whose memory map covers every verified
//! section. It is dropped when the guest epoch changes (load, reset, state load,
//! replay) or when that process no longer maps those sections.
use crate::native_session::NativeSession;
use lotus_debugger::symbols::Symbols;
use lotus_protocol::debug::DebugCommand;
use serde_json::{json, Value};
use std::io;

/// Largest read the pinned workers accept (`CheckMemory` in `ctr_worker/debug.h`).
const READ_CHUNK: u32 = 4096;

pub(crate) struct DebugImage {
    pub index: Symbols,
    pub epoch: u64,
    pub elf_sha256: String,
    elf_asset: String,
    process: u32,
    generation: u64,
    content: Value,
}

impl DebugImage {
    /// Compare the ELF with guest memory and name the process that maps it.
    pub(crate) async fn bind(
        native: &mut NativeSession,
        elf_asset: &str,
        index: Symbols,
        elf_sha256: String,
        generation: u64,
        check: &(dyn Fn() -> io::Result<()> + Sync),
    ) -> io::Result<Self> {
        if !matches!(
            native.state(),
            crate::SessionState::Running | crate::SessionState::Paused
        ) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "load content before binding an ELF to this guest",
            ));
        }
        let epoch = native.epoch();
        for (address, expected) in index.verification_ranges() {
            let name = section_name(&index, address);
            for (chunk, expected) in expected.chunks(READ_CHUNK as usize).enumerate() {
                check()?;
                let at = address + chunk as u32 * READ_CHUNK;
                let read = native
                    .debug(&DebugCommand::ReadMemory {
                        address: at,
                        length: expected.len() as u32,
                    })
                    .await
                    .map_err(|error| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "ELF section {name} at {at:#010x} is not readable in the active guest process: {error}"
                            ),
                        )
                    })?;
                let actual: Vec<u8> =
                    serde_json::from_value(read["bytes"].clone()).map_err(io::Error::other)?;
                if actual.len() != expected.len() {
                    return Err(io::Error::other("short guest memory read"));
                }
                if let Some(offset) = actual.iter().zip(expected).position(|(a, e)| a != e) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "ELF section {name} differs from guest memory at {:#010x}: this ELF is not the loaded build",
                            at + offset as u32
                        ),
                    ));
                }
            }
        }
        if native.epoch() != epoch {
            return Err(io::Error::other("guest changed during ELF verification"));
        }
        let map = native.debug(&DebugCommand::MemoryMap).await?;
        let owners = owners(&index, &map["regions"]);
        let [process] = owners[..] else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "verified sections must belong to exactly one guest process; found {owners:?}"
                ),
            ));
        };
        Ok(Self {
            index,
            epoch,
            elf_sha256,
            elf_asset: elf_asset.to_string(),
            process,
            generation,
            content: native.content_identity().clone(),
        })
    }

    /// Why this binding no longer describes the guest, if it doesn't.
    pub(crate) fn stale(&self, epoch: u64, regions: &Value) -> Option<String> {
        if epoch != self.epoch {
            return Some(format!(
                "guest epoch {epoch} replaced the bound epoch {}",
                self.epoch
            ));
        }
        (!owners(&self.index, regions).contains(&self.process)).then(|| {
            format!(
                "guest process {} no longer maps the bound ELF sections",
                self.process
            )
        })
    }

    /// Identity and mapping evidence attached to every symbolized reply.
    pub(crate) fn describe(&self) -> Value {
        json!({
            "elf_asset": self.elf_asset,
            "elf_sha256": self.elf_sha256,
            "build_id": self.index.build_id(),
            "content": self.content,
            "epoch": self.epoch,
            "process": self.process,
            "generation": self.generation,
            "mapping": "verified_bytes",
            "load_bias": 0,
        })
    }
}

/// A session's binding, plus why the last one was dropped.
#[derive(Default)]
pub(crate) struct SymbolSlot {
    pub image: Option<DebugImage>,
    dropped: Option<String>,
}

impl SymbolSlot {
    pub(crate) fn bind(&mut self, image: DebugImage) {
        self.image = Some(image);
        self.dropped = None;
    }

    /// Why no binding is available, including why the last one was dropped.
    pub(crate) fn missing(&self) -> io::Error {
        io::Error::new(
            io::ErrorKind::NotFound,
            match &self.dropped {
                Some(reason) => {
                    format!("ELF binding dropped: {reason}. Load the matching ELF again")
                }
                None => "load an approved ELF build artifact for this guest first".into(),
            },
        )
    }

    pub(crate) fn dropped(&self) -> Option<&str> {
        self.dropped.as_deref()
    }
}

/// Drop a binding that no longer describes the guest. Returns whether one remains.
/// The caller holds the session's native lock, so no other request rebinds meanwhile.
pub(crate) async fn validate(
    slot: &std::sync::Mutex<SymbolSlot>,
    native: &mut NativeSession,
) -> io::Result<bool> {
    let lock = || {
        slot.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    let epoch = native.epoch();
    {
        let mut slot = lock();
        let Some(image) = &slot.image else {
            return Ok(false);
        };
        if image.epoch != epoch {
            let reason = image.stale(epoch, &Value::Null);
            slot.image = None;
            slot.dropped = reason;
            return Ok(false);
        }
    }
    let map = native.debug(&DebugCommand::MemoryMap).await?;
    let mut slot = lock();
    let reason = slot
        .image
        .as_ref()
        .and_then(|image| image.stale(epoch, &map["regions"]));
    if reason.is_some() {
        slot.image = None;
        slot.dropped = reason;
    }
    Ok(slot.image.is_some())
}

fn section_name(index: &Symbols, address: u32) -> String {
    index
        .sections()
        .iter()
        .find(|s| s.address == address)
        .map_or_else(|| format!("{address:#x}"), |s| s.name.clone())
}

/// Processes whose mapped regions cover every verified section.
fn owners(index: &Symbols, regions: &Value) -> Vec<u32> {
    let mut regions: Vec<(u32, u64, u64)> = regions
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let start = r["address"].as_u64()?;
            Some((
                u32::try_from(r["process"].as_u64()?).ok()?,
                start,
                start.checked_add(r["length"].as_u64()?)?,
            ))
        })
        .collect();
    regions.sort_unstable();
    let mut processes: Vec<u32> = regions.iter().map(|r| r.0).collect();
    processes.dedup();
    processes.retain(|process| {
        index.verification_ranges().all(|(address, bytes)| {
            let (mut at, end) = (u64::from(address), u64::from(address) + bytes.len() as u64);
            for &(_, start, stop) in regions.iter().filter(|r| r.0 == *process) {
                if start <= at && at < stop {
                    at = stop;
                }
            }
            at >= end
        })
    });
    processes
}
