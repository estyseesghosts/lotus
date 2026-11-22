//! Owned ELF/DWARF data from an explicitly approved build artifact.
//!
//! An ELF only describes a running guest after its loaded bytes have been compared
//! with guest memory. [`Symbols::verification_ranges`] lists the read-only allocated
//! sections to compare; [`Symbols::locate`] refuses addresses outside allocated sections
//! so a nearby label is never reported for heap, stack or another module.
use object::{Object, ObjectSection, ObjectSymbol, SectionFlags, SectionKind};
use serde::Serialize;
use std::{collections::BTreeMap, io, sync::Arc};

type Reader = gimli::EndianArcSlice<gimli::RunTimeEndian>;

/// Read-only bytes compared with guest memory; larger images are refused.
pub const MAX_VERIFIED_BYTES: usize = 16 << 20;

use object::elf::{SHF_ALLOC, SHF_EXECINSTR, SHF_WRITE};

pub struct Symbols {
    context: addr2line::Context<Reader>,
    symbols: BTreeMap<u64, (String, u64)>,
    sections: Vec<ImageSection>,
    verify: Vec<(u32, Arc<[u8]>)>,
    build_id: Option<String>,
}

/// One allocated ELF section at its link address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImageSection {
    pub name: String,
    pub address: u32,
    pub size: u32,
    pub writable: bool,
    pub executable: bool,
    /// Read-only section bytes the host compares with guest memory.
    pub verified: bool,
}

#[derive(Debug, Serialize)]
pub struct Location {
    pub address: u32,
    /// Allocated ELF section containing the address; `None` means the address is
    /// outside this image and nothing else is reported.
    pub section: Option<String>,
    pub symbol: Option<String>,
    pub offset: Option<u64>,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

impl Symbols {
    pub fn parse(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() > 128 << 20 {
            return Err(io::Error::other("ELF exceeds 128 MiB"));
        }
        let file = object::File::parse(bytes).map_err(io::Error::other)?;
        if file.format() != object::BinaryFormat::Elf
            || file.architecture() != object::Architecture::Arm
            || !file.is_little_endian()
        {
            return Err(io::Error::other(
                "expected a little-endian ARM ELF build artifact",
            ));
        }
        let dwarf = gimli::Dwarf::load(|section| -> io::Result<Reader> {
            let data = file
                .section_by_name(section.name())
                .map(|s| s.uncompressed_data())
                .transpose()
                .map_err(io::Error::other)?
                .unwrap_or_default();
            if data.len() > 64 << 20 {
                return Err(io::Error::other("DWARF section exceeds budget"));
            }
            Ok(Reader::new(
                Arc::from(data.as_ref()),
                gimli::RunTimeEndian::Little,
            ))
        })?;
        let context = addr2line::Context::from_dwarf(dwarf).map_err(io::Error::other)?;
        let mut sections = Vec::new();
        let mut verify = Vec::new();
        let mut verified_bytes = 0usize;
        for section in file.sections() {
            let SectionFlags::Elf { sh_flags, .. } = section.flags() else {
                continue;
            };
            if !sh_flags.contains(SHF_ALLOC) || section.size() == 0 {
                continue;
            }
            let (Ok(address), Ok(size)) = (
                u32::try_from(section.address()),
                u32::try_from(section.size()),
            ) else {
                return Err(io::Error::other(
                    "allocated section outside the 32-bit space",
                ));
            };
            if u64::from(address) + u64::from(size) > 1 << 32 {
                return Err(io::Error::other(
                    "allocated section outside the 32-bit space",
                ));
            }
            let writable = sh_flags.contains(SHF_WRITE);
            let loaded = section.kind() != SectionKind::UninitializedData;
            let verified = !writable && loaded;
            if verified {
                let data = section.data().map_err(io::Error::other)?;
                if data.len() != size as usize {
                    return Err(io::Error::other("section data does not match its size"));
                }
                verified_bytes += data.len();
                if verified_bytes > MAX_VERIFIED_BYTES {
                    return Err(io::Error::other(
                        "read-only sections exceed the 16 MiB verification budget",
                    ));
                }
                verify.push((address, Arc::from(data)));
            }
            sections.push(ImageSection {
                name: section.name().unwrap_or_default().to_string(),
                address,
                size,
                writable,
                executable: sh_flags.contains(SHF_EXECINSTR),
                verified,
            });
        }
        if verify.is_empty() {
            return Err(io::Error::other(
                "ELF has no read-only loaded section to verify against the guest",
            ));
        }
        sections.sort_by_key(|s| s.address);
        if sections
            .windows(2)
            .any(|w| u64::from(w[0].address) + u64::from(w[0].size) > u64::from(w[1].address))
        {
            return Err(io::Error::other("allocated ELF sections overlap"));
        }
        let mut symbols = BTreeMap::new();
        for symbol in file
            .symbols()
            .filter(|s| s.is_definition() && s.address() <= u64::from(u32::MAX))
        {
            if let Ok(name) = symbol.name() {
                if !name.is_empty() && !name.starts_with('$') {
                    symbols.insert(symbol.address() & !1, (name.to_string(), symbol.size()));
                }
            }
        }
        let build_id = file
            .build_id()
            .ok()
            .flatten()
            .map(|id| id.iter().map(|b| format!("{b:02x}")).collect());
        Ok(Self {
            context,
            symbols,
            sections,
            verify,
            build_id,
        })
    }

