//! Owned ELF/DWARF data from an explicitly approved build artifact.
//!
//! An ELF only describes a running guest after its loaded bytes have been compared
//! with guest memory. [`Symbols::verification_ranges`] lists the read-only allocated
//! sections to compare; [`Symbols::locate`] refuses addresses outside allocated sections
//! so a nearby label is never reported for heap, stack or another module.
use crate::lines::{LineTable, SourceFile};
use object::{Object, ObjectSection, ObjectSymbol, SectionFlags, SectionKind, SymbolKind};
use serde::Serialize;
use std::{collections::BTreeMap, io, sync::Arc};

type Reader = gimli::EndianArcSlice<gimli::RunTimeEndian>;

/// Read-only bytes compared with guest memory; larger images are refused.
pub const MAX_VERIFIED_BYTES: usize = 16 << 20;

use object::elf::{SHF_ALLOC, SHF_EXECINSTR, SHF_WRITE};

pub struct Symbols {
    /// `Context` caches parsed units in `OnceCell`s and is not `Sync`; the mutex lets
    /// a bound image be shared across the supervisor's async tasks.
    context: std::sync::Mutex<addr2line::Context<Reader>>,
    symbols: BTreeMap<u64, (String, u64)>,
    /// Function start -> (end, name) for sized `STT_FUNC` symbols.
    functions: BTreeMap<u64, (u64, String)>,
    /// AAELF mapping symbols: address -> Thumb (`$t`) or ARM (`$a`); `$d` data is `None`.
    modes: BTreeMap<u64, Option<bool>>,
    lines: LineTable,
    /// Inlined-subroutine entry address -> how many inlined subroutines begin there.
    inline_entries: BTreeMap<u32, u32>,
    /// `.debug_frame` call-frame information, when the build has it.
    debug_frame: Option<gimli::DebugFrame<Reader>>,
    sections: Vec<ImageSection>,
    verify: Vec<(u32, Arc<[u8]>)>,
    build_id: Option<String>,
}

/// The source position of one frame at an address: the innermost frame's line, or for
/// an outer frame the call site of the frame it inlines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameView {
    pub file: String,
    pub line: u32,
    pub column: u32,
    /// Inlined frames above this one's concrete function.
    pub depth: usize,
    /// Frames skipped from the innermost one.
    pub hidden: u32,
}

/// Each frame's function name, with the call-site line and column of a frame that
/// inlines another.
type InlineChain = Vec<(String, Option<(u32, u32)>)>;

/// Where a frame's return address is at a PC, from call-frame information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnAddress {
    /// Still in this core register.
    Register(u16),
    /// Saved in memory at CFA + offset.
    Saved(i64),
}

