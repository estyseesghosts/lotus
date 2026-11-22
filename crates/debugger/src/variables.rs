//! DWARF variables of a stopped frame (lotus#78).
//!
//! Frames are unwound with `.debug_frame` (DWARF 5, 6.4). A caller frame looks up scopes
//! and location lists at its return address minus one, the call instruction, as gdb's
//! `get_frame_address_in_block` does. Each concrete frame splits into one frame per
//! inlined subroutine active at that address, innermost first, as gdb counts frames.
//! Locations are DWARF expressions evaluated with `gimli::Evaluation` against the
//! frame's registers, frame base and CFA, and values are rendered from their DWARF
//! types within node, byte and depth budgets. Nothing is guessed: a value the DWARF
//! cannot recover carries an explicit state and reason instead.
use crate::symbols::{Reader, RegisterRule, Symbols};
use gimli::{AttributeValue, UnitOffset};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashSet},
    future::Future,
    io,
    pin::Pin,
};

/// Deepest expansion a request may ask for.
pub const MAX_DEPTH: u32 = 8;
const MAX_NODES: usize = 2048;
const MAX_READ_BYTES: usize = 128 << 10;
const MAX_ELEMENTS: u64 = 64;
const MAX_MEMBERS: usize = 256;
const MAX_FRAMES: usize = 32;
const MAX_STRING: usize = 256;
/// Typedef, qualifier and abstract-origin chains longer than this are corrupt.
const MAX_CHAIN: usize = 32;
const MAX_ENTRY_DEPTH: u32 = 8;
const PAGE: u32 = 4096;

/// Guest memory of the stopped process.
pub trait Memory: Send {
    /// Read `length` bytes, at most one 4 KiB page.
    fn read(
        &mut self,
        address: u32,
        length: u32,
    ) -> impl Future<Output = io::Result<Vec<u8>>> + Send;
}

type Boxed<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Registers of the innermost frame: r0-r15 and the VFP single-precision words.
#[derive(Debug, Clone)]
pub struct Registers {
    pub core: [Option<u32>; 16],
    pub vfp: Vec<Option<u32>>,
}

/// What to inspect in the selected frame.
#[derive(Debug, Clone)]
pub enum Query {
    /// Every parameter and local in scope.
    Frame,
    /// One variable: a local in scope, else a global.
    Name(String),
    /// A restricted typed expression (see [`parse_expression`]).
    Expression(String),
}

#[derive(Debug, Clone)]
pub struct Inspect {
    pub frame: u32,
    /// Inlined frames hidden at the innermost frame, as the last source stop showed it.
    pub hidden: u32,
    pub depth: u32,
    pub query: Query,
}

/// Parsed units for variable lookup.
pub(crate) struct Debug {
    dwarf: gimli::Dwarf<Reader>,
    units: Vec<gimli::Unit<Reader>>,
    ranges: Vec<Vec<(u64, u64)>>,
}

fn other(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

impl Debug {
    pub(crate) fn parse(dwarf: gimli::Dwarf<Reader>) -> io::Result<Self> {
        let mut units = Vec::new();
        let mut ranges = Vec::new();
        let mut headers = dwarf.units();
        while let Some(header) = headers.next().map_err(other)? {
            let unit = dwarf.unit(header).map_err(other)?;
            let mut list = Vec::new();
            let mut iter = dwarf.unit_ranges(&unit).map_err(other)?;
            while let Some(range) = iter.next().map_err(other)? {
                if range.begin < range.end {
                    list.push((range.begin, range.end));
                }
            }
            units.push(unit);
            ranges.push(list);
            if units.len() > 1 << 16 {
                return Err(io::Error::other("DWARF units exceed budget"));
            }
        }
        Ok(Self {
            dwarf,
            units,
            ranges,
        })
    }

    fn entry(
        &self,
        unit: usize,
        offset: UnitOffset,
    ) -> io::Result<gimli::DebuggingInformationEntry<Reader>> {
        self.units[unit].entry(offset).map_err(other)
    }

    fn unit_ref(&self, unit: usize) -> gimli::UnitRef<'_, Reader> {
        self.units[unit].unit_ref(&self.dwarf)
    }

    fn reference(&self, unit: usize, value: AttributeValue<Reader>) -> Option<(usize, UnitOffset)> {
        match value {
            AttributeValue::UnitRef(offset) => Some((unit, offset)),
            AttributeValue::DebugInfoRef(offset) => {
                self.units
                    .iter()
                    .enumerate()
                    .find_map(|(index, candidate)| {
                        offset
                            .to_unit_offset(&candidate.header)
                            .map(|offset| (index, offset))
                    })
            }
            _ => None,
        }
    }

    /// An attribute of a DIE, else of the DIEs it completes (`DW_AT_abstract_origin`,
    /// `DW_AT_specification`), with the unit it was found in.
    fn inherited(
        &self,
        unit: usize,
        offset: UnitOffset,
        name: gimli::DwAt,
    ) -> io::Result<Option<(usize, AttributeValue<Reader>)>> {
        let (mut unit, mut offset) = (unit, offset);
        for _ in 0..MAX_CHAIN {
            let entry = self.entry(unit, offset)?;
            if let Some(value) = entry.attr_value(name) {
                return Ok(Some((unit, value)));
            }
            let next = entry
                .attr_value(gimli::DW_AT_abstract_origin)
                .or_else(|| entry.attr_value(gimli::DW_AT_specification));
            match next.and_then(|value| self.reference(unit, value)) {
                Some(next) => (unit, offset) = next,
                None => return Ok(None),
            }
        }
        Ok(None)
    }

    fn string(&self, unit: usize, value: AttributeValue<Reader>) -> io::Result<String> {
        use gimli::Reader as _;
        Ok(self
            .unit_ref(unit)
            .attr_string(value)
            .map_err(other)?
            .to_string_lossy()
            .map_err(other)?
            .into_owned())
    }

    fn name(&self, unit: usize, offset: UnitOffset) -> io::Result<Option<String>> {
        match self.inherited(unit, offset, gimli::DW_AT_name)? {
            Some((unit, value)) => self.string(unit, value).map(Some),
            None => Ok(None),
        }
    }

    fn type_of(&self, unit: usize, offset: UnitOffset) -> io::Result<Ty> {
        Ok(self
            .inherited(unit, offset, gimli::DW_AT_type)?
            .and_then(|(unit, value)| self.reference(unit, value))
            .map_or(Ty::Void, |(unit, offset)| Ty::Die(unit, offset)))
    }

    /// The scopes containing `pc`, outermost (a subprogram) first. A lexical block
    /// without PC bounds is processed as part of its parent, as gdb's
    /// `read_lexical_block_scope` does.
    fn chain(&self, pc: u64) -> io::Result<Option<Chain>> {
        let Some(unit) = self
            .ranges
            .iter()
            .position(|ranges| ranges.iter().any(|&(b, e)| b <= pc && pc < e))
        else {
            return Ok(None);
        };
        let unit_ref = self.unit_ref(unit);
        let mut scopes: Vec<Option<Scope>> = Vec::new();
        let mut parents: Vec<Option<usize>> = Vec::new();
        // (depth, active, enclosing scope)
        let mut stack: Vec<(isize, bool, Option<usize>)> = Vec::new();
        let mut cursor = self.units[unit].entries();
        while let Some(entry) = cursor.next_dfs().map_err(other)? {
            let depth = entry.depth();
            while stack.last().is_some_and(|(d, _, _)| *d >= depth) {
                stack.pop();
            }
            let (active, scope) = stack.last().map_or((true, None), |(_, a, s)| (*a, *s));
            let tag = entry.tag();
            let inner = match tag {
                gimli::DW_TAG_subprogram
                | gimli::DW_TAG_inlined_subroutine
                | gimli::DW_TAG_lexical_block => {
                    let mut ranges = unit_ref.die_ranges(entry).map_err(other)?;
                    let (mut bounded, mut contains) = (false, false);
                    while let Some(range) = ranges.next().map_err(other)? {
                        bounded = true;
                        contains |= range.begin <= pc && pc < range.end;
                    }
                    if !active {
                        (false, None)
                    } else if !bounded && tag == gimli::DW_TAG_lexical_block {
                        (true, scope)
                    } else if contains {
                        scopes.push(Some(Scope {
                            offset: entry.offset(),
                            tag,
                            vars: Vec::new(),
                            call_sites: Vec::new(),
                        }));
                        parents.push(scope);
                        (true, Some(scopes.len() - 1))
                    } else {
                        (false, None)
                    }
                }
                gimli::DW_TAG_variable | gimli::DW_TAG_formal_parameter => {
                    if let (true, Some(scope)) = (active, scope) {
                        if let Some(scope) = scopes[scope].as_mut() {
                            scope.vars.push(entry.offset());
                        }
                    }
                    (false, None)
                }
                gimli::DW_TAG_call_site | gimli::DW_TAG_GNU_call_site => {
                    if let (true, Some(scope)) = (active, scope) {
                        if let Some(scope) = scopes[scope].as_mut() {
                            scope.call_sites.push(entry.offset());
                        }
                    }
                    (false, None)
                }
                gimli::DW_TAG_compile_unit
                | gimli::DW_TAG_partial_unit
                | gimli::DW_TAG_namespace => (active, scope),
                _ => (false, None),
            };
            if entry.has_children() {
                stack.push((depth, inner.0, inner.1));
            }
            if scopes.len() > 4096 {
                return Err(io::Error::other(
                    "DWARF scopes at this address exceed budget",
                ));
            }
        }
        let Some(mut index) = scopes.len().checked_sub(1) else {
            return Ok(None);
        };
        let mut path = vec![index];
        while let Some(parent) = parents[index] {
            path.push(parent);
            index = parent;
        }
        path.reverse();
        let scopes: Vec<Scope> = path.into_iter().filter_map(|i| scopes[i].take()).collect();
        if scopes.first().map(|s| s.tag) != Some(gimli::DW_TAG_subprogram) {
            return Ok(None);
        }
        Ok(Some(Chain { unit, scopes }))
    }

    /// Children of a DIE (one level).
    fn children(&self, unit: usize, offset: UnitOffset) -> io::Result<Vec<UnitOffset>> {
        let mut cursor = self.units[unit].entries_at_offset(offset).map_err(other)?;
        let mut children = Vec::new();
        let mut first = true;
        while let Some(entry) = cursor.next_dfs().map_err(other)? {
            if first {
                first = false;
                if !entry.has_children() {
                    break;
                }
                continue;
            }
            if entry.depth() <= 0 {
                break;
            }
            if entry.depth() == 1 {
                children.push(entry.offset());
                if children.len() > 1 << 16 {
                    return Err(io::Error::other("DWARF children exceed budget"));
                }
            }
        }
        Ok(children)
    }

    fn value_type(&self, unit: usize, offset: UnitOffset) -> Result<gimli::ValueType, Missing> {
        let entry = self.entry(unit, offset).map_err(Missing::from)?;
        let encoding = match entry.attr_value(gimli::DW_AT_encoding) {
            Some(AttributeValue::Encoding(encoding)) => encoding,
            _ => {
                return Err(Missing::Unsupported(
                    "typed DWARF operation without a base type".into(),
                ))
            }
        };
        let size = entry
            .attr_value(gimli::DW_AT_byte_size)
            .and_then(|v| v.udata_value())
            .unwrap_or(0);
        gimli::ValueType::from_encoding(encoding, size).ok_or_else(|| {
            Missing::Unsupported(format!("DWARF base type {encoding} of {size} bytes"))
        })
    }
}