    /// GNU build ID note, when the linker emitted one.
    pub fn build_id(&self) -> Option<&str> {
        self.build_id.as_deref()
    }

    /// Allocated sections, sorted by address.
    pub fn sections(&self) -> &[ImageSection] {
        &self.sections
    }

    /// Read-only loaded bytes at their link addresses.
    pub fn verification_ranges(&self) -> impl Iterator<Item = (u32, &[u8])> {
        self.verify
            .iter()
            .map(|(address, bytes)| (*address, &**bytes))
    }

    fn section(&self, address: u32) -> Option<&ImageSection> {
        self.sections.iter().find(|s| {
            address >= s.address && u64::from(address) < u64::from(s.address) + u64::from(s.size)
        })
    }

    pub fn locate(&self, address: u32) -> io::Result<Location> {
        let pc = u64::from(address & !1);
        let Some(section) = self.section(address & !1) else {
            return Ok(Location {
                address,
                section: None,
                symbol: None,
                offset: None,
                file: None,
                line: None,
                column: None,
            });
        };
        let start = u64::from(section.address);
        let symbol = self
            .symbols
            .range(start..=pc)
            .next_back()
            .filter(|(base, (_, size))| *size == 0 || pc < **base + size);
        let location = if section.executable {
            self.context.find_location(pc).map_err(io::Error::other)?
        } else {
            None
        };
        Ok(Location {
            address,
            section: Some(section.name.clone()),
            symbol: symbol.map(|(_, (name, _))| name.clone()),
            offset: symbol.map(|(base, _)| pc - base),
            file: location.as_ref().and_then(|l| l.file.map(str::to_string)),
            line: location.as_ref().and_then(|l| l.line),
            column: location.and_then(|l| l.column),
        })
    }

    pub fn resolve(&self, query: &str) -> io::Result<Vec<u32>> {
        if query.is_empty() || query.len() > 1024 {
            return Err(io::Error::other("invalid symbol query"));
        }
        if let Some((path, line)) = query
            .rsplit_once(':')
            .filter(|(_, s)| s.parse::<u32>().is_ok())
        {
            let line = line.parse::<u32>().map_err(io::Error::other)?;
            let mut result = Vec::new();
            for (address, _, location) in self
                .context
                .find_location_range(0, 1u64 << 32)
                .map_err(io::Error::other)?
                .take(1_000_000)
            {
                if location.line == Some(line)
                    && location.file.is_some_and(|file| {
                        file.replace('\\', "/").ends_with(&path.replace('\\', "/"))
                    })
                {
                    result.push(address as u32);
                    if result.len() == 128 {
                        break;
                    }
                }
            }
            return Ok(result);
        }
        Ok(self
            .symbols
            .iter()
            .filter(|(_, (name, _))| name == query)
            .take(128)
            .map(|(address, _)| *address as u32)
            .collect())
    }

    pub fn count(&self) -> usize {
        self.symbols.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_non_elf_and_host_elf() {
        assert!(Symbols::parse(b"not an ELF").is_err());
        assert!(Symbols::parse(&[0; 52]).is_err());
    }
}