/// The canonical frame address rule and return-address location at a PC (DWARF 5, 6.4).
/// On ARM the CFA is the caller's SP at the call, so SP equals it after the return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRule {
    pub cfa_register: u16,
    pub cfa_offset: i64,
    pub return_address: ReturnAddress,
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
        let lines = LineTable::parse(&dwarf)?;
        let inline_entries = crate::lines::inline_entries(&dwarf)?;
        let debug_frame = file
            .section_by_name(".debug_frame")
            .map(|section| section.uncompressed_data())
            .transpose()
            .map_err(io::Error::other)?
            .filter(|data| !data.is_empty())
            .map(|data| {
                let mut frame = gimli::DebugFrame::from(Reader::new(
                    Arc::from(data.as_ref()),
                    gimli::RunTimeEndian::Little,
                ));
                frame.set_address_size(4);
                frame
            });
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
        let mut functions = BTreeMap::new();
        let mut modes = BTreeMap::new();
        for symbol in file
            .symbols()
            .filter(|s| s.address() <= u64::from(u32::MAX))
        {
            let Ok(name) = symbol.name() else {
                continue;
            };
            // ARM ELF (AAELF32) 5.5.5: $a, $t and $d mark the instruction set that follows.
            let mapping = name.split('.').next().unwrap_or_default();
            if matches!(mapping, "$a" | "$t" | "$d") {
                modes.insert(
                    symbol.address(),
                    (mapping != "$d").then_some(mapping == "$t"),
                );
                continue;
            }
            if !symbol.is_definition() || name.is_empty() {
                continue;
            }
            let start = symbol.address() & !1;
            symbols.insert(start, (name.to_string(), symbol.size()));
            if symbol.kind() == SymbolKind::Text && symbol.size() > 0 {
                functions.insert(start, (start + symbol.size(), name.to_string()));
            }
        }
        let build_id = file
            .build_id()
            .ok()
            .flatten()
            .map(|id| id.iter().map(|b| format!("{b:02x}")).collect());
        Ok(Self {
            context: std::sync::Mutex::new(context),
            symbols,
            functions,
            modes,
            lines,
            inline_entries,
            debug_frame,
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

    fn context(&self) -> std::sync::MutexGuard<'_, addr2line::Context<Reader>> {
        self.context
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
            let context = self.context();
            context
                .find_location(pc)
                .map_err(io::Error::other)?
                .map(|l| (l.file.map(str::to_string), l.line, l.column))
        } else {
            None
        };
        Ok(Location {
            address,
            section: Some(section.name.clone()),
            symbol: symbol.map(|(_, (name, _))| name.clone()),
            offset: symbol.map(|(base, _)| pc - base),
            file: location.as_ref().and_then(|l| l.0.clone()),
            line: location.as_ref().and_then(|l| l.1),
            column: location.and_then(|l| l.2),
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
            return Ok(self
                .lines
                .matching_files(path)
                .into_iter()
                .map(|file| self.line_sites(file, line))
                .collect::<io::Result<Vec<_>>>()?
                .into_iter()
                .flatten()
                .take(128)
                .collect());
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

    /// Source file, line and whether `address` starts a statement, from the line table.
    pub fn line_at(&self, address: u32) -> Option<(&SourceFile, u32, bool)> {
        let (file, at) = self.line_row(address)?;
        Some((file, at.line, at.statement_start))
    }

    /// The line-table row covering `address` in an executable section.
    pub fn line_row(&self, address: u32) -> Option<(&SourceFile, crate::lines::LineAt)> {
        self.section(address & !1).filter(|s| s.executable)?;
        let at = self.lines.at(address & !1)?;
        Some((self.lines.file(at.file)?, at))
    }

    /// `[start, end)` and name of the sized function symbol containing `address`.
    pub fn function_at(&self, address: u32) -> Option<(u32, u32, &str)> {
        let pc = u64::from(address & !1);
        self.functions
            .range(..=pc)
            .next_back()
            .filter(|(_, (end, _))| pc < *end)
            .map(|(start, (end, name))| (*start as u32, *end as u32, name.as_str()))
    }

    /// The post-prologue stop for a function entered at `address`, when `address` is a
    /// sized function's first instruction and the line table places it later.
    pub fn after_prologue(&self, address: u32) -> Option<u32> {
        let (start, end, _) = self.function_at(address)?;
        (start == address & !1)
            .then(|| self.lines.after_prologue(start, end))?
            .filter(|target| *target != start)
    }

    /// Instruction set at `address` from mapping symbols; `None` for data or unknown.
    pub fn thumb_at(&self, address: u32) -> Option<bool> {
        let pc = u64::from(address & !1);
        let (start, mode) = self.modes.range(..=pc).next_back()?;
        // A mapping symbol only applies within its own section.
        let section = self.section(address & !1)?;
        (*start >= u64::from(section.address)).then_some(*mode)?
    }

    /// Inlined frames above the concrete function at `address` (0 when not inlined).
    pub fn inline_depth(&self, address: u32) -> io::Result<usize> {
        let context = self.context();
        let mut frames = context
            .find_frames(u64::from(address & !1))
            .skip_all_loads()
            .map_err(io::Error::other)?;
        let mut count = 0usize;
        while frames.next().map_err(io::Error::other)?.is_some() {
            count += 1;
        }
        Ok(count.saturating_sub(1))
    }

    /// Statement starts of `file:line`, with each address's instruction set.
    /// Several files matching `file` is an error naming them.
    pub fn source_statements(
        &self,
        file: &str,
        line: u32,
    ) -> io::Result<(&SourceFile, Vec<(u32, bool)>)> {
        if self.lines.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the bound ELF has no DWARF line table; source breakpoints need a -g build",
            ));
        }
        let files = self.lines.matching_files(file);
        let [id] = files[..] else {
            return Err(io::Error::new(
                if files.is_empty() {
                    io::ErrorKind::NotFound
                } else {
                    io::ErrorKind::InvalidInput
                },
                if files.is_empty() {
                    format!("{file} is not in this ELF's line table")
                } else {
                    format!(
                        "{file} matches {} files; use a longer path: {}",
                        files.len(),
                        files
                            .iter()
                            .take(8)
                            .filter_map(|id| self.lines.file(*id))
                            .map(|f| f.path.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                },
            ));
        };
        let source = self
            .lines
            .file(id)
            .ok_or_else(|| io::Error::other("file index"))?;
        let mut addresses = Vec::new();
        for address in self.line_sites(id, line)? {
            let thumb = self.thumb_at(address).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("no ARM/Thumb mapping symbol covers {address:#010x}"),
                )
            })?;
            addresses.push((address, thumb));
        }
        if addresses.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{}:{line} has no statement in this build", source.path),
            ));
        }
        Ok((source, addresses))
    }

    /// Breakpoint sites of `line`: its statement starts, and the entries of subroutines
    /// inlined from it (`DW_AT_call_line`; GCC may emit no row for the call line before
    /// the inlined body). One site is kept per scope: a line resumed after an inlined
    /// call gets a second `is_stmt` row, and like gdb (`linespec.c`,
    /// `create_sals_line_offset` keeps one location per block) only its lowest address
    /// is a site. Separate inlined copies stay separate.
    fn line_sites(&self, file: u32, line: u32) -> io::Result<Vec<u32>> {
        let path = self
            .lines
            .file(file)
            .map(|f| f.path.clone())
            .unwrap_or_default();
        let mut candidates: Vec<(u32, u32)> = self
            .lines
            .statement_starts(file, line)
            .into_iter()
            .map(|address| (address, 0))
            .collect();
        for (&address, &count) in &self.inline_entries {
            let view = self.frame_view(address, count)?;
            if view.is_some_and(|view| view.line == line && view.file == path) {
                candidates.push((address, count));
            }
        }
        candidates.sort_unstable();
        candidates.dedup_by_key(|(address, _)| *address);
        let mut seen = Vec::new();
        let mut sites = Vec::new();
        for (address, hidden) in candidates {
            let key = (
                self.function_at(address).map(|(start, _, _)| start),
                self.inline_context(address, hidden)?,
            );
            if !seen.contains(&key) {
                seen.push(key);
                sites.push(address);
            }
        }
        Ok(sites)
    }

    /// How to find the caller of the frame executing at `address`, from `.debug_frame`.
    /// `None` when no FDE covers it or the rules need DWARF expressions. An unspecified
    /// return-address rule means the register still holds it, as gdb's `dwarf2-frame.c`
    /// treats unspecified registers as unchanged.
    pub fn frame_rule(&self, address: u32) -> io::Result<Option<FrameRule>> {
        use gimli::UnwindSection as _;
        let Some(frame) = &self.debug_frame else {
            return Ok(None);
        };
        let bases = gimli::BaseAddresses::default();
        let pc = u64::from(address & !1);
        let fde = match frame.fde_for_address(&bases, pc, gimli::DebugFrame::cie_from_offset) {
            Ok(fde) => fde,
            Err(gimli::Error::NoUnwindInfoForAddress) => return Ok(None),
            Err(error) => return Err(io::Error::other(error)),
        };
        let mut context = Box::new(gimli::UnwindContext::new());
        let row = fde
            .unwind_info_for_address(frame, &bases, &mut context, pc)
            .map_err(io::Error::other)?;
        let gimli::CfaRule::RegisterAndOffset { register, offset } = row.cfa().clone() else {
            return Ok(None);
        };
        let column = fde.cie().return_address_register();
        let return_address = match row.register(column) {
            None | Some(gimli::RegisterRule::SameValue) => ReturnAddress::Register(column.0),
            Some(gimli::RegisterRule::Register(register)) => ReturnAddress::Register(register.0),
            Some(gimli::RegisterRule::Offset(offset)) => ReturnAddress::Saved(offset),
            Some(gimli::RegisterRule::Undefined) => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "this frame has no caller (return address undefined)",
                ))
            }
            Some(_) => return Ok(None),
        };
        Ok(Some(FrameRule {
            cfa_register: register.0,
            cfa_offset: offset,
            return_address,
        }))
    }

    /// Inlined subroutines whose entry is `address`; a stop there is shown at their call
    /// site in the caller, as gdb's `skip_inline_frames` does.
    pub fn inline_entries_at(&self, address: u32) -> u32 {
        self.inline_entries
            .get(&(address & !1))
            .copied()
            .unwrap_or(0)
    }

    /// The frame `hidden` levels out from the innermost one at `address`, with the call
    /// site as its position; `None` without debug information for the address.
    pub fn frame_view(&self, address: u32, hidden: u32) -> io::Result<Option<FrameView>> {
        let context = self.context();
        let mut frames = context
            .find_frames(u64::from(address & !1))
            .skip_all_loads()
            .map_err(io::Error::other)?;
        let mut positions = Vec::new();
        while let Some(frame) = frames.next().map_err(io::Error::other)? {
            positions.push(frame.location.map(|l| {
                (
                    l.file.unwrap_or_default().to_string(),
                    l.line.unwrap_or(0),
                    l.column.unwrap_or(0),
                )
            }));
        }
        let Some(position) = positions.get(hidden as usize) else {
            return Ok(None);
        };
        Ok(position
            .clone()
            .filter(|(_, line, _)| *line != 0)
            .map(|(file, line, column)| FrameView {
                file,
                line,
                column,
                depth: positions.len() - 1 - hidden as usize,
                hidden,
            }))
    }

    /// Function names of the frames at `address` from `skip` levels out, with the
    /// call-site line and column of each frame that inlines another.
    fn inline_context(&self, address: u32, skip: u32) -> io::Result<InlineChain> {
        let context = self.context();
        let mut frames = context
            .find_frames(u64::from(address & !1))
            .skip_all_loads()
            .map_err(io::Error::other)?;
        for _ in 0..skip {
            if frames.next().map_err(io::Error::other)?.is_none() {
                break;
            }
        }
        let mut chain = Vec::new();
        let mut innermost = true;
        while let Some(frame) = frames.next().map_err(io::Error::other)? {
            let name = frame
                .function
                .as_ref()
                .and_then(|name| name.raw_name().ok().map(|n| n.into_owned()))
                .unwrap_or_default();
            let call = (!innermost)
                .then(|| {
                    frame
                        .location
                        .as_ref()
                        .map(|l| (l.line.unwrap_or(0), l.column.unwrap_or(0)))
                })
                .flatten();
            chain.push((name, call));
            innermost = false;
        }
        Ok(chain)
    }

    /// The line table's entry for a file path exactly as the table renders it.
    pub fn source_file(&self, path: &str) -> Option<&SourceFile> {
        let [id] = self.lines.matching_files(path)[..] else {
            return None;
        };
        self.lines.file(id)
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