#[derive(Debug)]
struct Scope {
    offset: UnitOffset,
    tag: gimli::DwTag,
    vars: Vec<UnitOffset>,
    call_sites: Vec<UnitOffset>,
}

#[derive(Debug)]
struct Chain {
    unit: usize,
    scopes: Vec<Scope>,
}

/// One frame as a user sees it: a concrete function or an inlined subroutine.
#[derive(Debug)]
struct Virtual {
    concrete: usize,
    unit: usize,
    /// The concrete subprogram, whose `DW_AT_frame_base` the frame uses.
    subprogram: UnitOffset,
    /// The subprogram or inlined subroutine.
    function: UnitOffset,
    inline: bool,
    /// Outermost first, ending at the innermost lexical block of this frame.
    scopes: Vec<Scope>,
    /// Position within the concrete frame's inline chain, innermost 0.
    level: u32,
}

fn virtuals(concrete: usize, chain: Chain) -> Vec<Virtual> {
    let subprogram = chain.scopes[0].offset;
    let mut frames: Vec<Virtual> = Vec::new();
    for scope in chain.scopes {
        if scope.tag == gimli::DW_TAG_lexical_block && !frames.is_empty() {
            frames.last_mut().unwrap().scopes.push(scope);
            continue;
        }
        frames.push(Virtual {
            concrete,
            unit: chain.unit,
            subprogram,
            function: scope.offset,
            inline: scope.tag == gimli::DW_TAG_inlined_subroutine,
            scopes: vec![scope],
            level: 0,
        });
    }
    frames.reverse();
    for (level, frame) in frames.iter_mut().enumerate() {
        frame.level = level as u32;
    }
    frames
}

/// A concrete frame recovered by unwinding.
#[derive(Debug, Clone)]
struct Concrete {
    pc: u32,
    thumb: bool,
    registers: Registers,
    cfa: Option<u32>,
    caller: bool,
}

impl Concrete {
    fn lookup(&self) -> u32 {
        if self.caller {
            self.pc.wrapping_sub(1)
        } else {
            self.pc
        }
    }
}

/// Why a value is not shown.
#[derive(Debug, Clone)]
enum Missing {
    OptimizedOut(String),
    Unavailable(String),
    InvalidPointer(String),
    Unsupported(String),
    Truncated(String),
}

impl From<io::Error> for Missing {
    fn from(error: io::Error) -> Self {
        Missing::Unsupported(error.to_string())
    }
}

impl From<gimli::Error> for Missing {
    fn from(error: gimli::Error) -> Self {
        Missing::Unsupported(format!("DWARF expression: {error}"))
    }
}

impl Missing {
    fn write(&self, out: &mut Value) {
        let (state, reason) = match self {
            Missing::OptimizedOut(reason) => ("optimized_out", reason),
            Missing::Unavailable(reason) => ("unavailable", reason),
            Missing::InvalidPointer(reason) => ("invalid_pointer", reason),
            Missing::Unsupported(reason) => ("unsupported", reason),
            Missing::Truncated(reason) => ("truncated", reason),
        };
        out["state"] = state.into();
        out["reason"] = reason.as_str().into();
    }
}

#[derive(Debug, Clone)]
enum Place {
    Memory(u32),
    Bytes { data: Vec<u8>, known: Vec<bool> },
}

impl Place {
    fn offset(&self, by: u64) -> Place {
        match self {
            Place::Memory(address) => Place::Memory(address.wrapping_add(by as u32)),
            Place::Bytes { data, known } => {
                let start = (by as usize).min(data.len());
                Place::Bytes {
                    data: data[start..].to_vec(),
                    known: known[start..].to_vec(),
                }
            }
        }
    }

    fn known(data: Vec<u8>) -> Place {
        let known = vec![true; data.len()];
        Place::Bytes { data, known }
    }
}

/// A type: a DIE, or one built by an expression (`&x`, a slice of a multi-dimensional
/// array).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Ty {
    Void,
    Die(usize, UnitOffset),
    Pointer(Box<Ty>),
    Array(Box<Ty>, Vec<Option<u64>>),
}

#[derive(Debug)]
enum Shape {
    Void,
    Base {
        encoding: gimli::DwAte,
        size: u64,
    },
    Pointer {
        target: Ty,
        reference: bool,
    },
    Composite {
        kind: &'static str,
        size: Option<u64>,
        unit: usize,
        offset: UnitOffset,
    },
    Array {
        element: Ty,
        count: Option<u64>,
    },
    Enum {
        size: u64,
        unit: usize,
        offset: UnitOffset,
    },
    Function,
    Unknown(String),
}

#[derive(Debug, Clone)]
struct Member {
    name: String,
    ty: Ty,
    offset: Result<u64, String>,
    /// Bit position from the start of the containing object, and width.
    bits: Option<(u64, u64)>,
}

impl Debug {
    fn shape(&self, ty: &Ty) -> io::Result<Shape> {
        let mut ty = ty.clone();
        for _ in 0..MAX_CHAIN {
            let (unit, offset) = match &ty {
                Ty::Void => return Ok(Shape::Void),
                Ty::Pointer(target) => {
                    return Ok(Shape::Pointer {
                        target: (**target).clone(),
                        reference: false,
                    })
                }
                Ty::Array(element, dims) => {
                    let element = if dims.len() > 1 {
                        Ty::Array(element.clone(), dims[1..].to_vec())
                    } else {
                        (**element).clone()
                    };
                    return Ok(Shape::Array {
                        element,
                        count: dims.first().copied().flatten(),
                    });
                }
                Ty::Die(unit, offset) => (*unit, *offset),
            };
            let entry = self.entry(unit, offset)?;
            let size = entry
                .attr_value(gimli::DW_AT_byte_size)
                .and_then(|v| v.udata_value());
            let inner = || self.type_of(unit, offset);
            match entry.tag() {
                gimli::DW_TAG_typedef
                | gimli::DW_TAG_const_type
                | gimli::DW_TAG_volatile_type
                | gimli::DW_TAG_restrict_type
                | gimli::DW_TAG_atomic_type => ty = inner()?,
                gimli::DW_TAG_base_type => {
                    let encoding = match entry.attr_value(gimli::DW_AT_encoding) {
                        Some(AttributeValue::Encoding(encoding)) => encoding,
                        _ => return Ok(Shape::Unknown("base type without an encoding".into())),
                    };
                    return Ok(Shape::Base {
                        encoding,
                        size: size.unwrap_or(0),
                    });
                }
                gimli::DW_TAG_pointer_type => {
                    return Ok(Shape::Pointer {
                        target: inner()?,
                        reference: false,
                    })
                }
                gimli::DW_TAG_reference_type | gimli::DW_TAG_rvalue_reference_type => {
                    return Ok(Shape::Pointer {
                        target: inner()?,
                        reference: true,
                    })
                }
                tag @ (gimli::DW_TAG_structure_type
                | gimli::DW_TAG_class_type
                | gimli::DW_TAG_union_type) => {
                    let declaration = entry.attr_value(gimli::DW_AT_declaration).is_some();
                    return Ok(Shape::Composite {
                        kind: match tag {
                            gimli::DW_TAG_union_type => "union",
                            gimli::DW_TAG_class_type => "class",
                            _ => "struct",
                        },
                        size: size.filter(|_| !declaration),
                        unit,
                        offset,
                    });
                }
                gimli::DW_TAG_array_type => {
                    let mut dims = Vec::new();
                    for child in self.children(unit, offset)? {
                        let range = self.entry(unit, child)?;
                        if range.tag() != gimli::DW_TAG_subrange_type {
                            continue;
                        }
                        let count = range
                            .attr_value(gimli::DW_AT_count)
                            .and_then(|v| v.udata_value());
                        let upper = range
                            .attr_value(gimli::DW_AT_upper_bound)
                            .and_then(|v| v.udata_value());
                        let lower = range
                            .attr_value(gimli::DW_AT_lower_bound)
                            .and_then(|v| v.udata_value())
                            .unwrap_or(0);
                        dims.push(
                            count.or_else(|| {
                                upper.and_then(|u| u.checked_add(1)?.checked_sub(lower))
                            }),
                        );
                    }
                    if dims.is_empty() {
                        dims.push(None);
                    }
                    ty = Ty::Array(Box::new(inner()?), dims);
                }
                gimli::DW_TAG_enumeration_type => {
                    return Ok(Shape::Enum {
                        size: size.unwrap_or(4),
                        unit,
                        offset,
                    })
                }
                gimli::DW_TAG_subroutine_type => return Ok(Shape::Function),
                gimli::DW_TAG_unspecified_type => return Ok(Shape::Void),
                tag => return Ok(Shape::Unknown(format!("DWARF type {tag}"))),
            }
        }
        Ok(Shape::Unknown("type chain exceeds 32 links".into()))
    }

    fn size_of(&self, ty: &Ty) -> io::Result<Option<u64>> {
        self.size_bounded(ty, 0)
    }

    fn size_bounded(&self, ty: &Ty, depth: usize) -> io::Result<Option<u64>> {
        if depth > MAX_CHAIN {
            return Ok(None);
        }
        Ok(match self.shape(ty)? {
            Shape::Base { size, .. } | Shape::Enum { size, .. } => Some(size),
            Shape::Pointer { .. } => Some(4),
            Shape::Composite { size, .. } => size,
            Shape::Array { element, count } => count
                .zip(self.size_bounded(&element, depth + 1)?)
                .and_then(|(count, size)| count.checked_mul(size)),
            _ => None,
        })
    }

    fn type_name(&self, ty: &Ty) -> String {
        self.name_bounded(ty, 0)
            .unwrap_or_else(|error| format!("<{error}>"))
    }

