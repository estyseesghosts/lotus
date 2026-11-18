//! Host artifact catalog with bounded retention (#27).
//!
//! Artifacts (captures, GPU/audio/movie exports, reports) are immutable files under
//! `<storage>/artifacts/`, addressed only by `lotus://artifact/<id>` URIs. The catalog lives
//! in memory, so URIs expire with the host and files left by an earlier host are removed
//! when a host opens the store.
//!
//! Limits are a global count and byte quota. Each live session is guaranteed a **floor** of
//! `quota / max_sessions`: space beyond its floor is borrowed and is reclaimed first when
//! another session needs room. Retention, in eviction order:
//! 1. Unpinned artifacts of ended sessions and of sessions above their floor, least recently
//!    used first, once their lease has expired.
//! 2. The requesting session's own unpinned, unleased artifacts, least recently used first.
//! 3. A session still below its floor may also reclaim leased (never pinned) artifacts of
//!    sessions above their floor; this is what makes the floor a guarantee.
//!
//! A new artifact is **leased** for `lease_seconds` so a caller can read what it was just
//! given. **Pinned** artifacts are never evicted; a session may pin at most its floor.
//! `release` deletes at once. A URI that was evicted or released reports `expired` with the
//! reason, distinct from an unknown URI. Files are written to `<id>.tmp` and renamed after
//! the whole batch succeeded, so the catalog never names a partial file.
//!
//! [`ArtifactStore::reserve`] plans every eviction before changing anything: a request that
//! cannot be satisfied returns `QuotaExceeded` with the catalog, tombstones, counters and
//! files untouched (#43). Once a plan is feasible the evictions are committed even if the
//! following write fails; the space stays free. Evicted files the caller could not delete
//! are retried on the next publish and removed by the next [`ArtifactStore::open`].
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lotus_protocol::envelope::SessionUuid;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Largest single artifact.
pub const MAX_ARTIFACT_BYTES: u64 = 64 << 20;
/// Artifacts accepted in one publish.
pub const MAX_BATCH: usize = 16;
/// Expired or released URIs remembered for a clear `expired` answer.
const TOMBSTONES: usize = 1024;

/// Host artifact limits; part of the host configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactQuota {
    #[serde(default = "default_max_count")]
    pub max_count: usize,
    #[serde(default = "default_max_bytes")]
    pub max_bytes: u64,
    /// Protection of a new artifact from eviction by other sessions' work.
    #[serde(default = "default_lease_seconds")]
    pub lease_seconds: u64,
}

fn default_max_count() -> usize {
    512
}
fn default_max_bytes() -> u64 {
    512 << 20
}
fn default_lease_seconds() -> u64 {
    60
}

