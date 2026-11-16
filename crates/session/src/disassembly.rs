//! Bounded ARM/Thumb decoding through the host's LLVM tools; no guest code runs.
use std::{io, path::Path};
use tokio::process::Command;

/// A tiny ELF object lets llvm-objdump decode arbitrary captured bytes faithfully.
fn elf(address: u32, bytes: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; 52];
    out[..7].copy_from_slice(b"\x7fELF\x01\x01\x01");
    out[16..18].copy_from_slice(&1u16.to_le_bytes());
    out[18..20].copy_from_slice(&40u16.to_le_bytes());
    out[20..24].copy_from_slice(&1u32.to_le_bytes());
    out[36..40].copy_from_slice(&0x05000000u32.to_le_bytes());
    out[40..42].copy_from_slice(&52u16.to_le_bytes());
    out[46..48].copy_from_slice(&40u16.to_le_bytes());
    out[48..50].copy_from_slice(&3u16.to_le_bytes());
    out[50..52].copy_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(bytes);
    let names_offset = out.len() as u32;
    let names = b"\x00.text\x00.shstrtab\x00";
    out.extend_from_slice(names);
    while out.len() % 4 != 0 {
        out.push(0);
    }
    let table = out.len() as u32;
    out[32..36].copy_from_slice(&table.to_le_bytes());
    out.extend_from_slice(&[0; 40]);
    for fields in [
        [1, 1, 6, address, 52, bytes.len() as u32, 0, 0, 4, 0],
        [7, 3, 0, 0, names_offset, names.len() as u32, 0, 0, 1, 0],
    ] {
        for word in fields {
            out.extend_from_slice(&word.to_le_bytes());
        }
    }
    out
}

pub async fn decode(root: &Path, address: u32, bytes: &[u8], thumb: bool) -> io::Result<String> {
    if bytes.is_empty() || bytes.len() > 4096 {
        return Err(io::Error::other("decode budget 1..4096 bytes"));
    }
    let path = root.join(format!(
        "decode-{}.o",
        lotus_protocol::envelope::SessionUuid::generate()
    ));
    std::fs::write(&path, elf(address, bytes))?;
    let mut command = Command::new("llvm-objdump");
    command
        .args([
            "--disassemble",
            if thumb {
                "--triple=thumbv6k-none-eabi"
            } else {
                "--triple=armv6k-none-eabi"
            },
            "--mcpu=mpcore",
        ])
        .arg(&path)
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), command.output()).await;
    let _ = std::fs::remove_file(path);
    let output =
        result.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "LLVM decode deadline"))??;
    if !output.status.success() || output.stdout.len() > 131072 {
        return Err(io::Error::other(
            "LLVM ARM disassembler failed; install llvm-objdump on the host PATH",
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| {
            line.trim_start()
                .split_once(':')
                .is_some_and(|(address, _)| u32::from_str_radix(address, 16).is_ok())
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn arm_and_thumb_decode_the_actual_bytes() {
        if Command::new("llvm-objdump")
            .arg("--version")
            .output()
            .await
            .is_err()
        {
            return;
        }
        let root = std::env::temp_dir();
        let arm = decode(&root, 0x100000, &[0xfe, 0xff, 0xff, 0xea], false)
            .await
            .unwrap();
        assert!(arm.contains("100000:") && arm.contains("b\t"), "{arm}");
        let thumb = decode(&root, 0x100000, &[0xfe, 0xe7], true).await.unwrap();
        assert!(
            thumb.contains("100000:") && thumb.contains("b\t"),
            "{thumb}"
        );
    }
}