    fn name_bounded(&self, ty: &Ty, depth: usize) -> io::Result<String> {
        if depth > MAX_CHAIN {
            return Ok("<type chain exceeds 32 links>".into());
        }
        let (unit, offset) = match ty {
            Ty::Void => return Ok("void".into()),
            Ty::Pointer(target) => {
                return Ok(format!("{} *", self.name_bounded(target, depth + 1)?))
            }
            Ty::Array(element, dims) => {
                let mut name = self.name_bounded(element, depth + 1)?;
                for dim in dims {
                    name += &dim.map_or("[]".into(), |n| format!("[{n}]"));
                }
                return Ok(name);
            }
            Ty::Die(unit, offset) => (*unit, *offset),
        };
        let entry = self.entry(unit, offset)?;
        let own = self.name(unit, offset)?;
        let inner =
            || -> io::Result<String> { self.name_bounded(&self.type_of(unit, offset)?, depth + 1) };
        Ok(match entry.tag() {
            gimli::DW_TAG_base_type | gimli::DW_TAG_typedef | gimli::DW_TAG_unspecified_type => {
                own.unwrap_or_else(|| "<unnamed>".into())
            }
            gimli::DW_TAG_structure_type => {
                format!("struct {}", own.as_deref().unwrap_or("<anonymous>"))
            }
            gimli::DW_TAG_class_type => {
                format!("class {}", own.as_deref().unwrap_or("<anonymous>"))
            }
            gimli::DW_TAG_union_type => {
                format!("union {}", own.as_deref().unwrap_or("<anonymous>"))
            }
            gimli::DW_TAG_enumeration_type => {
                format!("enum {}", own.as_deref().unwrap_or("<anonymous>"))
            }
            gimli::DW_TAG_const_type => format!("const {}", inner()?),
            gimli::DW_TAG_volatile_type => format!("volatile {}", inner()?),
            gimli::DW_TAG_restrict_type => format!("{} restrict", inner()?),
            gimli::DW_TAG_atomic_type => format!("_Atomic {}", inner()?),
            gimli::DW_TAG_reference_type => format!("{} &", inner()?),
            gimli::DW_TAG_rvalue_reference_type => format!("{} &&", inner()?),
            gimli::DW_TAG_pointer_type => {
                let target = self.type_of(unit, offset)?;
                if let Ty::Die(tu, to) = &target {
                    if self.entry(*tu, *to)?.tag() == gimli::DW_TAG_subroutine_type {
                        let (result, parameters) = self.signature(*tu, *to, depth)?;
                        return Ok(format!("{result} (*)({parameters})"));
                    }
                }
                format!("{} *", self.name_bounded(&target, depth + 1)?)
            }
            gimli::DW_TAG_array_type => self.name_bounded(
                &match self.shape(&Ty::Die(unit, offset))? {
                    Shape::Array { element, count } => Ty::Array(Box::new(element), vec![count]),
                    _ => Ty::Void,
                },
                depth + 1,
            )?,
            gimli::DW_TAG_subroutine_type => {
                let (result, parameters) = self.signature(unit, offset, depth)?;
                format!("{result} ({parameters})")
            }
            tag => format!("<{tag}>"),
        })
    }

    fn signature(
        &self,
        unit: usize,
        offset: UnitOffset,
        depth: usize,
    ) -> io::Result<(String, String)> {
        let result = self.name_bounded(&self.type_of(unit, offset)?, depth + 1)?;
        let mut parameters = Vec::new();
        for child in self.children(unit, offset)? {
            match self.entry(unit, child)?.tag() {
                gimli::DW_TAG_formal_parameter => {
                    parameters.push(self.name_bounded(&self.type_of(unit, child)?, depth + 1)?)
                }
                gimli::DW_TAG_unspecified_parameters => parameters.push("...".into()),
                _ => {}
            }
        }
        Ok((result, parameters.join(", ")))
    }

    fn members(&self, unit: usize, offset: UnitOffset, union: bool) -> io::Result<Vec<Member>> {
        let mut members = Vec::new();
        for child in self.children(unit, offset)? {
            let entry = self.entry(unit, child)?;
            let inheritance = entry.tag() == gimli::DW_TAG_inheritance;
            if entry.tag() != gimli::DW_TAG_member && !inheritance {
                continue;
            }
            // A DWARF 4 C++ static data member is a declaration, not storage in the object.
            if entry.attr_value(gimli::DW_AT_external).is_some()
                && entry.attr_value(gimli::DW_AT_declaration).is_some()
            {
                continue;
            }
            let ty = self.type_of(unit, child)?;
            let name = match self.name(unit, child)? {
                Some(name) => name,
                None if inheritance => format!("<base {}>", self.type_name(&ty)),
                None => "<anonymous>".into(),
            };
            let offset = match entry.attr_value(gimli::DW_AT_data_member_location) {
                None if union => Ok(0),
                None => Ok(0),
                Some(AttributeValue::Exprloc(expression)) => {
                    // DWARF 2 style: a lone DW_OP_plus_uconst from the object address.
                    let mut ops = expression.operations(self.units[unit].encoding());
                    match (ops.next(), ops.next()) {
                        (Ok(Some(gimli::Operation::PlusConstant { value })), Ok(None)) => Ok(value),
                        _ => Err("member location is a DWARF expression".to_string()),
                    }
                }
                Some(value) => value
                    .udata_value()
                    .ok_or_else(|| "member location form".to_string()),
            };
            let bit_size = entry
                .attr_value(gimli::DW_AT_bit_size)
                .and_then(|v| v.udata_value());
            let bits = match bit_size {
                None => None,
                Some(size) => {
                    let start = if let Some(position) = entry
                        .attr_value(gimli::DW_AT_data_bit_offset)
                        .and_then(|v| v.udata_value())
                    {
                        position
                    } else {
                        // DWARF 3 DW_AT_bit_offset counts from the most significant bit of
                        // the storage unit; on a little-endian target that unit starts at
                        // the member offset.
                        let byte = *offset.as_ref().unwrap_or(&0);
                        let storage = entry
                            .attr_value(gimli::DW_AT_byte_size)
                            .and_then(|v| v.udata_value())
                            .or(self.size_of(&ty)?)
                            .unwrap_or(4);
                        let from_msb = entry
                            .attr_value(gimli::DW_AT_bit_offset)
                            .and_then(|v| v.udata_value())
                            .unwrap_or(0);
                        (byte * 8 + storage * 8).saturating_sub(from_msb + size)
                    };
                    Some((start, size))
                }
            };
            members.push(Member {
                name,
                ty,
                offset: if bits.is_some() { Ok(0) } else { offset },
                bits,
            });
            if members.len() > MAX_MEMBERS {
                break;
            }
        }
        Ok(members)
    }

    fn enumerator(&self, unit: usize, offset: UnitOffset, raw: i64) -> io::Result<Option<String>> {
        for child in self.children(unit, offset)? {
            let entry = self.entry(unit, child)?;
            if entry.tag() != gimli::DW_TAG_enumerator {
                continue;
            }
            let value = entry.attr_value(gimli::DW_AT_const_value);
            let matches = value.as_ref().is_some_and(|value| {
                value.sdata_value() == Some(raw)
                    || value.udata_value().map(|u| u as i64) == Some(raw)
            });
            if matches {
                return self.name(unit, child);
            }
        }
        Ok(None)
    }
}

fn register_name(register: u16) -> String {
    match register {
        13 => "sp".into(),
        14 => "lr".into(),
        15 => "pc".into(),
        0..=12 => format!("r{register}"),
        64..=95 => format!("s{}", register - 64),
        256..=287 => format!("d{}", register - 256),
        _ => format!("DWARF register {register}"),
    }
}

/// The ARM DWARF register numbers of the VFP registers this host reads: `s0`-`s31` (64-95)
/// and `d0`-`d31` (256-287), per the AADWARF32 register table.
fn vfp_words(register: u16) -> Option<(usize, usize)> {
    match register {
        64..=95 => Some((usize::from(register - 64), 1)),
        256..=287 => Some((usize::from(register - 256) * 2, 2)),
        _ => None,
    }
}

/// Page-cached reads of the stopped guest, bounded per request.
struct Reads<'m, M> {
    memory: &'m mut M,
    pages: BTreeMap<u32, Result<Vec<u8>, String>>,
    bytes: usize,
}

enum Failure {
    Unreadable(String),
    Budget,
}

impl<M: Memory> Reads<'_, M> {
    async fn bytes(&mut self, address: u32, length: u32) -> Result<Vec<u8>, Failure> {
        let mut out = Vec::with_capacity(length as usize);
        let end = u64::from(address) + u64::from(length);
        let mut at = u64::from(address);
        while at < end {
            let page = (at as u32) & !(PAGE - 1);
            if !self.pages.contains_key(&page) {
                if self.bytes + PAGE as usize > MAX_READ_BYTES {
                    return Err(Failure::Budget);
                }
                self.bytes += PAGE as usize;
                let read = self
                    .memory
                    .read(page, PAGE)
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|data| {
                        if data.len() == PAGE as usize {
                            Ok(data)
                        } else {
                            Err("short read".into())
                        }
                    });
                self.pages.insert(page, read);
            }
            let data = match &self.pages[&page] {
                Ok(data) => data,
                Err(error) => {
                    return Err(Failure::Unreadable(format!("0x{:08x}: {error}", at as u32)))
                }
            };
            let from = (at as u32 - page) as usize;
            let take = ((end - at) as usize).min(PAGE as usize - from);
            out.extend_from_slice(&data[from..from + take]);
            at += take as u64;
        }
        Ok(out)
    }

    async fn word(&mut self, address: u32) -> Result<u32, Failure> {
        let bytes = self.bytes(address, 4).await?;
        Ok(u32::from_le_bytes(bytes[..4].try_into().unwrap()))
    }
}

fn le(bytes: &[u8]) -> u64 {
    let mut raw = [0u8; 8];
    let n = bytes.len().min(8);
    raw[..n].copy_from_slice(&bytes[..n]);
    u64::from_le_bytes(raw)
}

fn value_bits(value: gimli::Value) -> Result<u64, Missing> {
    match value {
        gimli::Value::F32(v) => Ok(u64::from(v.to_bits())),
        gimli::Value::F64(v) => Ok(v.to_bits()),
        value => value.to_u64(u64::MAX).map_err(Missing::from),
    }
}

struct Cx<'s, 'm, M> {
    symbols: &'s Symbols,
    debug: &'s Debug,
    reads: Reads<'m, M>,
    frames: Vec<Concrete>,
    nodes: usize,
    entry_depth: u32,
    path: HashSet<(u32, Ty)>,
}

/// A variable found by name, ready to be located.
struct Found {
    unit: usize,
    offset: UnitOffset,
    /// Present in the abstract instance only: the inlined copy dropped it.
    inherited: bool,
    parameter: bool,
}

/// A located value: its type and where its bytes are.
struct Lvalue {
    name: String,
    ty: Ty,
    place: Result<Place, Missing>,
    location: Value,
    via_pointer: bool,
    bits: Option<(u64, u64)>,
}

impl<M: Memory> Cx<'_, '_, M> {
    fn register(&self, frame: usize, register: u16) -> Result<u64, Missing> {
        let concrete = &self.frames[frame];
        if let Some((word, count)) = vfp_words(register) {
            let words: Option<Vec<u32>> = (word..word + count)
                .map(|i| concrete.registers.vfp.get(i).copied().flatten())
                .collect();
            return words
                .map(|words| {
                    words
                        .iter()
                        .rev()
                        .fold(0u64, |acc, w| (acc << 32) | u64::from(*w))
                })
                .ok_or_else(|| {
                    Missing::Unavailable(format!(
                        "{} is not available in this frame",
                        register_name(register)
                    ))
                });
        }
        let Some(slot) = concrete.registers.core.get(usize::from(register)) else {
            return Err(Missing::Unsupported(register_name(register)));
        };
        slot.map(u64::from).ok_or_else(|| {
            Missing::Unavailable(format!(
                "{} is not preserved across calls (AAPCS 6.1.1), and the call-frame information does not record it",
                register_name(register)
            ))
        })
    }

    fn register_bytes(&self, frame: usize, register: u16, size: u64) -> Result<Vec<u8>, Missing> {
        let value = self.register(frame, register)?;
        let width = vfp_words(register).map_or(4, |(_, count)| count * 4);
        let mut bytes = value.to_le_bytes()[..width].to_vec();
        bytes.truncate(size.max(1) as usize);
        Ok(bytes)
    }