impl Default for ArtifactQuota {
    fn default() -> Self {
        Self {
            max_count: default_max_count(),
            max_bytes: default_max_bytes(),
            lease_seconds: default_lease_seconds(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub uri: String,
    pub screen: String,
    pub width: u32,
    pub height: u32,
    pub bytes: usize,
    pub sha256: String,
    pub mime_type: String,
    /// Session that produced it.
    #[serde(default)]
    pub session: String,
    #[serde(default)]
    pub pinned: bool,
}

/// Content and description of one artifact to publish.
pub struct NewArtifact {
    pub bytes: Vec<u8>,
    pub screen: String,
    pub width: u32,
    pub height: u32,
    pub mime_type: String,
    /// File-name extension inside the store, e.g. `bgra`; none when empty.
    pub extension: &'static str,
}

#[derive(Debug)]
struct Entry {
    artifact: Artifact,
    path: PathBuf,
    leased_until: Instant,
    used: u64,
}

/// Usage per session, as reported by [`ArtifactStore::usage`].
#[derive(Debug, Clone, Default, Serialize)]
pub struct SessionUsage {
    pub count: usize,
    pub bytes: u64,
    pub pinned: usize,
    pub live: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Usage {
    pub count: usize,
    pub bytes: u64,
    pub max_count: usize,
    pub max_bytes: u64,
    pub floor_count: usize,
    pub floor_bytes: u64,
    pub lease_seconds: u64,
    pub evicted: u64,
    pub released: u64,
    pub sessions: BTreeMap<String, SessionUsage>,
}

/// The catalog. Callers hold it behind a mutex only for bookkeeping, never for file I/O:
/// [`Self::reserve`] makes room, files are written unlocked, [`Self::commit`] publishes.
#[derive(Debug)]
pub struct ArtifactStore {
    directory: PathBuf,
    quota: ArtifactQuota,
    max_sessions: usize,
    entries: BTreeMap<String, Entry>,
    /// Space promised to publishes still writing, per session.
    reserved: BTreeMap<String, (usize, u64)>,
    tombstones: VecDeque<(String, &'static str)>,
    clock: u64,
    evicted: u64,
    released: u64,
    /// Files of removed artifacts whose deletion failed; retried by the next publish.
    undeleted: Vec<PathBuf>,
}

/// Room made by [`ArtifactStore::reserve`]; files of evicted artifacts are deleted by the
/// caller outside the lock.
pub struct Reservation {
    pub session: String,
    pub count: usize,
    pub bytes: u64,
    pub evicted: Vec<PathBuf>,
}

impl ArtifactStore {
    /// Open the store, removing files no catalog can name any more.
    pub fn open(directory: PathBuf, quota: ArtifactQuota, max_sessions: usize) -> io::Result<Self> {
        if quota.max_count == 0 || quota.max_bytes == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "artifact quota must allow at least one artifact and byte",
            ));
        }
        fs::create_dir_all(&directory)?;
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                fs::remove_file(entry.path())?;
            }
        }
        Ok(Self {
            directory,
            quota,
            max_sessions: max_sessions.max(1),
            entries: BTreeMap::new(),
            reserved: BTreeMap::new(),
            tombstones: VecDeque::new(),
            clock: 0,
            evicted: 0,
            released: 0,
            undeleted: Vec::new(),
        })
    }

    fn floor(&self) -> (usize, u64) {
        (
            self.quota.max_count / self.max_sessions,
            self.quota.max_bytes / self.max_sessions as u64,
        )
    }

    fn held(&self, session: &str) -> (usize, u64) {
        self.held_without(session, &BTreeSet::new())
    }

    /// [`Self::held`] as if the artifacts in `planned` were already evicted.
    fn held_without(&self, session: &str, planned: &BTreeSet<String>) -> (usize, u64) {
        let (mut count, mut bytes) = self.reserved.get(session).copied().unwrap_or_default();
        for (uri, entry) in &self.entries {
            if entry.artifact.session == session && !planned.contains(uri) {
                count += 1;
                bytes += entry.artifact.bytes as u64;
            }
        }
        (count, bytes)
    }

    fn total(&self) -> (usize, u64) {
        self.total_without(&BTreeSet::new())
    }

    fn total_without(&self, planned: &BTreeSet<String>) -> (usize, u64) {
        let (mut count, mut bytes) = self
            .reserved
            .values()
            .fold((0, 0), |(c, b), (rc, rb)| (c + rc, b + rb));
        for (uri, entry) in &self.entries {
            if planned.contains(uri) {
                continue;
            }
            count += 1;
            bytes += entry.artifact.bytes as u64;
        }
        (count, bytes)
    }

