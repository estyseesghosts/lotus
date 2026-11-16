//! Explicitly approved private assets, copied only to fixed native locations.
use lotus_protocol::debug::AssetKind;
use std::{fs, io, path::Path};

pub fn mount(root: &Path, source: &Path, kind: AssetKind) -> io::Result<()> {
    let (relative, maximum) = match kind {
        AssetKind::DspFirmware => ("sdmc/3ds/dspfirm.cdc", 4 << 20),
        AssetKind::Otp => ("sysdata/otp.bin", 0x100),
        AssetKind::SecureInfo => ("nand/rw/sys/SecureInfo_A", 4096),
        AssetKind::LocalFriendCodeSeed => ("nand/rw/sys/LocalFriendCodeSeed_B", 4096),
        AssetKind::Movable => ("nand/private/movable.sed", 4096),
        AssetKind::MicWav => ("media/mic.wav", 64 << 20),
        AssetKind::Nfc => ("media/nfc.bin", 128 << 10),
        AssetKind::Cia => ("media/content.cia", 256 << 20),
        AssetKind::Rom => ("media/content.rom", 32 << 20),
    };
    let size = fs::metadata(source)?.len();
    if size == 0 || size > maximum {
        return Err(io::Error::other(
            "private asset exceeds its type's size limit",
        ));
    }
    let bytes = fs::read(source)?;
    if matches!(kind, AssetKind::DspFirmware) {
        validate_dsp(&bytes)?;
    }
    let root = root.canonicalize()?;
    let destination = root.join(relative);
    let parent = destination.parent().expect("fixed path has parent");
    // Check each existing component before creating anything under it.
    let mut checked = root.clone();
    for component in Path::new(relative)
        .parent()
        .expect("fixed parent")
        .components()
    {
        checked.push(component);
        if checked.exists() && !checked.canonicalize()?.starts_with(&root) {
            return Err(io::Error::other("asset destination escaped session root"));
        }
    }
    fs::create_dir_all(parent)?;
    if !parent.canonicalize()?.starts_with(&root)
        || fs::symlink_metadata(&destination).is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(io::Error::other("asset destination escaped session root"));
    }
    fs::write(destination, bytes)
}

fn validate_dsp(bytes: &[u8]) -> io::Result<()> {
    let invalid = || io::Error::other("invalid DSP1 firmware asset");
    if bytes.len() < 0x300 || &bytes[0x100..0x104] != b"DSP1" || bytes[0x10e] > 10 {
        return Err(invalid());
    }
    let size = u32::from_le_bytes(bytes[0x104..0x108].try_into().map_err(|_| invalid())?) as usize;
    if size > bytes.len() || size < 0x300 {
        return Err(invalid());
    }
    for index in 0..usize::from(bytes[0x10e]) {
        let start = 0x120 + index * 0x30;
        let offset =
            u32::from_le_bytes(bytes[start..start + 4].try_into().map_err(|_| invalid())?) as u64;
        let length = u32::from_le_bytes(
            bytes[start + 8..start + 12]
                .try_into()
                .map_err(|_| invalid())?,
        ) as u64;
        if offset + length > size as u64 {
            return Err(invalid());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_truncated_or_out_of_bounds_dsp_segments() {
        assert!(validate_dsp(&[]).is_err());
        let mut data = vec![0; 0x300];
        data[0x100..0x104].copy_from_slice(b"DSP1");
        data[0x104..0x108].copy_from_slice(&0x300u32.to_le_bytes());
        assert!(validate_dsp(&data).is_ok());
        data[0x10e] = 1;
        data[0x120..0x124].copy_from_slice(&0x301u32.to_le_bytes());
        assert!(validate_dsp(&data).is_err());
    }
}