    fn evaluate<'a>(
        &'a mut self,
        frame: usize,
        unit: usize,
        subprogram: Option<UnitOffset>,
        expression: gimli::Expression<Reader>,
    ) -> Boxed<'a, Result<Vec<gimli::Piece<Reader>>, Missing>>
    where
        M: 'a,
    {
        Box::pin(async move {
            let encoding = self.debug.units[unit].encoding();
            let mut evaluation = expression.evaluation(encoding);
            evaluation.set_max_iterations(10_000);
            let mut state = evaluation.evaluate()?;
            loop {
                state = match state {
                    gimli::EvaluationResult::Complete => return Ok(evaluation.result()),
                    gimli::EvaluationResult::RequiresMemory {
                        address,
                        size,
                        base_type,
                        ..
                    } => {
                        let address = u32::try_from(address).map_err(|_| {
                            Missing::InvalidPointer(format!("address {address:#x}"))
                        })?;
                        let bytes = match self.reads.bytes(address, u32::from(size)).await {
                            Ok(bytes) => bytes,
                            Err(Failure::Unreadable(error)) => {
                                return Err(Missing::Unavailable(error))
                            }
                            Err(Failure::Budget) => {
                                return Err(Missing::Truncated("read budget".into()))
                            }
                        };
                        let raw = le(&bytes);
                        let value = if base_type.0 == 0 {
                            gimli::Value::Generic(raw)
                        } else {
                            gimli::Value::from_u64(self.debug.value_type(unit, base_type)?, raw)?
                        };
                        evaluation.resume_with_memory(value)?
                    }
                    gimli::EvaluationResult::RequiresRegister {
                        register,
                        base_type,
                    } => {
                        let raw = self.register(frame, register.0)?;
                        let value = if base_type.0 == 0 {
                            gimli::Value::Generic(raw)
                        } else {
                            gimli::Value::from_u64(self.debug.value_type(unit, base_type)?, raw)?
                        };
                        evaluation.resume_with_register(value)?
                    }
                    gimli::EvaluationResult::RequiresFrameBase => {
                        let base = self.frame_base(frame, unit, subprogram).await?;
                        evaluation.resume_with_frame_base(base)?
                    }
                    gimli::EvaluationResult::RequiresCallFrameCfa => {
                        let cfa = self.frames[frame].cfa.ok_or_else(|| {
                            Missing::Unavailable(
                                "no call-frame information gives this frame's CFA".into(),
                            )
                        })?;
                        evaluation.resume_with_call_frame_cfa(u64::from(cfa))?
                    }
                    // 3DSX code is loaded at its link address (lotus#76): no relocation.
                    gimli::EvaluationResult::RequiresRelocatedAddress(address) => {
                        evaluation.resume_with_relocated_address(address)?
                    }
                    gimli::EvaluationResult::RequiresIndexedAddress { index, .. } => {
                        let address = self.debug.dwarf.address(&self.debug.units[unit], index)?;
                        evaluation.resume_with_indexed_address(address)?
                    }
                    // Offset 0 names the generic type (DWARF 5, 2.5.1.6, DW_OP_convert).
                    gimli::EvaluationResult::RequiresBaseType(offset) if offset.0 == 0 => {
                        evaluation.resume_with_base_type(gimli::ValueType::Generic)?
                    }
                    gimli::EvaluationResult::RequiresBaseType(offset) => {
                        evaluation.resume_with_base_type(self.debug.value_type(unit, offset)?)?
                    }
                    gimli::EvaluationResult::RequiresEntryValue(inner) => {
                        let value = self.entry_value(frame, unit, inner).await?;
                        evaluation.resume_with_entry_value(gimli::Value::Generic(value))?
                    }
                    gimli::EvaluationResult::RequiresTls(_) => {
                        return Err(Missing::Unsupported("thread-local storage".into()))
                    }
                    gimli::EvaluationResult::RequiresAtLocation(_) => {
                        return Err(Missing::Unsupported("DW_OP_call".into()))
                    }
                    gimli::EvaluationResult::RequiresParameterRef(_) => {
                        return Err(Missing::Unsupported("DW_OP_GNU_parameter_ref".into()))
                    }
                    _ => return Err(Missing::Unsupported("WebAssembly location".into())),
                };
            }
        })
    }

    async fn frame_base(
        &mut self,
        frame: usize,
        unit: usize,
        subprogram: Option<UnitOffset>,
    ) -> Result<u64, Missing> {
        let subprogram = subprogram
            .ok_or_else(|| Missing::Unsupported("DW_OP_fbreg outside a function".into()))?;
        let (unit, value) = self
            .debug
            .inherited(unit, subprogram, gimli::DW_AT_frame_base)?
            .ok_or_else(|| Missing::Unsupported("the function has no DW_AT_frame_base".into()))?;
        let pc = self.frames[frame].lookup();
        let Some(expression) = self.expression_at(unit, value, pc)? else {
            return Err(Missing::Unavailable("no frame base at this address".into()));
        };
        let pieces = self.evaluate(frame, unit, None, expression).await?;
        match pieces.as_slice() {
            [piece] => match &piece.location {
                gimli::Location::Register { register } => self.register(frame, register.0),
                gimli::Location::Address { address } => Ok(*address),
                gimli::Location::Value { value } => value_bits(*value),
                _ => Err(Missing::Unsupported("frame base location".into())),
            },
            _ => Err(Missing::Unsupported("frame base location".into())),
        }
    }

    /// A location attribute's expression at `pc`: an expression, or the location-list entry
    /// covering `pc` (`None` when no entry does).
    fn expression_at(
        &self,
        unit: usize,
        value: AttributeValue<Reader>,
        pc: u32,
    ) -> Result<Option<gimli::Expression<Reader>>, Missing> {
        if let AttributeValue::Exprloc(expression) = value {
            return Ok(Some(expression));
        }
        let Some(mut entries) = self.debug.unit_ref(unit).attr_locations(value)? else {
            return Err(Missing::Unsupported("location attribute form".into()));
        };
        let pc = u64::from(pc & !1);
        while let Some(entry) = entries.next()? {
            if entry.range.begin <= pc && pc < entry.range.end {
                return Ok(Some(entry.data));
            }
        }
        Ok(None)
    }

    /// `DW_OP_entry_value` of a register: the value the caller passed, from the call
    /// site's `DW_AT_call_value` evaluated in the caller frame, as gdb resolves entry
    /// values through `DW_TAG_call_site` parameters (DWARF 5, 3.4.2).
    fn entry_value<'a>(
        &'a mut self,
        frame: usize,
        unit: usize,
        inner: gimli::Expression<Reader>,
    ) -> Boxed<'a, Result<u64, Missing>>
    where
        M: 'a,
    {
        Box::pin(async move {
            if self.entry_depth >= MAX_ENTRY_DEPTH {
                return Err(Missing::Unavailable(
                    "entry values nest more than 8 calls deep".into(),
                ));
            }
            let encoding = self.debug.units[unit].encoding();
            let mut ops = inner.operations(encoding);
            let (register, offset) = match (ops.next()?, ops.next()?) {
                (Some(gimli::Operation::Register { register }), None) => (register, 0),
                (
                    Some(gimli::Operation::RegisterOffset {
                        register, offset, ..
                    }),
                    None,
                ) => (register, offset),
                _ => {
                    return Err(Missing::Unsupported(
                        "DW_OP_entry_value of a non-register expression".into(),
                    ))
                }
            };
            let name = register_name(register.0);
            let caller = frame + 1;
            let Some(concrete) = self.frames.get(caller) else {
                return Err(Missing::Unavailable(format!(
                    "{name} at function entry needs the caller frame, which was not unwound"
                )));
            };
            let return_pc = concrete.pc;
            let Some(chain) = self.debug.chain(u64::from(concrete.lookup()))? else {
                return Err(Missing::Unavailable(format!(
                    "{name} at function entry: the caller has no debug information"
                )));
            };
            let mut value_expression = None;
            'sites: for scope in &chain.scopes {
                for site in &scope.call_sites {
                    let entry = self.debug.entry(chain.unit, *site)?;
                    let returns = entry
                        .attr_value(gimli::DW_AT_call_return_pc)
                        .or_else(|| {
                            (entry.tag() == gimli::DW_TAG_GNU_call_site)
                                .then(|| entry.attr_value(gimli::DW_AT_low_pc))
                                .flatten()
                        })
                        .map(|value| self.debug.unit_ref(chain.unit).attr_address(value))
                        .transpose()?
                        .flatten();
                    if returns != Some(u64::from(return_pc)) {
                        continue;
                    }
                    for parameter in self.debug.children(chain.unit, *site)? {
                        let entry = self.debug.entry(chain.unit, parameter)?;
                        let Some(AttributeValue::Exprloc(location)) =
                            entry.attr_value(gimli::DW_AT_location)
                        else {
                            continue;
                        };
                        let mut ops = location.operations(self.debug.units[chain.unit].encoding());
                        let names_register = matches!(
                            (ops.next(), ops.next()),
                            (Ok(Some(gimli::Operation::Register { register: r })), Ok(None)) if r == register
                        );
                        if !names_register {
                            continue;
                        }
                        if let Some(AttributeValue::Exprloc(value)) = entry
                            .attr_value(gimli::DW_AT_call_value)
                            .or_else(|| entry.attr_value(gimli::DW_AT_GNU_call_site_value))
                        {
                            value_expression = Some(value);
                        }
                        break 'sites;
                    }
                    break 'sites;
                }
            }
            let Some(expression) = value_expression else {
                return Err(Missing::Unavailable(format!(
                    "{name} at function entry: the call site returning to 0x{return_pc:08x} records no value for it"
                )));
            };
            let subprogram = chain.scopes[0].offset;
            self.entry_depth += 1;
            let pieces = self
                .evaluate(caller, chain.unit, Some(subprogram), expression)
                .await;
            self.entry_depth -= 1;
            let value = match pieces?.as_slice() {
                [piece] => match &piece.location {
                    gimli::Location::Address { address } => *address,
                    gimli::Location::Value { value } => value_bits(*value)?,
                    gimli::Location::Register { register } => self.register(caller, register.0)?,
                    _ => return Err(Missing::Unsupported("call-site value form".into())),
                },
                _ => return Err(Missing::Unsupported("call-site value form".into())),
            };
            Ok(value.wrapping_add(offset as u64))
        })
    }

    /// Where a variable's bytes are in `frame`, with the location's provenance.
    async fn locate(
        &mut self,
        frame: usize,
        subprogram: Option<UnitOffset>,
        found: &Found,
        size: Option<u64>,
    ) -> (Result<Place, Missing>, Value) {
        if found.inherited {
            return (
                Err(Missing::OptimizedOut(
                    "the inlined copy of this function has no entry for it".into(),
                )),
                json!({"kind": "none"}),
            );
        }
        let entry = match self.debug.entry(found.unit, found.offset) {
            Ok(entry) => entry,
            Err(error) => return (Err(error.into()), json!({"kind": "none"})),
        };
        if let Some(value) = entry.attr_value(gimli::DW_AT_location) {
            let pc = self.frames[frame].lookup();
            let expression = match self.expression_at(found.unit, value, pc) {
                Ok(Some(expression)) => expression,
                Ok(None) => {
                    return (
                        Err(Missing::OptimizedOut(format!(
                            "no location-list entry covers 0x{:08x}",
                            pc & !1
                        ))),
                        json!({"kind": "location_list"}),
                    )
                }
                Err(missing) => return (Err(missing), json!({"kind": "none"})),
            };
            let pieces = match self
                .evaluate(frame, found.unit, subprogram, expression)
                .await
            {
                Ok(pieces) => pieces,
                Err(missing) => return (Err(missing), json!({"kind": "expression"})),
            };
            return self.materialize(frame, pieces, size.unwrap_or(4)).await;
        }
        if let Ok(Some((_, value))) =
            self.debug
                .inherited(found.unit, found.offset, gimli::DW_AT_const_value)
        {
            let bytes = match value {
                AttributeValue::Block(block) => {
                    use gimli::Reader as _;
                    block.to_slice().map(|b| b.to_vec()).unwrap_or_default()
                }
                AttributeValue::Sdata(value) => value.to_le_bytes().to_vec(),
                value => value.udata_value().unwrap_or(0).to_le_bytes().to_vec(),
            };
            let mut bytes = bytes;
            bytes.truncate(size.unwrap_or(bytes.len() as u64) as usize);
            return (Ok(Place::known(bytes)), json!({"kind": "constant"}));
        }
        (
            Err(Missing::OptimizedOut(
                "the variable has no DW_AT_location".into(),
            )),
            json!({"kind": "none"}),
        )
    }

    async fn materialize(
        &mut self,
        frame: usize,
        pieces: Vec<gimli::Piece<Reader>>,
        size: u64,
    ) -> (Result<Place, Missing>, Value) {
        if let [piece] = pieces.as_slice() {
            if piece.size_in_bits.is_none() {
                return match &piece.location {
                    gimli::Location::Empty => (
                        Err(Missing::OptimizedOut("the location is empty".into())),
                        json!({"kind": "none"}),
                    ),
                    gimli::Location::Address { address } => match u32::try_from(*address) {
                        Ok(address) => (
                            Ok(Place::Memory(address)),
                            json!({"kind": "memory", "address": address}),
                        ),
                        Err(_) => (
                            Err(Missing::InvalidPointer(format!("{address:#x}"))),
                            json!({"kind": "memory"}),
                        ),
                    },
                    gimli::Location::Register { register } => (
                        self.register_bytes(frame, register.0, size)
                            .map(Place::known),
                        json!({"kind": "register", "register": register_name(register.0)}),
                    ),
                    gimli::Location::Value { value } => (
                        value_bits(*value).map(|raw| {
                            let mut bytes = raw.to_le_bytes().to_vec();
                            bytes.truncate(size.clamp(1, 8) as usize);
                            Place::known(bytes)
                        }),
                        json!({"kind": "computed"}),
                    ),
                    gimli::Location::Bytes { value } => {
                        use gimli::Reader as _;
                        (
                            Ok(Place::known(
                                value.to_slice().map(|b| b.to_vec()).unwrap_or_default(),
                            )),
                            json!({"kind": "implicit_value"}),
                        )
                    }
                    gimli::Location::ImplicitPointer { .. } => (
                        Err(Missing::Unsupported("DW_OP_implicit_pointer".into())),
                        json!({"kind": "implicit_pointer"}),
                    ),
                };
            }
        }
        // Composite location (DWARF 5, 2.6.1.2): assemble the pieces, keeping which bytes
        // are known.
        let size = size.min(4096) as usize;
        let mut data = vec![0u8; size];
        let mut known = vec![false; size];
        let mut at = 0usize;
        let mut described = Vec::new();
        for piece in pieces {
            let Some(bits) = piece.size_in_bits else {
                return (
                    Err(Missing::Unsupported("piece without a size".into())),
                    json!({"kind": "pieces"}),
                );
            };
            if bits % 8 != 0 || piece.bit_offset.is_some_and(|offset| offset != 0) {
                return (
                    Err(Missing::Unsupported("bit pieces".into())),
                    json!({"kind": "pieces"}),
                );
            }
            let length = (bits / 8) as usize;
            let (bytes, description): (Option<Vec<u8>>, Value) = match &piece.location {
                gimli::Location::Empty => (None, json!({"bytes": length, "kind": "none"})),
                gimli::Location::Address { address } => {
                    let bytes = match u32::try_from(*address) {
                        Ok(address) => self.reads.bytes(address, length as u32).await.ok(),
                        Err(_) => None,
                    };
                    (
                        bytes,
                        json!({"bytes": length, "kind": "memory", "address": address}),
                    )
                }
                gimli::Location::Register { register } => (
                    self.register_bytes(frame, register.0, length as u64).ok(),
                    json!({"bytes": length, "kind": "register", "register": register_name(register.0)}),
                ),
                gimli::Location::Value { value } => (
                    value_bits(*value)
                        .ok()
                        .map(|raw| raw.to_le_bytes().to_vec()),
                    json!({"bytes": length, "kind": "computed"}),
                ),
                gimli::Location::Bytes { value } => {
                    use gimli::Reader as _;
                    (
                        value.to_slice().ok().map(|b| b.to_vec()),
                        json!({"bytes": length, "kind": "implicit_value"}),
                    )
                }
                gimli::Location::ImplicitPointer { .. } => {
                    (None, json!({"bytes": length, "kind": "implicit_pointer"}))
                }
            };
            if let Some(bytes) = bytes {
                for i in 0..length.min(bytes.len()) {
                    if at + i < size {
                        data[at + i] = bytes[i];
                        known[at + i] = true;
                    }
                }
            }
            described.push(description);
            at += length;
        }
        (
            Ok(Place::Bytes { data, known }),
            json!({"kind": "pieces", "pieces": described}),
        )
    }

    async fn load(
        &mut self,
        place: &Place,
        size: u64,
        via_pointer: bool,
    ) -> Result<Vec<u8>, Missing> {
        match place {
            Place::Memory(address) => match self.reads.bytes(*address, size as u32).await {
                Ok(bytes) => Ok(bytes),
                Err(Failure::Budget) => {
                    Err(Missing::Truncated("read budget of 128 KiB reached".into()))
                }
                Err(Failure::Unreadable(error)) if via_pointer => {
                    Err(Missing::InvalidPointer(error))
                }
                Err(Failure::Unreadable(error)) => Err(Missing::Unavailable(error)),
            },
            Place::Bytes { data, known } => {
                let size = size as usize;
                if size > data.len() || known[..size].iter().any(|k| !k) {
                    Err(Missing::OptimizedOut(
                        "part of the value is optimized out".into(),
                    ))
                } else {
                    Ok(data[..size].to_vec())
                }
            }
        }
    }

    async fn string(&mut self, address: u32) -> Result<String, Missing> {
        let mut text = Vec::new();
        let mut at = address;
        while text.len() < MAX_STRING {
            let chunk = (PAGE - at % PAGE).min((MAX_STRING - text.len()) as u32);
            let bytes = self
                .load(&Place::Memory(at), u64::from(chunk), true)
                .await?;
            if let Some(end) = bytes.iter().position(|b| *b == 0) {
                text.extend_from_slice(&bytes[..end]);
                return Ok(String::from_utf8_lossy(&text).into_owned());
            }
            text.extend_from_slice(&bytes);
            at = at.wrapping_add(chunk);
        }
        Ok(format!("{}…", String::from_utf8_lossy(&text)))
    }

    fn is_char(&self, ty: &Ty) -> bool {
        matches!(
            self.debug.shape(ty),
            Ok(Shape::Base {
                encoding: gimli::DW_ATE_signed_char | gimli::DW_ATE_unsigned_char,
                size: 1
            })
        )
    }

    fn node<'a>(&'a mut self, value: Lvalue, depth: u32) -> Boxed<'a, Value>
    where
        M: 'a,
    {
        Box::pin(async move {
            self.nodes += 1;
            let mut out = json!({"name": value.name, "type": self.debug.type_name(&value.ty)});
            if self.nodes > MAX_NODES {
                Missing::Truncated(format!("node budget of {MAX_NODES} reached")).write(&mut out);
                return out;
            }
            let place = match value.place {
                Ok(place) => place,
                Err(missing) => {
                    missing.write(&mut out);
                    return out;
                }
            };
            if let Place::Memory(address) = place {
                out["address"] = address.into();
            }
            let shape = match self.debug.shape(&value.ty) {
                Ok(shape) => shape,
                Err(error) => {
                    Missing::Unsupported(error.to_string()).write(&mut out);
                    return out;
                }
            };
            if let Some((start, size)) = value.bits {
                out["bits"] = json!({"offset": start, "size": size});
                let first = start / 8;
                let last = (start + size.max(1) - 1) / 8;
                match self
                    .load(&place.offset(first), last - first + 1, value.via_pointer)
                    .await
                {
                    Ok(bytes) => {
                        let raw = le(&bytes) >> (start % 8);
                        let mask = if size >= 64 {
                            u64::MAX
                        } else {
                            (1u64 << size) - 1
                        };
                        let raw = raw & mask;
                        let signed = matches!(
                            shape,
                            Shape::Base {
                                encoding: gimli::DW_ATE_signed | gimli::DW_ATE_signed_char,
                                ..
                            }
                        ) || matches!(shape, Shape::Enum { .. });
                        out["value"] =
                            if signed && size > 0 && size < 64 && raw >> (size - 1) & 1 == 1 {
                                json!((raw | !mask) as i64)
                            } else if matches!(
                                shape,
                                Shape::Base {
                                    encoding: gimli::DW_ATE_boolean,
                                    ..
                                }
                            ) {
                                json!(raw != 0)
                            } else {
                                json!(raw)
                            };
                        out["state"] = "ok".into();
                    }
                    Err(missing) => missing.write(&mut out),
                }
                return out;
            }
            match shape {
                Shape::Void => Missing::Unsupported("void has no value".into()).write(&mut out),
                Shape::Unknown(reason) => Missing::Unsupported(reason).write(&mut out),
                Shape::Function => {
                    out["kind"] = "function".into();
                    out["state"] = "ok".into();
                }
                Shape::Base { encoding, size } => {
                    match self.load(&place, size, value.via_pointer).await {
                        Ok(bytes) => {
                            let raw = le(&bytes);
                            let signed = |raw: u64| {
                                let shift = 64 - 8 * size.clamp(1, 8);
                                ((raw << shift) as i64) >> shift
                            };
                            let decoded = match (encoding, size) {
                                (gimli::DW_ATE_boolean, _) => json!(raw != 0),
                                (gimli::DW_ATE_signed | gimli::DW_ATE_signed_char, 1..=8) => {
                                    json!(signed(raw))
                                }
                                (
                                    gimli::DW_ATE_unsigned
                                    | gimli::DW_ATE_unsigned_char
                                    | gimli::DW_ATE_UTF
                                    | gimli::DW_ATE_address,
                                    1..=8,
                                ) => json!(raw),
                                (gimli::DW_ATE_float, 4) => {
                                    float(f64::from(f32::from_bits(raw as u32)))
                                }
                                (gimli::DW_ATE_float, 8) => float(f64::from_bits(raw)),
                                _ => {
                                    Missing::Unsupported(format!(
                                        "base type {encoding} of {size} bytes"
                                    ))
                                    .write(&mut out);
                                    return out;
                                }
                            };
                            if matches!(
                                encoding,
                                gimli::DW_ATE_signed_char | gimli::DW_ATE_unsigned_char
                            ) && size == 1
                            {
                                let byte = raw as u8;
                                out["char"] = std::ascii::escape_default(byte).to_string().into();
                            }
                            out["value"] = decoded;
                            out["state"] = "ok".into();
                        }
                        Err(missing) => missing.write(&mut out),
                    }
                }
                Shape::Enum { size, unit, offset } => {
                    match self.load(&place, size, value.via_pointer).await {
                        Ok(bytes) => {
                            let raw = le(&bytes);
                            let shift = 64 - 8 * size.clamp(1, 8);
                            let signed = ((raw << shift) as i64) >> shift;
                            let name = self
                                .debug
                                .enumerator(unit, offset, signed)
                                .ok()
                                .flatten()
                                .or_else(|| {
                                    self.debug
                                        .enumerator(unit, offset, raw as i64)
                                        .ok()
                                        .flatten()
                                });
                            out["raw"] = json!(signed);
                            out["value"] = name.map_or(json!(signed), Value::from);
                            out["state"] = "ok".into();
                        }
                        Err(missing) => missing.write(&mut out),
                    }
                }
                Shape::Pointer { target, reference } => {
                    let address = match self.load(&place, 4, value.via_pointer).await {
                        Ok(bytes) => le(&bytes) as u32,
                        Err(missing) => {
                            missing.write(&mut out);
                            return out;
                        }
                    };
                    out["value"] = format!("0x{address:08x}").into();
                    out["points_to"] = address.into();
                    out["state"] = "ok".into();
                    if let Some((symbol, offset)) = self.symbols.symbol_at(address) {
                        out["symbol"] = if offset == 0 {
                            symbol.to_string()
                        } else {
                            format!("{symbol}+{offset:#x}")
                        }
                        .into();
                    }
                    if address == 0 {
                        out["null"] = true.into();
                        return out;
                    }
                    let target_shape = self.debug.shape(&target).ok();
                    if matches!(target_shape, Some(Shape::Void | Shape::Function) | None) {
                        return out;
                    }
                    if self.is_char(&target) {
                        match self.string(address).await {
                            Ok(text) => out["string"] = text.into(),
                            Err(missing) => {
                                let mut error = json!({});
                                missing.write(&mut error);
                                out["string_error"] = error;
                            }
                        }
                    }
                    if depth == 0 {
                        out["truncated"] = true.into();
                        return out;
                    }
                    let key = (address, target.clone());
                    let child = if self.path.contains(&key) {
                        json!({"name": "*", "type": self.debug.type_name(&target), "address": address,
                               "state": "cycle", "reason": "already expanded on this path"})
                    } else {
                        self.path.insert(key.clone());
                        let child = self
                            .node(
                                Lvalue {
                                    name: "*".into(),
                                    ty: target,
                                    place: Ok(Place::Memory(address)),
                                    location: Value::Null,
                                    via_pointer: true,
                                    bits: None,
                                },
                                depth - 1,
                            )
                            .await;
                        self.path.remove(&key);
                        child
                    };
                    if reference {
                        out["kind"] = "reference".into();
                    }
                    out["children"] = json!([child]);
                }
                Shape::Composite {
                    kind,
                    size,
                    unit,
                    offset,
                } => {
                    out["kind"] = kind.into();
                    if size.is_none() {
                        Missing::Unsupported(
                            "incomplete type: the ELF declares it without a definition".into(),
                        )
                        .write(&mut out);
                        return out;
                    }
                    out["state"] = "ok".into();
                    if depth == 0 {
                        out["truncated"] = true.into();
                        return out;
                    }
                    let members = match self.debug.members(unit, offset, kind == "union") {
                        Ok(members) => members,
                        Err(error) => {
                            Missing::Unsupported(error.to_string()).write(&mut out);
                            return out;
                        }
                    };
                    if members.len() > MAX_MEMBERS {
                        out["truncated"] = true.into();
                    }
                    let mut children = Vec::new();
                    for member in members.into_iter().take(MAX_MEMBERS) {
                        let place = match &member.offset {
                            Ok(offset) => Ok(place.offset(*offset)),
                            Err(reason) => Err(Missing::Unsupported(reason.clone())),
                        };
                        children.push(
                            self.node(
                                Lvalue {
                                    name: member.name,
                                    ty: member.ty,
                                    place,
                                    location: Value::Null,
                                    via_pointer: value.via_pointer,
                                    bits: member.bits,
                                },
                                depth - 1,
                            )
                            .await,
                        );
                    }
                    out["children"] = children.into();
                }
                Shape::Array { element, count } => {
                    out["kind"] = "array".into();
                    out["count"] = count.into();
                    out["state"] = "ok".into();
                    let element_size = self.debug.size_of(&element).ok().flatten();
                    if self.is_char(&element) {
                        let length = count.unwrap_or(0).min(MAX_STRING as u64);
                        if let Ok(bytes) = self.load(&place, length, value.via_pointer).await {
                            let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
                            out["string"] =
                                String::from_utf8_lossy(&bytes[..end]).into_owned().into();
                        }
                    }
                    if depth == 0 {
                        out["truncated"] = true.into();
                        return out;
                    }
                    let Some(element_size) = element_size else {
                        Missing::Unsupported("array element without a size".into()).write(&mut out);
                        return out;
                    };
                    let shown = count.unwrap_or(0).min(MAX_ELEMENTS);
                    if count.is_some_and(|count| count > shown) {
                        out["truncated"] = true.into();
                    }
                    let mut children = Vec::new();
                    for index in 0..shown {
                        children.push(
                            self.node(
                                Lvalue {
                                    name: format!("[{index}]"),
                                    ty: element.clone(),
                                    place: Ok(place.offset(index * element_size)),
                                    location: Value::Null,
                                    via_pointer: value.via_pointer,
                                    bits: None,
                                },
                                depth - 1,
                            )
                            .await,
                        );
                    }
                    out["children"] = children.into();
                }
            }
            if !value.location.is_null() {
                out["location"] = value.location;
            }
            out
        })
    }
}