    /// Make room for `count` artifacts of `bytes` total for `session`. `live` lists the
    /// sessions that currently exist; only they have a floor. Concurrent publishes see each
    /// other's reservations. On error nothing observable has changed.
    pub fn reserve(
        &mut self,
        session: &str,
        count: usize,
        bytes: u64,
        live: &[String],
        now: Instant,
    ) -> io::Result<Reservation> {
        if count > MAX_BATCH {
            return Err(io::Error::other("artifact batch exceeds budget"));
        }
        if bytes > self.quota.max_bytes || count > self.quota.max_count {
            return Err(io::Error::new(
                io::ErrorKind::QuotaExceeded,
                "artifact larger than the host artifact quota",
            ));
        }
        let floor = self.floor();
        let (own_count, own_bytes) = self.held(session);
        let below_floor = own_count + count <= floor.0 && own_bytes + bytes <= floor.1;
        // Phase 1: choose victims against a hypothetical catalog without them.
        let mut planned = BTreeSet::new();
        let mut order = Vec::new();
        loop {
            let (total_count, total_bytes) = self.total_without(&planned);
            if total_count + count <= self.quota.max_count
                && total_bytes + bytes <= self.quota.max_bytes
            {
                break;
            }
            let over_floor = |store: &Self, owner: &str| {
                if !live.iter().any(|id| id == owner) {
                    return true;
                }
                let (c, b) = store.held_without(owner, &planned);
                c > floor.0 || b > floor.1
            };
            let candidates = || {
                self.entries
                    .iter()
                    .filter(|(uri, e)| !e.artifact.pinned && !planned.contains(*uri))
            };
            let victim = candidates()
                .filter(|(_, e)| e.artifact.session != session)
                .filter(|(_, e)| over_floor(self, &e.artifact.session))
                .filter(|(_, e)| e.leased_until <= now)
                .min_by_key(|(_, e)| e.used)
                .or_else(|| {
                    candidates()
                        .filter(|(_, e)| e.artifact.session == session)
                        .filter(|(_, e)| e.leased_until <= now)
                        .min_by_key(|(_, e)| e.used)
                })
                .or_else(|| {
                    // Floor guarantee: borrowed space is reclaimed even while leased.
                    candidates()
                        .filter(|_| below_floor)
                        .filter(|(_, e)| e.artifact.session != session)
                        .filter(|(_, e)| over_floor(self, &e.artifact.session))
                        .min_by_key(|(_, e)| e.used)
                })
                .map(|(uri, _)| uri.clone());
            let Some(uri) = victim else {
                return Err(io::Error::new(
                    io::ErrorKind::QuotaExceeded,
                    format!(
                        "artifact quota reached ({} artifacts, {} bytes, {} per session guaranteed); the rest is pinned or leased: release or unpin artifacts",
                        self.quota.max_count, self.quota.max_bytes, floor.1
                    ),
                ));
            };
            planned.insert(uri.clone());
            order.push(uri);
        }
        // Phase 2: the plan is feasible; commit it and the reservation together.
        let mut evicted = Vec::with_capacity(order.len());
        for uri in order {
            let entry = self.entries.remove(&uri).expect("victim is catalogued");
            self.tombstone(uri, "evicted to make room under the host artifact quota");
            self.evicted += 1;
            evicted.push(entry.path);
        }
        let held = self.reserved.entry(session.to_string()).or_default();
        held.0 += count;
        held.1 += bytes;
        Ok(Reservation {
            session: session.to_string(),
            count,
            bytes,
            evicted,
        })
    }

    fn unreserve(&mut self, reservation: &Reservation) {
        if let Some(held) = self.reserved.get_mut(&reservation.session) {
            held.0 -= reservation.count;
            held.1 -= reservation.bytes;
            if *held == (0, 0) {
                self.reserved.remove(&reservation.session);
            }
        }
    }

    fn tombstone(&mut self, uri: String, reason: &'static str) {
        if self.tombstones.len() == TOMBSTONES {
            self.tombstones.pop_front();
        }
        self.tombstones.push_back((uri, reason));
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Publish files written by [`write_batch`] under `reservation`.
    pub fn commit(
        &mut self,
        reservation: &Reservation,
        written: Vec<(Artifact, PathBuf)>,
        now: Instant,
    ) -> Vec<Artifact> {
        self.unreserve(reservation);
        let lease = now + Duration::from_secs(self.quota.lease_seconds);
        written
            .into_iter()
            .map(|(artifact, path)| {
                self.clock += 1;
                self.entries.insert(
                    artifact.uri.clone(),
                    Entry {
                        artifact: artifact.clone(),
                        path,
                        leased_until: lease,
                        used: self.clock,
                    },
                );
                artifact
            })
            .collect()
    }

    /// Files whose deletion failed earlier, for the caller to retry outside the lock.
    pub fn take_undeleted(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.undeleted)
    }

    /// Record files the caller failed to delete; retried by a later publish.
    pub fn defer_delete(&mut self, paths: Vec<PathBuf>) {
        self.undeleted.extend(paths);
    }

    /// A publish failed after [`Self::reserve`]; give the space back.
    pub fn abandon(&mut self, reservation: &Reservation) {
        self.unreserve(reservation);
    }

