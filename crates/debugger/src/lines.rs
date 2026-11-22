//! DWARF line-number rows of an approved ELF (DWARF 4 and 5, section 6.2).
//!
//! Rows are kept sorted by address with their `is_stmt` flag, so the host can tell a
//! statement start (a recommended breakpoint location) from an address inside one.
//! File paths are rendered the same way as `addr2line` (`comp_dir`, directory, name),
//! and DWARF 5 MD5 file checksums are kept when the producer emitted them.
use gimli::Reader as _;
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap},
    io,
};

type Reader = gimli::EndianArcSlice<gimli::RunTimeEndian>;

/// Rows above this are refused instead of allocating without bound.
const MAX_ROWS: usize = 4 << 20;

#[derive(Debug, Clone, Copy)]
struct Row {
    address: u32,
    file: u32,
    line: u32,
    /// 0 when the producer records no column (DWARF 5, 6.2.2).
    column: u32,
    discriminator: u32,
    is_stmt: bool,
    prologue_end: bool,
    /// First address after a sequence; no row covers it.
    end: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceFile {
    pub path: String,
    /// DWARF 5 `DW_LNCT_MD5`, when present.
    #[serde(skip)]
    pub md5: Option<[u8; 16]>,
}

/// The line row covering an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineAt {
    pub file: u32,
    pub line: u32,
    pub column: u32,
    pub discriminator: u32,
    /// The address is the first address of an `is_stmt` row.
    pub statement_start: bool,
    /// `[start, end)` of the line-table row covering the address.
    pub range: (u32, u32),
}

#[derive(Default)]
pub struct LineTable {
    rows: Vec<Row>,
    files: Vec<SourceFile>,
}

/// addr2line 0.27.1 `line.rs` `path_push`: a rooted part replaces the path; otherwise
/// it is joined with `\` when the path has a backslash root, else `/`.
fn path_push(path: &mut String, part: &str) {
    let forward_root = |p: &str| p.starts_with('/') || p.get(1..3) == Some(":/");
    let backward_root = |p: &str| p.starts_with('\\') || p.get(1..3) == Some(":\\");
    if forward_root(part) || backward_root(part) {
        *path = part.to_string();
    } else {
        let separator = if backward_root(path) { '\\' } else { '/' };
        if !path.is_empty() && !path.ends_with(separator) {
            path.push(separator);
        }
        path.push_str(part);
    }
}

/// Entry addresses of inlined subroutines -> how many begin there. The entry is
/// `DW_AT_entry_pc` when present, else the lowest address of the subroutine's ranges
/// (DWARF 5, 2.18 and 3.3.8.3); a constant `DW_AT_entry_pc` is an offset from that base.
pub(crate) fn inline_entries(dwarf: &gimli::Dwarf<Reader>) -> io::Result<BTreeMap<u32, u32>> {
    let mut entries = BTreeMap::new();
    let mut units = dwarf.units();
    while let Some(header) = units.next().map_err(io::Error::other)? {
        let unit = dwarf.unit(header).map_err(io::Error::other)?;
        let unit_ref = unit.unit_ref(dwarf);
        let mut cursor = unit.entries();
        while let Some(entry) = cursor.next_dfs().map_err(io::Error::other)? {
            if entry.tag() != gimli::DW_TAG_inlined_subroutine {
                continue;
            }
            let mut ranges = unit_ref.die_ranges(entry).map_err(io::Error::other)?;
            let mut lowest: Option<u64> = None;
            while let Some(range) = ranges.next().map_err(io::Error::other)? {
                if range.begin < range.end {
                    lowest = Some(lowest.map_or(range.begin, |low| low.min(range.begin)));
                }
            }
            let Some(lowest) = lowest else {
                continue;
            };
            let entry_pc = match entry.attr_value(gimli::DW_AT_entry_pc) {
                Some(value) => match value.udata_value() {
                    Some(offset) if !matches!(value, gimli::AttributeValue::Addr(_)) => {
                        Some(lowest + offset)
                    }
                    _ => unit_ref.attr_address(value).map_err(io::Error::other)?,
                },
                None => None,
            };
            if let Ok(address) = u32::try_from(entry_pc.unwrap_or(lowest)) {
                *entries.entry(address).or_insert(0) += 1;
            }
            if entries.len() > MAX_ROWS {
                return Err(io::Error::other("DWARF inline entries exceed budget"));
            }
        }
    }
    Ok(entries)
}