fn float(value: f64) -> Value {
    if value.is_finite() {
        json!(value)
    } else {
        json!(value.to_string())
    }
}

/// A restricted expression: names, `.member`, `->member`, `[index]`, unary `*` and `&`,
/// and parentheses. No arithmetic, calls, casts or assignment, so nothing executes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Name(String),
    Member(Box<Expr>, String),
    Arrow(Box<Expr>, String),
    Index(Box<Expr>, u64),
    Deref(Box<Expr>),
    AddressOf(Box<Expr>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Name(String),
    Number(u64),
    Punct(&'static str),
}

fn tokens(text: &str) -> Result<Vec<Token>, String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric()
                    || bytes[i] == b'_'
                    || (bytes[i] == b':' && bytes.get(i + 1) == Some(&b':')))
            {
                i += if bytes[i] == b':' { 2 } else { 1 };
            }
            out.push(Token::Name(text[start..i].to_string()));
        } else if c.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_alphanumeric() {
                i += 1;
            }
            let literal = &text[start..i];
            let number = match literal
                .strip_prefix("0x")
                .or_else(|| literal.strip_prefix("0X"))
            {
                Some(hex) => u64::from_str_radix(hex, 16),
                None => literal.parse(),
            }
            .map_err(|_| format!("invalid number {literal}"))?;
            out.push(Token::Number(number));
        } else {
            let punct = ["->", ".", "[", "]", "*", "&", "(", ")"]
                .into_iter()
                .find(|p| text[i..].starts_with(p))
                .ok_or_else(|| format!("unexpected {:?} at {i}", c as char))?;
            out.push(Token::Punct(punct));
            i += punct.len();
        }
    }
    Ok(out)
}

