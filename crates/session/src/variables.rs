//! Variables of a stopped guest from the bound ELF's DWARF (lotus#78).
//!
//! Values are read only while the worker reports the guest paused. The stop sequence
//! and epoch are compared before and after the read; if the guest ran or changed state in
//! between, the reply is refused instead of mixing values from two states. Every reply
//! names the ELF binding, the process address space, and the stop it describes.
use crate::debug_image::SymbolSlot;
use crate::native_session::NativeSession;
use lotus_debugger::variables::{Inspect, Memory, Query, Registers};
use lotus_protocol::debug::DebugCommand as D;
use serde_json::{json, Value};
use std::io;
use std::sync::Mutex;

struct GuestMemory<'a> {
    native: &'a mut NativeSession,
}

impl Memory for GuestMemory<'_> {
    async fn read(&mut self, address: u32, length: u32) -> io::Result<Vec<u8>> {
        let reply = self
            .native
            .debug(&D::ReadMemory { address, length })
            .await?;
        serde_json::from_value(reply["bytes"].clone()).map_err(io::Error::other)
    }
}

fn word(value: &Value) -> Option<u32> {
    value.as_u64().and_then(|v| u32::try_from(v).ok())
}

pub(crate) async fn inspect(
    slot: &Mutex<SymbolSlot>,
    native: &mut NativeSession,
    core: u32,
    frame: u32,
    depth: u32,
    query: Query,
) -> io::Result<Value> {
    let image = crate::source_debug::bound(slot, native).await?;
    let epoch = native.epoch();
    let before = native.status().await?;
    if before["paused"] != true {
        return Err(io::Error::other(
            "variables are read only from a paused guest; pause it first",
        ));
    }
    let sequence = before["stop"]["sequence"].clone();
    let registers = native.debug(&D::Registers { core }).await?;
    let mut core_registers = [None; 16];
    for (index, slot) in core_registers.iter_mut().enumerate() {
        *slot = word(&registers["registers"][index]);
    }
    let pc = core_registers[15].ok_or_else(|| io::Error::other("registers reply has no pc"))?;
    let thumb = registers["cpsr"]
        .as_u64()
        .ok_or_else(|| io::Error::other("registers reply has no cpsr"))?
        & 0x20
        != 0;
    let vfp = registers["vfp"]
        .as_array()
        .map(|words| words.iter().map(word).collect())
        .unwrap_or_default();
    let hidden = crate::source_debug::presented_hidden(slot, epoch, pc, &sequence);
    let mut memory = GuestMemory { native };
    let mut reply = image
        .index
        .inspect(
            &mut memory,
            Registers {
                core: core_registers,
                vfp,
            },
            thumb,
            Inspect {
                frame,
                hidden,
                depth,
                query,
            },
        )
        .await?;
    let after = native.status().await?;
    if native.epoch() != epoch || after["paused"] != true || after["stop"]["sequence"] != sequence {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "the guest ran or changed state during the read; the values were discarded",
        ));
    }
    let describe = image.describe();
    reply["consistency"] = json!({
        "paused": true,
        "epoch": epoch,
        "stop_sequence": sequence,
        "core": core,
        "thread": registers["thread"],
    });
    reply["address_space"] = json!({"kind": "guest_virtual", "process": describe["process"]});
    reply["image"] = describe;
    Ok(reply)
}