    /// Path and identity of a catalogued artifact; marks it used.
    pub fn open_artifact(&mut self, uri: &str) -> io::Result<(Artifact, PathBuf)> {
        self.clock += 1;
        let clock = self.clock;
        if let Some(entry) = self.entries.get_mut(uri) {
            entry.used = clock;
            return Ok((entry.artifact.clone(), entry.path.clone()));
        }
        Err(self.missing(uri))
    }

    fn missing(&self, uri: &str) -> io::Error {
        match self.tombstones.iter().rev().find(|(old, _)| old == uri) {
            Some((_, reason)) => io::Error::new(
                io::ErrorKind::NotFound,
                format!("artifact expired: {reason}"),
            ),
            None => io::Error::new(io::ErrorKind::PermissionDenied, "unknown artifact resource"),
        }
    }

    /// Remove artifacts now; returns their files for deletion outside the lock.
    pub fn release(&mut self, uris: &[String]) -> io::Result<Vec<PathBuf>> {
        if let Some(uri) = uris.iter().find(|uri| !self.entries.contains_key(*uri)) {
            return Err(self.missing(uri));
        }
        let mut files = Vec::new();
        for uri in uris {
            if let Some(entry) = self.entries.remove(uri) {
                files.push(entry.path);
                self.tombstone(uri.clone(), "released");
                self.released += 1;
            }
        }
        Ok(files)
    }

    /// Pin or unpin. A session may pin at most its floor, so pins never starve others.
    pub fn pin(&mut self, uri: &str, pinned: bool) -> io::Result<Artifact> {
        let floor = self.floor();
        let Some(entry) = self.entries.get(uri) else {
            return Err(self.missing(uri));
        };
        if pinned && !entry.artifact.pinned {
            let session = entry.artifact.session.clone();
            let (count, bytes) = self
                .entries
                .values()
                .filter(|e| e.artifact.session == session && e.artifact.pinned)
                .fold((1, entry.artifact.bytes as u64), |(c, b), e| {
                    (c + 1, b + e.artifact.bytes as u64)
                });
            if count > floor.0 || bytes > floor.1 {
                return Err(io::Error::new(
                    io::ErrorKind::QuotaExceeded,
                    "pinning this would exceed the session's guaranteed artifact share",
                ));
            }
        }
        let entry = self.entries.get_mut(uri).expect("checked above");
        entry.artifact.pinned = pinned;
        Ok(entry.artifact.clone())
    }

    pub fn list(&self) -> Vec<Artifact> {
        self.entries.values().map(|e| e.artifact.clone()).collect()
    }

    pub fn usage(&self, live: &[String]) -> Usage {
        let (count, bytes) = self.total();
        let floor = self.floor();
        let mut sessions: BTreeMap<String, SessionUsage> = live
            .iter()
            .map(|id| {
                (
                    id.clone(),
                    SessionUsage {
                        live: true,
                        ..SessionUsage::default()
                    },
                )
            })
            .collect();
        for entry in self.entries.values() {
            let usage = sessions.entry(entry.artifact.session.clone()).or_default();
            usage.count += 1;
            usage.bytes += entry.artifact.bytes as u64;
            usage.pinned += usize::from(entry.artifact.pinned);
        }
        Usage {
            count,
            bytes,
            max_count: self.quota.max_count,
            max_bytes: self.quota.max_bytes,
            floor_count: floor.0,
            floor_bytes: floor.1,
            lease_seconds: self.quota.lease_seconds,
            evicted: self.evicted,
            released: self.released,
            sessions,
        }
    }
}

/// Write a batch to `<id>.tmp`, then rename every file; on any failure remove all of them.
pub fn write_batch(
    directory: &Path,
    session: &str,
    batch: Vec<NewArtifact>,
) -> io::Result<Vec<(Artifact, PathBuf)>> {
    write_batch_with(directory, session, batch, |path, bytes| {
        fs::write(path, bytes)
    })
}