/// Parse a restricted expression (grammar on [`Expr`]).
pub fn parse_expression(text: &str) -> Result<Expr, String> {
    struct Parser {
        tokens: Vec<Token>,
        at: usize,
        depth: usize,
    }
    impl Parser {
        fn peek(&self) -> Option<&Token> {
            self.tokens.get(self.at)
        }
        fn eat(&mut self, punct: &str) -> bool {
            if matches!(self.peek(), Some(Token::Punct(p)) if *p == punct) {
                self.at += 1;
                true
            } else {
                false
            }
        }
        fn name(&mut self) -> Result<String, String> {
            match self.tokens.get(self.at) {
                Some(Token::Name(name)) => {
                    self.at += 1;
                    Ok(name.clone())
                }
                _ => Err("expected a name".into()),
            }
        }
        fn unary(&mut self) -> Result<Expr, String> {
            self.depth += 1;
            if self.depth > 64 {
                return Err("expression nests too deeply".into());
            }
            let expr = if self.eat("*") {
                Expr::Deref(Box::new(self.unary()?))
            } else if self.eat("&") {
                Expr::AddressOf(Box::new(self.unary()?))
            } else {
                self.postfix()?
            };
            self.depth -= 1;
            Ok(expr)
        }
        fn postfix(&mut self) -> Result<Expr, String> {
            let mut expr = if self.eat("(") {
                let inner = self.unary()?;
                if !self.eat(")") {
                    return Err("expected )".into());
                }
                inner
            } else {
                Expr::Name(self.name()?)
            };
            loop {
                if self.eat(".") {
                    expr = Expr::Member(Box::new(expr), self.name()?);
                } else if self.eat("->") {
                    expr = Expr::Arrow(Box::new(expr), self.name()?);
                } else if self.eat("[") {
                    let Some(Token::Number(index)) = self.tokens.get(self.at).cloned() else {
                        return Err("an index must be a non-negative integer literal".into());
                    };
                    self.at += 1;
                    if !self.eat("]") {
                        return Err("expected ]".into());
                    }
                    expr = Expr::Index(Box::new(expr), index);
                } else {
                    return Ok(expr);
                }
            }
        }
    }
    if text.len() > 256 {
        return Err("expression exceeds 256 bytes".into());
    }
    let mut parser = Parser {
        tokens: tokens(text)?,
        at: 0,
        depth: 0,
    };
    let expr = parser.unary()?;
    if parser.at != parser.tokens.len() {
        return Err(format!("unexpected input after token {}", parser.at));
    }
    Ok(expr)
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

impl Symbols {
    /// Frames, variables and values of the stopped guest (lotus#78). `registers` and
    /// `thumb` describe the innermost frame; the caller guarantees the guest stays stopped.
    pub async fn inspect<M: Memory>(
        &self,
        memory: &mut M,
        registers: Registers,
        thumb: bool,
        request: Inspect,
    ) -> io::Result<Value> {
        if request.depth > MAX_DEPTH {
            return Err(invalid(format!("depth is at most {MAX_DEPTH}")));
        }
        let expression = match &request.query {
            Query::Expression(text) => Some(parse_expression(text).map_err(invalid)?),
            _ => None,
        };
        let mut cx = Cx {
            symbols: self,
            debug: &self.debug,
            reads: Reads {
                memory,
                pages: BTreeMap::new(),
                bytes: 0,
            },
            frames: Vec::new(),
            nodes: 0,
            entry_depth: 0,
            path: HashSet::new(),
        };
        let end = cx.unwind(registers, thumb).await;
        // Frames as a user counts them: each inlined subroutine is a frame.
        let mut listed: Vec<(Option<Virtual>, usize, u32)> = Vec::new();
        for (index, concrete) in cx.frames.iter().enumerate() {
            let chain = self.debug.chain(u64::from(concrete.lookup()))?;
            match chain {
                Some(chain) => {
                    let mut frames = virtuals(index, chain);
                    if index == 0 {
                        let hidden = (request.hidden as usize).min(frames.len().saturating_sub(1));
                        frames.drain(..hidden);
                    }
                    listed.extend(frames.into_iter().map(|frame| {
                        let level = frame.level;
                        (Some(frame), index, level)
                    }));
                }
                None => listed.push((None, index, 0)),
            }
        }
        let describe = |(frame, concrete, level): &(Option<Virtual>, usize, u32),
                        index: usize|
         -> io::Result<Value> {
            let pc = cx.frames[*concrete].pc;
            let lookup = cx.frames[*concrete].lookup();
            let function = match frame {
                Some(frame) => self.debug.name(frame.unit, frame.function)?,
                None => self.function_at(pc).map(|(_, _, name)| name.to_string()),
            };
            let position = self.frame_view(lookup, *level)?;
            Ok(json!({
                "index": index,
                "function": function,
                "pc": pc,
                "thumb": cx.frames[*concrete].thumb,
                "inline": frame.as_ref().is_some_and(|f| f.inline),
                "file": position.as_ref().map(|p| p.file.clone()),
                "line": position.as_ref().map(|p| p.line),
                "debug_information": frame.is_some(),
            }))
        };
        let frames: Vec<Value> = listed
            .iter()
            .enumerate()
            .map(|(index, frame)| describe(frame, index))
            .collect::<io::Result<_>>()?;
        let selected = listed.get(request.frame as usize).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "no frame {}; {} frames were unwound",
                    request.frame,
                    listed.len()
                ),
            )
        })?;
        let Some(frame) = &selected.0 else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "frame {} at 0x{:08x} has no DWARF debugging information",
                    request.frame, cx.frames[selected.1].pc
                ),
            ));
        };
        let mut reply = json!({
            "frame": frames[request.frame as usize],
            "frames": frames,
            "unwind": {"frames": cx.frames.len(), "end": end},
        });
        match &request.query {
            Query::Frame => {
                let mut variables = Vec::new();
                for found in self.debug.frame_variables(frame)? {
                    let node = cx.variable(frame, &found, request.depth).await;
                    variables.push(node);
                }
                reply["variables"] = variables.into();
            }
            Query::Name(name) => {
                let value = cx.named(frame, name).await?;
                reply["value"] = cx.node(value, request.depth).await;
            }
            Query::Expression(_) => {
                let value = cx.expression(frame, expression.as_ref().unwrap()).await?;
                reply["value"] = cx.node(value, request.depth).await;
            }
        }
        reply["budget"] = json!({"nodes": cx.nodes, "bytes_read": cx.reads.bytes});
        Ok(reply)
    }
}