impl LineTable {
    pub(crate) fn parse(dwarf: &gimli::Dwarf<Reader>) -> io::Result<Self> {
        let mut table = Self::default();
        let mut paths: HashMap<String, u32> = HashMap::new();
        let mut units = dwarf.units();
        while let Some(header) = units.next().map_err(io::Error::other)? {
            let unit = dwarf.unit(header).map_err(io::Error::other)?;
            let unit_ref = unit.unit_ref(dwarf);
            let Some(program) = unit.line_program.clone() else {
                continue;
            };
            let mut files: HashMap<u64, u32> = HashMap::new();
            let mut rows = program.rows();
            while let Some((header, row)) = rows.next_row().map_err(io::Error::other)? {
                let index = row.file_index();
                let file = match files.get(&index) {
                    Some(file) => *file,
                    None => {
                        let Some(entry) = header.file(index) else {
                            continue;
                        };
                        let mut path = unit
                            .comp_dir
                            .as_ref()
                            .map(|dir| dir.to_string_lossy().map(|d| d.into_owned()))
                            .transpose()
                            .map_err(io::Error::other)?
                            .unwrap_or_default();
                        // Directory index 0 is the compilation directory (DWARF 5, 6.2.4.1).
                        if entry.directory_index() != 0 {
                            if let Some(directory) = entry.directory(header) {
                                let directory = unit_ref
                                    .attr_string(directory)
                                    .and_then(|d| d.to_string_lossy().map(|d| d.into_owned()))
                                    .map_err(io::Error::other)?;
                                path_push(&mut path, &directory);
                            }
                        }
                        let name = unit_ref
                            .attr_string(entry.path_name())
                            .and_then(|n| n.to_string_lossy().map(|n| n.into_owned()))
                            .map_err(io::Error::other)?;
                        path_push(&mut path, &name);
                        let md5 = header.file_has_md5().then(|| *entry.md5());
                        let next = table.files.len() as u32;
                        let id = *paths.entry(path.clone()).or_insert_with(|| {
                            table.files.push(SourceFile { path, md5 });
                            next
                        });
                        files.insert(index, id);
                        id
                    }
                };
                let Ok(address) = u32::try_from(row.address()) else {
                    continue;
                };
                table.rows.push(Row {
                    address,
                    file,
                    line: row
                        .line()
                        .map_or(0, |l| u32::try_from(l.get()).unwrap_or(0)),
                    column: match row.column() {
                        gimli::ColumnType::LeftEdge => 0,
                        gimli::ColumnType::Column(column) => {
                            u32::try_from(column.get()).unwrap_or(0)
                        }
                    },
                    discriminator: u32::try_from(row.discriminator()).unwrap_or(0),
                    is_stmt: row.is_stmt(),
                    prologue_end: row.prologue_end(),
                    end: row.end_sequence(),
                });
                if table.rows.len() > MAX_ROWS {
                    return Err(io::Error::other("DWARF line table exceeds budget"));
                }
            }
        }
        // Stable: an end-of-sequence row sorts before a new sequence at the same address.
        table.rows.sort_by_key(|row| (row.address, !row.end));
        Ok(table)
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn file(&self, id: u32) -> Option<&SourceFile> {
        self.files.get(id as usize)
    }

    /// The row covering `address`, if a sequence covers it.
    pub fn at(&self, address: u32) -> Option<LineAt> {
        let index = self.rows.partition_point(|row| row.address <= address);
        let row = self.rows[..index].last()?;
        if row.end || row.line == 0 {
            return None;
        }
        let end = self
            .rows
            .get(index)
            .map_or(row.address, |next| next.address);
        let statement_start = self.rows[..index]
            .iter()
            .rev()
            .take_while(|r| r.address == address)
            .any(|r| r.is_stmt && !r.end && r.line != 0);
        Some(LineAt {
            file: row.file,
            line: row.line,
            column: row.column,
            discriminator: row.discriminator,
            statement_start,
            range: (row.address, end),
        })
    }

    /// Where a stop after the prologue of the function at `[start, end)` belongs: the
    /// first `prologue_end` row (DWARF 5, 6.2.2), else the first statement of another
    /// line than the entry row, as gdb's `skip_prologue_using_sal` falls back to.
    pub fn after_prologue(&self, start: u32, end: u32) -> Option<u32> {
        // Skip the end row of a sequence that stops where this function starts.
        let first = self
            .rows
            .partition_point(|row| row.address < start || (row.address == start && row.end));
        let entry = self
            .rows
            .get(first)
            .filter(|row| row.address == start && row.line != 0)?;
        let rows = self.rows[first..]
            .iter()
            .take_while(|row| row.address < end && !row.end);
        if let Some(row) = rows.clone().find(|row| row.prologue_end) {
            return Some(row.address);
        }
        rows.into_iter()
            .find(|row| {
                row.is_stmt && row.line != 0 && (row.line, row.file) != (entry.line, entry.file)
            })
            .map(|row| row.address)
    }

    /// Files whose path equals `query` or ends with it at a path separator.
    pub fn matching_files(&self, query: &str) -> Vec<u32> {
        let query = query.replace('\\', "/");
        self.files
            .iter()
            .enumerate()
            .filter(|(_, file)| {
                let path = file.path.replace('\\', "/");
                path == query
                    || path
                        .strip_suffix(&query)
                        .is_some_and(|prefix| prefix.ends_with('/'))
            })
            .map(|(id, _)| id as u32)
            .collect()
    }

    /// Statement starts of `line` in `file`: each `is_stmt` row whose previous row in the
    /// same sequence belongs to another line or file. Inlined and duplicated copies of a
    /// line each yield an address.
    pub fn statement_starts(&self, file: u32, line: u32) -> Vec<u32> {
        let mut starts = Vec::new();
        let mut previous: Option<&Row> = None;
        for row in &self.rows {
            let continues = previous.is_some_and(|p| {
                !p.end && p.file == row.file && p.line == row.line && p.address <= row.address
            });
            if !row.end && row.is_stmt && row.file == file && row.line == line && !continues {
                starts.push(row.address);
            }
            previous = Some(row);
        }
        starts.dedup();
        starts
    }
}
