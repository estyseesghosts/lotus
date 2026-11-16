//! Owned ELF/DWARF data from an explicitly approved build artifact.
use object::{Object, ObjectSection, ObjectSymbol};
use serde::Serialize;
use std::{collections::BTreeMap, io, sync::Arc};

type Reader = gimli::EndianArcSlice<gimli::RunTimeEndian>;
pub struct Symbols {
    context: addr2line::Context<Reader>,
    symbols: BTreeMap<u64, (String, u64)>,
}
#[derive(Debug, Serialize)]
pub struct Location {
    pub address: u32,
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
        Ok(Self { context, symbols })
    }
    pub fn locate(&self, address: u32) -> io::Result<Location> {
        let pc = u64::from(address & !1);
        let symbol = self
            .symbols
            .range(..=pc)
            .next_back()
            .filter(|(base, (_, size))| *size == 0 || pc < **base + size);
        let location = self.context.find_location(pc).map_err(io::Error::other)?;
        Ok(Location {
            address,
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