impl Debug {
    /// Parameters, then locals from the innermost block outward. Variables of the
    /// abstract instance that the inlined or concrete copy omits are included as
    /// optimized out, as gdb's `inherit_abstract_dies` does.
    fn frame_variables(&self, frame: &Virtual) -> io::Result<Vec<Found>> {
        let mut parameters = Vec::new();
        let mut locals = Vec::new();
        for (depth, scope) in frame.scopes.iter().enumerate().rev() {
            let mut seen_origins = Vec::new();
            let mut here = Vec::new();
            for offset in &scope.vars {
                let entry = self.entry(frame.unit, *offset)?;
                if let Some(origin) = entry
                    .attr_value(gimli::DW_AT_abstract_origin)
                    .and_then(|v| self.reference(frame.unit, v))
                {
                    seen_origins.push(origin);
                }
                here.push(Found {
                    unit: frame.unit,
                    offset: *offset,
                    inherited: false,
                    parameter: entry.tag() == gimli::DW_TAG_formal_parameter,
                });
            }
            let scope_entry = self.entry(frame.unit, scope.offset)?;
            if let Some((unit, origin)) = scope_entry
                .attr_value(gimli::DW_AT_abstract_origin)
                .and_then(|v| self.reference(frame.unit, v))
            {
                for child in self.children(unit, origin)? {
                    let entry = self.entry(unit, child)?;
                    let tag = entry.tag();
                    if (tag == gimli::DW_TAG_variable || tag == gimli::DW_TAG_formal_parameter)
                        && !seen_origins.contains(&(unit, child))
                    {
                        here.push(Found {
                            unit,
                            offset: child,
                            inherited: true,
                            parameter: tag == gimli::DW_TAG_formal_parameter,
                        });
                    }
                }
            }
            for found in here {
                if depth == 0 && found.parameter {
                    parameters.push(found);
                } else {
                    locals.push(found);
                }
            }
        }
        // Keep each scope's declaration order for parameters.
        parameters.sort_by_key(|f| f.offset.0);
        parameters.extend(locals);
        Ok(parameters)
    }

    /// Globals named `name` (or `ns::name`), preferring the definition in `unit`.
    fn global(&self, unit: usize, name: &str) -> io::Result<Option<(usize, UnitOffset)>> {
        let order = std::iter::once(unit).chain((0..self.units.len()).filter(|u| *u != unit));
        for candidate in order {
            let mut cursor = self.units[candidate].entries();
            let mut namespaces: Vec<(isize, String)> = Vec::new();
            while let Some(entry) = cursor.next_dfs().map_err(other)? {
                let depth = entry.depth();
                while namespaces.last().is_some_and(|(d, _)| *d >= depth) {
                    namespaces.pop();
                }
                let tag = entry.tag();
                let offset = entry.offset();
                let has_children = entry.has_children();
                let located = entry.attr_value(gimli::DW_AT_location).is_some()
                    || entry.attr_value(gimli::DW_AT_const_value).is_some();
                let top = depth == 1 || namespaces.last().is_some_and(|(d, _)| *d == depth - 1);
                if tag == gimli::DW_TAG_namespace && has_children {
                    let label = self
                        .name(candidate, offset)?
                        .unwrap_or_else(|| "(anonymous namespace)".into());
                    namespaces.push((depth, label));
                    continue;
                }
                if tag != gimli::DW_TAG_variable || !located || !top {
                    continue;
                }
                let Some(simple) = self.name(candidate, offset)? else {
                    continue;
                };
                let qualified = namespaces
                    .iter()
                    .map(|(_, n)| n.as_str())
                    .chain(std::iter::once(simple.as_str()))
                    .collect::<Vec<_>>()
                    .join("::");
                if qualified == name || (!name.contains("::") && simple == name) {
                    return Ok(Some((candidate, offset)));
                }
            }
        }
        Ok(None)
    }
}

impl<M: Memory> Cx<'_, '_, M> {
    /// Unwind from the innermost frame with `.debug_frame`. Registers without a rule
    /// keep their value only if AAPCS 6.1.1 has the callee preserve them (r4-r11; s16-s31
    /// by 6.1.2.1); SP becomes the CFA and LR is unknown, since the call wrote it.
    async fn unwind(&mut self, registers: Registers, thumb: bool) -> String {
        let Some(pc) = registers.core[15] else {
            return "the worker reported no PC".into();
        };
        self.frames.push(Concrete {
            pc: pc & !1,
            thumb,
            registers,
            cfa: None,
            caller: false,
        });
        loop {
            if self.frames.len() >= MAX_FRAMES {
                return format!("stopped after {MAX_FRAMES} frames");
            }
            let current = self.frames.last().unwrap().clone();
            let row = match self.symbols.unwind_row(current.lookup()) {
                Ok(Some(row)) => row,
                Ok(None) => {
                    return format!("no call-frame information covers 0x{:08x}", current.pc)
                }
                Err(error) => return error.to_string(),
            };
            let Some(base) = current
                .registers
                .core
                .get(usize::from(row.cfa_register))
                .copied()
                .flatten()
            else {
                return format!(
                    "the CFA register {} is unavailable",
                    register_name(row.cfa_register)
                );
            };
            let cfa = (i64::from(base) + row.cfa_offset) as u32;
            self.frames.last_mut().unwrap().cfa = Some(cfa);
            let rule = |register: u16| {
                row.rules
                    .iter()
                    .find(|(r, _)| *r == register)
                    .map(|(_, rule)| *rule)
            };
            let mut core = [None; 16];
            for (index, slot) in core.iter_mut().enumerate() {
                let register = index as u16;
                *slot = match rule(register) {
                    Some(RegisterRule::Offset(offset)) => {
                        self.reads.word((i64::from(cfa) + offset) as u32).await.ok()
                    }
                    Some(RegisterRule::ValOffset(offset)) => Some((i64::from(cfa) + offset) as u32),
                    Some(RegisterRule::Register(other)) => current
                        .registers
                        .core
                        .get(usize::from(other))
                        .copied()
                        .flatten(),
                    Some(RegisterRule::SameValue) => current.registers.core[index],
                    Some(RegisterRule::Undefined | RegisterRule::Unsupported) => None,
                    None if (4..=11).contains(&index) => current.registers.core[index],
                    None => None,
                };
            }
            let return_address = match rule(row.return_address) {
                None | Some(RegisterRule::SameValue) => current
                    .registers
                    .core
                    .get(usize::from(row.return_address))
                    .copied()
                    .flatten(),
                Some(RegisterRule::Undefined) => {
                    return "outermost frame: the return address is undefined".into()
                }
                Some(_) => core.get(usize::from(row.return_address)).copied().flatten(),
            };
            let Some(return_address) = return_address else {
                return "the return address is unavailable".into();
            };
            if return_address & !1 == 0 {
                return "the return address is 0".into();
            }
            if current.caller && current.registers.core[13].is_some_and(|sp| cfa < sp) {
                return format!("the CFA 0x{cfa:08x} does not move up the stack");
            }
            core[13] = Some(cfa);
            core[14] = None;
            core[15] = Some(return_address & !1);
            let mut vfp = vec![None; current.registers.vfp.len()];
            for word in 16..32.min(vfp.len()) {
                // d8-d15 (DWARF 264-271) are callee-saved.
                let d = 256 + (word / 2) as u16;
                vfp[word] = match rule(d) {
                    Some(RegisterRule::Offset(offset)) => {
                        let at = (i64::from(cfa) + offset) as u32 + (word as u32 % 2) * 4;
                        self.reads.word(at).await.ok()
                    }
                    Some(
                        RegisterRule::Undefined
                        | RegisterRule::Unsupported
                        | RegisterRule::ValOffset(_),
                    ) => None,
                    _ => current.registers.vfp[word],
                };
            }
            self.frames.push(Concrete {
                pc: return_address & !1,
                thumb: return_address & 1 == 1,
                registers: Registers { core, vfp },
                cfa: None,
                caller: true,
            });
        }
    }