fn write_batch_with(
    directory: &Path,
    session: &str,
    batch: Vec<NewArtifact>,
    mut write: impl FnMut(&Path, &[u8]) -> io::Result<()>,
) -> io::Result<Vec<(Artifact, PathBuf)>> {
    let mut staged: Vec<(Artifact, PathBuf, PathBuf)> = Vec::new();
    let result = (|| {
        for item in batch {
            let id = SessionUuid::generate().to_string();
            let name = if item.extension.is_empty() {
                id.clone()
            } else {
                format!("{id}.{}", item.extension)
            };
            let path = directory.join(&name);
            let temporary = directory.join(format!("{name}.tmp"));
            staged.push((
                Artifact {
                    uri: format!("lotus://artifact/{id}"),
                    screen: item.screen,
                    width: item.width,
                    height: item.height,
                    bytes: item.bytes.len(),
                    sha256: format!("{:x}", Sha256::digest(&item.bytes)),
                    mime_type: item.mime_type,
                    session: session.to_string(),
                    pinned: false,
                },
                path,
                temporary.clone(),
            ));
            write(&temporary, &item.bytes)?;
        }
        for (_, path, temporary) in &staged {
            fs::rename(temporary, path)?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok(staged
            .into_iter()
            .map(|(artifact, path, _)| (artifact, path))
            .collect()),
        Err(error) => {
            for (_, path, temporary) in &staged {
                let _ = fs::remove_file(temporary);
                let _ = fs::remove_file(path);
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str, quota: ArtifactQuota, sessions: usize) -> (PathBuf, ArtifactStore) {
        let base =
            std::env::temp_dir().join(format!("lotus-art-{name}-{}", SessionUuid::generate()));
        let store = ArtifactStore::open(base.join("artifacts"), quota, sessions).expect("open");
        (base, store)
    }

    fn item(bytes: usize) -> NewArtifact {
        NewArtifact {
            bytes: vec![7; bytes],
            screen: "top".into(),
            width: 0,
            height: 0,
            mime_type: "application/octet-stream".into(),
            extension: "bgra",
        }
    }

    fn publish(
        store: &mut ArtifactStore,
        session: &str,
        sizes: &[usize],
        live: &[String],
        now: Instant,
    ) -> io::Result<Vec<Artifact>> {
        let total = sizes.iter().map(|s| *s as u64).sum();
        let reservation = store.reserve(session, sizes.len(), total, live, now)?;
        for path in &reservation.evicted {
            fs::remove_file(path)?;
        }
        let written = write_batch(
            store.directory(),
            session,
            sizes.iter().map(|size| item(*size)).collect(),
        )?;
        Ok(store.commit(&reservation, written, now))
    }

    fn files(store: &ArtifactStore) -> usize {
        fs::read_dir(store.directory()).expect("dir").count()
    }

    #[test]
    fn capture_release_cycles_never_exhaust_the_catalog() {
        let quota = ArtifactQuota {
            max_count: 8,
            max_bytes: 1 << 20,
            lease_seconds: 0,
        };
        let (base, mut store) = store("cycle", quota, 1);
        let live = vec!["a".to_string()];
        let now = Instant::now();
        let first = publish(&mut store, "a", &[100], &live, now).expect("first");
        let kept = store.pin(&first[0].uri, true).expect("pin");
        let kept_path = store.open_artifact(&kept.uri).expect("open").1;
        let kept_bytes = fs::read(&kept_path).expect("kept");
        for round in 0..300 {
            let made = publish(&mut store, "a", &[64, 64], &live, now).expect("publish");
            if round % 2 == 0 {
                for path in store
                    .release(&made.iter().map(|a| a.uri.clone()).collect::<Vec<_>>())
                    .expect("release")
                {
                    fs::remove_file(path).expect("remove");
                }
            }
            assert!(files(&store) <= 8);
        }
        assert_eq!(fs::read(&kept_path).expect("pinned file"), kept_bytes);
        assert!(store.usage(&live).evicted > 0);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn evicted_and_released_uris_report_expired() {
        let quota = ArtifactQuota {
            max_count: 2,
            max_bytes: 1 << 20,
            lease_seconds: 0,
        };
        let (base, mut store) = store("expired", quota, 1);
        let live = vec!["a".to_string()];
        let now = Instant::now();
        let first = publish(&mut store, "a", &[1], &live, now).expect("one");
        let second = publish(&mut store, "a", &[1], &live, now).expect("two");
        publish(&mut store, "a", &[1], &live, now).expect("three evicts one");
        let error = store.open_artifact(&first[0].uri).expect_err("evicted");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(error.to_string().contains("expired"));
        store.release(&[second[0].uri.clone()]).expect("release");
        assert!(store
            .open_artifact(&second[0].uri)
            .expect_err("released")
            .to_string()
            .contains("released"));
        let unknown = store
            .open_artifact("lotus://artifact/nope")
            .expect_err("unknown");
        assert_eq!(unknown.kind(), io::ErrorKind::PermissionDenied);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn a_noisy_session_cannot_take_another_sessions_floor() {
        let quota = ArtifactQuota {
            max_count: 12,
            max_bytes: 1 << 20,
            lease_seconds: 3600,
        };
        let (base, mut store) = store("floor", quota, 3);
        let live: Vec<String> = ["noisy", "b", "c"].iter().map(|s| s.to_string()).collect();
        let now = Instant::now();
        // The noisy session fills the whole store; everything is still leased.
        for _ in 0..12 {
            publish(&mut store, "noisy", &[1], &live, now).expect("noisy");
        }
        assert!(
            publish(&mut store, "noisy", &[1], &live, now).is_err(),
            "leases hold"
        );
        // b and c each still get their 4-artifact floor, reclaimed from noisy's borrowings.
        for session in ["b", "c"] {
            for _ in 0..4 {
                publish(&mut store, session, &[1], &live, now).expect("floor is guaranteed");
            }
        }
        let usage = store.usage(&live);
        assert_eq!(usage.sessions["b"].count, 4);
        assert_eq!(usage.sessions["c"].count, 4);
        assert_eq!(usage.sessions["noisy"].count, 4);
        assert!(
            publish(&mut store, "b", &[1], &live, now).is_err(),
            "b is at its floor"
        );
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn pins_are_limited_to_the_floor_and_never_evicted() {
        let quota = ArtifactQuota {
            max_count: 4,
            max_bytes: 1 << 20,
            lease_seconds: 0,
        };
        let (base, mut store) = store("pins", quota, 2);
        let live = vec!["a".to_string(), "b".to_string()];
        let now = Instant::now();
        let made = publish(&mut store, "a", &[1, 1, 1], &live, now).expect("three");
        store.pin(&made[0].uri, true).expect("pin 1");
        store.pin(&made[1].uri, true).expect("pin 2");
        assert!(store.pin(&made[2].uri, true).is_err(), "floor is 2");
        for _ in 0..10 {
            publish(&mut store, "b", &[1], &live, now).expect("b");
        }
        assert!(store.open_artifact(&made[0].uri).is_ok());
        assert!(store.open_artifact(&made[1].uri).is_ok());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn a_failed_write_leaves_no_file_and_no_entry() {
        let (base, mut store) = store("partial", ArtifactQuota::default(), 1);
        let live = vec!["a".to_string()];
        let reservation = store
            .reserve("a", 2, 20, &live, Instant::now())
            .expect("reserve");
        // The first file is written, the second fails mid-batch.
        let mut calls = 0;
        let result = write_batch_with(
            store.directory(),
            "a",
            vec![item(10), item(10)],
            |path, bytes| {
                calls += 1;
                if calls == 2 {
                    fs::write(path, &bytes[..3])?;
                    return Err(io::Error::other("disk full"));
                }
                fs::write(path, bytes)
            },
        );
        assert!(result.is_err());
        store.abandon(&reservation);
        assert_eq!(files(&store), 0);
        assert!(store.list().is_empty());
        assert_eq!(store.usage(&live).count, 0, "reservation returned");
        let _ = fs::remove_dir_all(base);
    }

    /// Catalog, tombstone and counter state visible to callers.
    fn snapshot(store: &mut ArtifactStore, uris: &[String], live: &[String]) -> String {
        let usage = serde_json::to_string(&store.usage(live)).expect("usage");
        let opened: Vec<String> = uris
            .iter()
            .map(|uri| match store.open_artifact(uri) {
                Ok((artifact, path)) => format!("{}:{}", artifact.uri, path.exists()),
                Err(error) => error.to_string(),
            })
            .collect();
        format!("{usage} {opened:?} {:?}", store.tombstones)
    }

    #[test]
    fn an_infeasible_reservation_changes_nothing() {
        // Two artifacts must go for a 2-artifact publish; only one is evictable.
        let quota = ArtifactQuota {
            max_count: 3,
            max_bytes: 1 << 20,
            lease_seconds: 0,
        };
        let (base, mut store) = store("atomic", quota, 1);
        let live = vec!["a".to_string()];
        let now = Instant::now();
        let made = publish(&mut store, "a", &[1, 1, 1], &live, now).expect("fill");
        store.pin(&made[0].uri, true).expect("pin");
        store.pin(&made[1].uri, true).expect("pin");
        let uris: Vec<String> = made.iter().map(|a| a.uri.clone()).collect();
        let before = snapshot(&mut store, &uris, &live);
        let error = store
            .reserve("a", 3, 3, &live, now)
            .err()
            .expect("needs three victims, has one");
        assert_eq!(error.kind(), io::ErrorKind::QuotaExceeded);
        assert_eq!(snapshot(&mut store, &uris, &live), before);
        assert_eq!(store.usage(&live).evicted, 0);
        assert_eq!(files(&store), 3);
        // The same holds when a concurrent reservation takes the last free capacity.
        let quota = ArtifactQuota {
            max_count: 4,
            max_bytes: 1 << 20,
            lease_seconds: 3600,
        };
        let (base2, mut store2) = self::store("atomic2", quota, 2);
        let live = vec!["a".to_string(), "b".to_string()];
        let held = publish(&mut store2, "a", &[1, 1], &live, now).expect("a");
        let first = store2.reserve("b", 1, 1, &live, now).expect("b first");
        let second = store2.reserve("b", 1, 1, &live, now).expect("b second");
        // Store is now full (2 held, 2 reserved) and every entry is leased: b is at its floor.
        let uris: Vec<String> = held.iter().map(|a| a.uri.clone()).collect();
        let before = snapshot(&mut store2, &uris, &live);
        assert!(store2.reserve("b", 1, 1, &live, now).is_err());
        assert!(store2.reserve("a", 1, 1, &live, now).is_err());
        assert_eq!(snapshot(&mut store2, &uris, &live), before);
        store2.abandon(&first);
        store2.abandon(&second);
        let _ = fs::remove_dir_all(base);
        let _ = fs::remove_dir_all(base2);
    }

    #[test]
    fn a_write_failure_after_eviction_keeps_the_eviction_and_frees_the_reservation() {
        let quota = ArtifactQuota {
            max_count: 2,
            max_bytes: 1 << 20,
            lease_seconds: 0,
        };
        let (base, mut store) = store("evictfail", quota, 1);
        let live = vec!["a".to_string()];
        let now = Instant::now();
        let old = publish(&mut store, "a", &[1, 1], &live, now).expect("fill");
        let reservation = store.reserve("a", 1, 1, &live, now).expect("feasible");
        assert_eq!(reservation.evicted.len(), 1);
        store.abandon(&reservation);
        // The evicted URI is expired; the survivor is readable; the slot is free again.
        let expired = old
            .iter()
            .filter(|a| store.open_artifact(&a.uri).is_err())
            .count();
        assert_eq!(expired, 1);
        assert_eq!(store.usage(&live).count, 1);
        store.defer_delete(reservation.evicted.clone());
        assert_eq!(store.take_undeleted(), reservation.evicted);
        assert!(store.take_undeleted().is_empty());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn reopening_removes_orphans_and_temporaries() {
        let (base, store) = store("orphans", ArtifactQuota::default(), 1);
        fs::write(store.directory().join("old.bgra"), b"x").expect("orphan");
        fs::write(store.directory().join("half.bgra.tmp"), b"x").expect("partial");
        let reopened =
            ArtifactStore::open(store.directory().to_path_buf(), ArtifactQuota::default(), 1)
                .expect("reopen");
        assert_eq!(files(&reopened), 0);
        let _ = fs::remove_dir_all(base);
    }
}
