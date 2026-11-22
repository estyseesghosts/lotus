//! Host-owned investigation metadata. Session references identify one creation and epoch.
use crate::supervisor::Supervisor;
use lotus_protocol::envelope::SessionUuid;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

const VERSION: u32 = 1;
const MAX_RECORDS: usize = 256;
const MAX_CATALOG_BYTES: u64 = 2 << 20;
const MAX_ITEMS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SessionReference {
    pub session: String,
    pub created_unix_ms: u64,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLocation {
    /// Path relative to an approved source root, with forward slashes.
    pub file: String,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedBreakpoint {
    pub location: SourceLocation,
    pub content_sha256: String,
    pub elf_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    pub label: String,
    pub target: SessionReference,
    pub frame: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactReference {
    pub uri: String,
    pub sha256: String,
}

/// Metadata supplied by a local client. Host authorization controls who can
/// write; a record never grants worker access or control to its reader.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InvestigationDetails {
    pub title: String,
    pub purpose: String,
    pub hypotheses: Vec<String>,
    pub content_hashes: Vec<String>,
    pub build_hashes: Vec<String>,
    pub sessions: Vec<SessionReference>,
    pub sources: Vec<SourceLocation>,
    pub bookmarks: Vec<Bookmark>,
    pub breakpoints: Vec<SavedBreakpoint>,
    pub traces: Vec<ArtifactReference>,
    pub artifacts: Vec<ArtifactReference>,
    pub next_steps: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Investigation {
    pub id: String,
    pub revision: u64,
    pub details: InvestigationDetails,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceState {
    Live,
    EpochChanged,
    Expired,
}

#[derive(Debug, Serialize)]
pub struct InvestigationView {
    #[serde(flatten)]
    pub record: Investigation,
    pub writable: bool,
    /// Latest published session state for each saved reference, in record order.
    pub session_states: Vec<ReferenceState>,
    /// Availability and hash comparison against the live artifact catalog, in record order.
    pub artifact_states: Vec<ArtifactState>,
    pub trace_states: Vec<ArtifactState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactState {
    Pinned,
    Available,
    Missing,
    HashMismatch,
}

impl InvestigationView {
    pub(crate) fn new(host: &Supervisor, record: Investigation) -> Self {
        let session_states = record
            .details
            .sessions
            .iter()
            .map(|target| {
                let Ok(summary) = host.summary(&target.session) else {
                    return ReferenceState::Expired;
                };
                if !summary.telemetry.state.is_active()
                    || summary.info.created_unix_ms != target.created_unix_ms
                {
                    return ReferenceState::Expired;
                }
                if summary.telemetry.epoch != target.epoch {
                    return ReferenceState::EpochChanged;
                }
                if host
                    .debug_target_current(&target.session, (target.created_unix_ms, target.epoch))
                {
                    ReferenceState::Live
                } else {
                    ReferenceState::Expired
                }
            })
            .collect();
        let artifacts = host.artifacts();
        let states = |references: &[ArtifactReference]| {
            references
                .iter()
                .map(|reference| {
                    let Some(artifact) = artifacts.iter().find(|item| item.uri == reference.uri)
                    else {
                        return ArtifactState::Missing;
                    };
                    if artifact.sha256 != reference.sha256 {
                        ArtifactState::HashMismatch
                    } else if artifact.pinned {
                        ArtifactState::Pinned
                    } else {
                        ArtifactState::Available
                    }
                })
                .collect()
        };
        Self {
            writable: host.allows_control(),
            artifact_states: states(&record.details.artifacts),
            trace_states: states(&record.details.traces),
            record,
            session_states,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    version: u32,
    records: BTreeMap<String, Investigation>,
}

impl Default for Catalog {
    fn default() -> Self {
        Self {
            version: VERSION,
            records: BTreeMap::new(),
        }
    }
}

/// File locks serialize compare-and-swap across independent local processes.
/// The catalog is readable without opening an emulator or its artifact store.
pub struct InvestigationStore {
    directory: PathBuf,
}

impl InvestigationStore {
    pub fn new(storage_root: &Path) -> Self {
        Self {
            directory: storage_root.join("investigations"),
        }
    }

    pub fn list(&self) -> io::Result<Vec<Investigation>> {
        self.with_catalog(|catalog| Ok(catalog.records.into_values().collect()))
    }

    pub fn get(&self, id: &str) -> io::Result<Investigation> {
        self.with_catalog(|mut catalog| catalog.records.remove(id).ok_or_else(not_found))
    }

    pub(crate) fn create(&self, details: InvestigationDetails) -> io::Result<Investigation> {
        validate(&details)?;
        self.with_catalog(|mut catalog| {
            if catalog.records.len() >= MAX_RECORDS {
                return Err(invalid("investigation limit reached"));
            }
            let record = Investigation {
                id: SessionUuid::generate().to_string(),
                revision: 1,
                details,
            };
            if catalog
                .records
                .insert(record.id.clone(), record.clone())
                .is_some()
            {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "investigation identifier collision",
                ));
            }
            self.save(catalog)?;
            Ok(record)
        })
    }

    pub(crate) fn update(
        &self,
        id: &str,
        revision: u64,
        details: InvestigationDetails,
    ) -> io::Result<Investigation> {
        validate(&details)?;
        self.with_catalog(|mut catalog| {
            let record = catalog.records.get_mut(id).ok_or_else(not_found)?;
            if record.revision != revision {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!(
                        "investigation revision mismatch: current revision {}",
                        record.revision
                    ),
                ));
            }
            record.revision = record
                .revision
                .checked_add(1)
                .ok_or_else(|| invalid("investigation revision exhausted"))?;
            record.details = details;
            let updated = record.clone();
            self.save(catalog)?;
            Ok(updated)
        })
    }

    fn with_catalog<T>(&self, operation: impl FnOnce(Catalog) -> io::Result<T>) -> io::Result<T> {
        fs::create_dir_all(&self.directory)?;
        let guard = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.directory.join("catalog.lock"))?;
        guard.lock()?;
        let catalog = match File::open(self.directory.join("catalog.json")) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_CATALOG_BYTES + 1).read_to_end(&mut bytes)?;
                if bytes.len() as u64 > MAX_CATALOG_BYTES {
                    return Err(invalid("investigation catalog exceeds the storage limit"));
                }
                // Check the version before decoding. Preserve unknown or damaged
                // files on all write paths.
                #[derive(Deserialize)]
                struct Version {
                    version: u32,
                }
                let version: Version = decode(&bytes)?;
                if version.version != VERSION {
                    return Err(invalid("unsupported investigation catalog version"));
                }
                decode(&bytes)?
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Catalog::default(),
            Err(error) => return Err(error),
        };
        operation(catalog)
    }

    fn save(&self, catalog: Catalog) -> io::Result<()> {
        let bytes = serde_json::to_vec(&catalog).map_err(io::Error::other)?;
        if bytes.len() as u64 > MAX_CATALOG_BYTES {
            return Err(invalid("investigation catalog exceeds the storage limit"));
        }
        let temporary = self.directory.join("catalog.tmp");
        let mut file = File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(temporary, self.directory.join("catalog.json"))
    }
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> io::Result<T> {
    serde_json::from_slice(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn not_found() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "investigation not found")
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn is_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_location(location: &SourceLocation) -> bool {
    location.line > 0
        && location.file.len() <= 1024
        && !location.file.contains(['\\', ':'])
        && !location.file.chars().any(char::is_control)
        && location
            .file
            .split('/')
            .all(|part| !matches!(part, "" | "." | ".."))
}

fn validate(details: &InvestigationDetails) -> io::Result<()> {
    if details.title.trim().is_empty()
        || details.title.len() > 256
        || details.title.chars().any(char::is_control)
        || details.purpose.len() > 8192
        || details.next_steps.len() > 8192
        || details.hypotheses.iter().any(|text| text.len() > 2048)
    {
        return Err(invalid("investigation title or notes exceed their limits"));
    }
    if [
        details.hypotheses.len(),
        details.content_hashes.len(),
        details.build_hashes.len(),
        details.sessions.len(),
        details.sources.len(),
        details.bookmarks.len(),
        details.breakpoints.len(),
        details.traces.len(),
        details.artifacts.len(),
    ]
    .into_iter()
    .any(|count| count > MAX_ITEMS)
    {
        return Err(invalid(
            "investigation lists are limited to 64 entries each",
        ));
    }
    if details
        .sessions
        .iter()
        .any(|target| !is_hex(&target.session, 32))
        || details.bookmarks.iter().any(|bookmark| {
            bookmark.label.trim().is_empty()
                || bookmark.label.len() > 256
                || !details.sessions.contains(&bookmark.target)
        })
    {
        return Err(invalid("invalid session reference or bookmark"));
    }
    if details
        .sources
        .iter()
        .any(|location| !valid_location(location))
        || details.breakpoints.iter().any(|breakpoint| {
            !valid_location(&breakpoint.location)
                || !is_hex(&breakpoint.content_sha256, 64)
                || !is_hex(&breakpoint.elf_sha256, 64)
        })
    {
        return Err(invalid(
            "source references require relative paths, line numbers and breakpoint image hashes",
        ));
    }
    if details
        .content_hashes
        .iter()
        .chain(&details.build_hashes)
        .any(|hash| !is_hex(hash, 64))
        || details
            .artifacts
            .iter()
            .chain(&details.traces)
            .any(|reference| {
                !is_hex(&reference.sha256, 64)
                    || !reference
                        .uri
                        .strip_prefix("lotus://artifact/")
                        .is_some_and(|id| is_hex(id, 32))
            })
    {
        return Err(invalid("invalid investigation hash or artifact URI"));
    }
    Ok(())
}