    async fn variable(&mut self, frame: &Virtual, found: &Found, depth: u32) -> Value {
        let lvalue = self
            .lvalue(frame.concrete, Some(frame.subprogram), found)
            .await;
        let mut node = self.node(lvalue, depth).await;
        node["kind"] = if found.parameter {
            "parameter"
        } else {
            "local"
        }
        .into();
        node
    }

    async fn lvalue(
        &mut self,
        concrete: usize,
        subprogram: Option<UnitOffset>,
        found: &Found,
    ) -> Lvalue {
        let name = self
            .debug
            .name(found.unit, found.offset)
            .ok()
            .flatten()
            .unwrap_or_else(|| "<anonymous>".into());
        let ty = self
            .debug
            .type_of(found.unit, found.offset)
            .unwrap_or(Ty::Void);
        let size = self.debug.size_of(&ty).ok().flatten();
        let (place, location) = self.locate(concrete, subprogram, found, size).await;
        Lvalue {
            name,
            ty,
            place,
            location,
            via_pointer: false,
            bits: None,
        }
    }

    async fn named(&mut self, frame: &Virtual, name: &str) -> io::Result<Lvalue> {
        if !name.contains("::") {
            // Locals from the innermost block outward shadow parameters.
            let (parameters, locals): (Vec<_>, Vec<_>) = self
                .debug
                .frame_variables(frame)?
                .into_iter()
                .partition(|found| found.parameter);
            for found in locals.into_iter().chain(parameters) {
                if self.debug.name(found.unit, found.offset)?.as_deref() == Some(name) {
                    return Ok(self
                        .lvalue(frame.concrete, Some(frame.subprogram), &found)
                        .await);
                }
            }
        }
        match self.debug.global(frame.unit, name)? {
            Some((unit, offset)) => {
                let found = Found {
                    unit,
                    offset,
                    inherited: false,
                    parameter: false,
                };
                let mut value = self.lvalue(frame.concrete, None, &found).await;
                value.location["scope"] = "global".into();
                Ok(value)
            }
            None => Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no variable {name} is in scope or global"),
            )),
        }
    }

    fn expression<'a>(
        &'a mut self,
        frame: &'a Virtual,
        expr: &'a Expr,
    ) -> Boxed<'a, io::Result<Lvalue>>
    where
        M: 'a,
    {
        Box::pin(async move {
            Ok(match expr {
                Expr::Name(name) => self.named(frame, name).await?,
                Expr::Member(base, member) => {
                    let base = self.expression(frame, base).await?;
                    self.member(base, member)?
                }
                Expr::Arrow(base, member) => {
                    let base = self.expression(frame, base).await?;
                    let target = self.deref(base, &format!("->{member}")).await?;
                    self.member(target, member)?
                }
                Expr::Deref(base) => {
                    let base = self.expression(frame, base).await?;
                    self.deref(base, "*").await?
                }
                Expr::Index(base, index) => {
                    let base = self.expression(frame, base).await?;
                    let name = format!("{}[{index}]", base.name);
                    match self.debug.shape(&base.ty)? {
                        Shape::Array { element, count } => {
                            if count.is_some_and(|count| *index >= count) {
                                return Err(invalid(format!(
                                    "index {index} is outside {}",
                                    self.debug.type_name(&base.ty)
                                )));
                            }
                            let size = self
                                .debug
                                .size_of(&element)?
                                .ok_or_else(|| invalid("element without a size"))?;
                            Lvalue {
                                name,
                                ty: element,
                                place: base.place.map(|p| p.offset(index * size)),
                                location: Value::Null,
                                via_pointer: base.via_pointer,
                                bits: None,
                            }
                        }
                        Shape::Pointer { target, .. } => {
                            let size = self
                                .debug
                                .size_of(&target)?
                                .ok_or_else(|| invalid("pointee without a size"))?;
                            let mut target = self.deref(base, "[]").await?;
                            target.name = name;
                            target.place = target.place.map(|p| p.offset(index * size));
                            target
                        }
                        _ => {
                            return Err(invalid(format!(
                                "{} is not an array or pointer",
                                base.name
                            )))
                        }
                    }
                }
                Expr::AddressOf(base) => {
                    let base = self.expression(frame, base).await?;
                    if base.bits.is_some() {
                        return Err(invalid("a bit-field has no address"));
                    }
                    let address = match &base.place {
                        Ok(Place::Memory(address)) => *address,
                        Ok(Place::Bytes { .. }) => {
                            return Err(invalid(format!("{} is not in memory", base.name)))
                        }
                        Err(_) => {
                            return Err(invalid(format!("{} has no location here", base.name)))
                        }
                    };
                    Lvalue {
                        name: format!("&{}", base.name),
                        ty: Ty::Pointer(Box::new(base.ty)),
                        place: Ok(Place::known(address.to_le_bytes().to_vec())),
                        location: json!({"kind": "computed"}),
                        via_pointer: false,
                        bits: None,
                    }
                }
            })
        })
    }

    fn member(&self, base: Lvalue, member: &str) -> io::Result<Lvalue> {
        let Shape::Composite {
            kind, unit, offset, ..
        } = self.debug.shape(&base.ty)?
        else {
            return Err(invalid(format!(
                "{} is not a struct, class or union",
                base.name
            )));
        };
        let found = self
            .debug
            .members(unit, offset, kind == "union")?
            .into_iter()
            .find(|m| m.name == member)
            .ok_or_else(|| {
                invalid(format!(
                    "{} has no member {member}",
                    self.debug.type_name(&base.ty)
                ))
            })?;
        let place = match (&found.offset, base.place) {
            (Ok(by), Ok(place)) => Ok(place.offset(*by)),
            (Err(reason), _) => Err(Missing::Unsupported(reason.clone())),
            (_, Err(missing)) => Err(missing),
        };
        Ok(Lvalue {
            name: format!("{}.{member}", base.name),
            ty: found.ty,
            place,
            location: Value::Null,
            via_pointer: base.via_pointer,
            bits: found.bits,
        })
    }

    async fn deref(&mut self, base: Lvalue, how: &str) -> io::Result<Lvalue> {
        let Shape::Pointer { target, .. } = self.debug.shape(&base.ty)? else {
            return Err(invalid(format!("{} is not a pointer", base.name)));
        };
        if matches!(self.debug.shape(&target)?, Shape::Void | Shape::Function) {
            return Err(invalid(format!(
                "{} points to {}",
                base.name,
                self.debug.type_name(&target)
            )));
        }
        let place = match base.place {
            Ok(place) => place,
            Err(missing) => {
                let mut out = json!({});
                missing.write(&mut out);
                return Err(invalid(format!(
                    "{}: {}",
                    base.name,
                    out["reason"].as_str().unwrap_or("")
                )));
            }
        };
        let address = match self.load(&place, 4, base.via_pointer).await {
            Ok(bytes) => le(&bytes) as u32,
            Err(missing) => {
                let mut out = json!({});
                missing.write(&mut out);
                return Err(invalid(format!(
                    "{}: {}",
                    base.name,
                    out["reason"].as_str().unwrap_or("")
                )));
            }
        };
        if address == 0 {
            return Err(invalid(format!("{} is a null pointer", base.name)));
        }
        Ok(Lvalue {
            name: if how == "*" {
                format!("*{}", base.name)
            } else {
                base.name
            },
            ty: target,
            place: Ok(Place::Memory(address)),
            location: Value::Null,
            via_pointer: true,
            bits: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expressions_parse_only_the_restricted_grammar() {
        assert_eq!(
            parse_expression("r->path[2].y").unwrap(),
            Expr::Member(
                Box::new(Expr::Index(
                    Box::new(Expr::Arrow(Box::new(Expr::Name("r".into())), "path".into())),
                    2
                )),
                "y".into()
            )
        );
        assert_eq!(
            parse_expression("*&(ns::value)").unwrap(),
            Expr::Deref(Box::new(Expr::AddressOf(Box::new(Expr::Name(
                "ns::value".into()
            )))))
        );
        assert_eq!(
            parse_expression("a[0x10]").unwrap(),
            Expr::Index(Box::new(Expr::Name("a".into())), 16)
        );
        for rejected in [
            "a + 1", "f()", "a = 1", "(int)a", "a[-1]", "a[b]", "", "a.", "**",
        ] {
            assert!(parse_expression(rejected).is_err(), "{rejected}");
        }
        assert!(parse_expression(&"*".repeat(100).to_string().replace("**", "* *")).is_err());
    }

    /// Hand-assembled DWARF 4: a typedef naming itself, a typedef/pointer loop, and an
    /// array whose element type is the array. Each lookup ends with a bounded error.
    #[test]
    fn corrupt_type_chains_are_bounded() {
        let abbrev: Vec<u8> = vec![
            1, 0x11, 1, 0, 0, // compile_unit, children
            2, 0x16, 0, 0x03, 0x08, 0x49, 0x13, 0, 0, // typedef: name string, type ref4
            3, 0x0f, 0, 0x49, 0x13, 0, 0, // pointer_type: type ref4
            4, 0x01, 1, 0x49, 0x13, 0, 0, // array_type, children: type ref4
            5, 0x21, 0, 0x37, 0x0b, 0, 0, // subrange_type: count data1
            0,
        ];
        let mut info: Vec<u8> = vec![0, 0, 0, 0, 4, 0, 0, 0, 0, 0, 4];
        info.push(1); // 11: compile_unit
        info.extend([2, b't', 0, 12, 0, 0, 0]); // 12: typedef t -> 12
        info.extend([2, b'p', 0, 26, 0, 0, 0]); // 19: typedef p -> 26
        info.extend([3, 19, 0, 0, 0]); // 26: pointer -> 19
        info.extend([4, 31, 0, 0, 0, 5, 1, 0]); // 31: array of 31, count 1
        info.push(0);
        let length = (info.len() - 4) as u32;
        info[..4].copy_from_slice(&length.to_le_bytes());
        let dwarf = gimli::Dwarf::load(|id| -> Result<Reader, gimli::Error> {
            let data: &[u8] = match id {
                gimli::SectionId::DebugInfo => &info,
                gimli::SectionId::DebugAbbrev => &abbrev,
                _ => &[],
            };
            Ok(Reader::new(
                std::sync::Arc::from(data),
                gimli::RunTimeEndian::Little,
            ))
        })
        .unwrap();
        let debug = Debug::parse(dwarf).unwrap();
        let die = |offset| Ty::Die(0, UnitOffset(offset));
        assert!(matches!(
            debug.shape(&die(12)).unwrap(),
            Shape::Unknown(reason) if reason.contains("exceeds")
        ));
        assert_eq!(debug.size_of(&die(12)).unwrap(), None);
        assert_eq!(debug.type_name(&die(26)), "p *");
        assert!(
            matches!(debug.shape(&die(26)).unwrap(), Shape::Pointer { target, .. } if target == die(19))
        );
        assert_eq!(debug.size_of(&die(31)).unwrap(), None);
        assert!(
            debug.type_name(&die(31)).contains("exceeds"),
            "{}",
            debug.type_name(&die(31))
        );
    }
}
