//! Primary cards and secondary scratch roots below the host storage root.
//!
//! A primary session mounts `cards/<name>/` as its worker root, so Azahar's
//! `sdmc/`, `nand/` and `sysdata/` (`common_paths.h`) persist between runs. A
//! secondary session gets `scratch/<session>/`: empty, or a point-in-time host
//! copy of one card's guest directories. The copy is not coordinated with a
//! running parent's guest writes. Scratch guest data is deleted when the
//! secondary ends; worker logs are kept only when it ended abnormally.
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

/// Guest-visible directories copied into a secondary from its parent card.
pub const GUEST_DIRS: [&str; 3] = ["sdmc", "nand", "sysdata"];
/// Largest parent copy accepted for one secondary.
pub const MAX_COPY_BYTES: u64 = 4 << 30;
/// Host diagnostics retained after an abnormal end.
const RETAINED: [&str; 3] = ["worker.stderr.log", "log", "native-manifest.json"];

/// Owns the `cards/` and `scratch/` directories of one storage root.
#[derive(Debug, Clone)]
pub struct CardStore {
    cards: PathBuf,
    scratch: PathBuf,
}

impl CardStore {
    /// Create both directories and discard scratch data left by a previous host.
    pub fn open(storage_root: &Path) -> io::Result<Self> {
        let store = Self {
            cards: storage_root.join("cards"),
            scratch: storage_root.join("scratch"),
        };
        fs::create_dir_all(&store.cards)?;
        fs::create_dir_all(&store.scratch)?;
        store.sweep()?;
        Ok(store)
    }

    /// Card names in sorted order.
    pub fn list(&self) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(&self.cards)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type()?.is_dir() && validate_name(&name).is_ok() {
                names.push(name);
            }
        }
        names.sort();
        Ok(names)
    }

    /// Root of a primary card, created on first use.
    pub fn primary_root(&self, name: &str) -> io::Result<PathBuf> {
        validate_name(name)?;
        let root = self.cards.join(name);
        fs::create_dir_all(&root)?;
        if is_locked(&root)? {
            return Err(io::Error::new(
                io::ErrorKind::ResourceBusy,
                format!("card '{name}' is mounted by another LOTUS process"),
            ));
        }
        Ok(root)
    }

    /// Create a scratch root, optionally seeded from a card's guest directories.
    pub fn create_scratch(&self, session: &str, parent: Option<&str>) -> io::Result<PathBuf> {
        let root = self.scratch.join(session);
        fs::create_dir(&root)?;
        let Some(parent) = parent else {
            return Ok(root);
        };
        let result = (|| {
            validate_name(parent)?;
            let source = self.cards.join(parent);
            if !source.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("unknown card '{parent}'"),
                ));
            }
            let mut budget = MAX_COPY_BYTES;
            for name in GUEST_DIRS {
                let from = source.join(name);
                if from.is_dir() {
                    copy_tree(&from, &root.join(name), &mut budget)?;
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            let _ = fs::remove_dir_all(&root);
            return Err(error);
        }
        Ok(root)
    }

    /// End a secondary. A clean end removes everything; otherwise logs remain.
    pub fn discard_scratch(&self, root: &Path, end: ScratchEnd) -> io::Result<()> {
        if root.parent() != Some(self.scratch.as_path()) {
            return Err(io::Error::other("refusing to discard a non-scratch root"));
        }
        match end {
            ScratchEnd::Clean => fs::remove_dir_all(root),
            ScratchEnd::Abnormal => discard_guest_data(root),
        }
    }

    /// Unlocked scratch roots belong to no live worker: drop their guest data.
    fn sweep(&self) -> io::Result<()> {
        for entry in fs::read_dir(&self.scratch)? {
            let root = entry?.path();
            if root.is_dir() && !is_locked(&root)? {
                discard_guest_data(&root)?;
            }
        }
        Ok(())
    }
}

/// How a secondary ended, which decides whether its logs are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScratchEnd {
    Clean,
    Abnormal,
}

/// Card names: 1–32 of `a-z 0-9 - _`, starting with a letter or digit.
pub fn validate_name(name: &str) -> io::Result<()> {
    let valid = (1..=32).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if valid {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "card name must be 1-32 of a-z, 0-9, '-', '_' and start with a letter or digit",
        ))
    }
}

/// Whether a live worker holds this root's lock. The worker's lock stays authoritative.
fn is_locked(root: &Path) -> io::Result<bool> {
    let path = root.join("worker.lock");
    if !path.exists() {
        return Ok(false);
    }
    let file = File::options().read(true).write(true).open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(false),
        Err(fs::TryLockError::WouldBlock) => Ok(true),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

fn discard_guest_data(root: &Path) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if RETAINED.iter().any(|kept| entry.file_name() == *kept) {
            continue;
        }
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// Copy regular files and directories; links are refused rather than followed.
fn copy_tree(from: &Path, to: &Path, budget: &mut u64) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target, budget)?;
        } else if kind.is_file() {
            let size = entry.metadata()?.len();
            *budget = budget.checked_sub(size).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::QuotaExceeded,
                    "parent card exceeds the 4 GiB secondary copy limit",
                )
            })?;
            fs::copy(entry.path(), target)?;
        } else {
            return Err(io::Error::other(format!(
                "refusing to copy link {}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

/// Called while the source worker is paused at a checkpoint. Every target is private.
pub(crate) fn copy_checkpoint_root(from: &Path, to: &Path) -> io::Result<()> {
    let mut budget = MAX_COPY_BYTES;
    for directory in ["sdmc", "nand", "sysdata", "checkpoints", "media"] {
        let source = from.join(directory);
        if source.is_dir() {
            if fs::symlink_metadata(&source)?.file_type().is_symlink() {
                return Err(io::Error::other(
                    "checkpoint root contains a directory link",
                ));
            }
            copy_tree(&source, &to.join(directory), &mut budget)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str) -> (PathBuf, CardStore) {
        let base = std::env::temp_dir().join(format!(
            "lotus-cards-{name}-{}",
            lotus_protocol::envelope::SessionUuid::generate()
        ));
        let store = CardStore::open(&base).expect("open store");
        (base, store)
    }

    #[test]
    fn names_reject_paths_and_uppercase() {
        for bad in ["", "..", "a/b", "a\\b", "Default", "-x", &"x".repeat(33)] {
            assert!(validate_name(bad).is_err(), "accepted {bad:?}");
        }
        for good in ["default", "hgss-dev", "p_2", "0"] {
            assert!(validate_name(good).is_ok(), "rejected {good:?}");
        }
    }

    #[test]
    fn secondary_copies_guest_dirs_and_writes_never_reach_parent() {
        let (base, store) = store("copy");
        let card = store.primary_root("dev").expect("card");
        fs::create_dir_all(card.join("sdmc/3ds")).expect("sdmc");
        fs::write(card.join("sdmc/3ds/save.bin"), b"parent").expect("seed");
        fs::write(card.join("worker.stderr.log"), b"not guest").expect("log");

        let scratch = store.create_scratch("s1", Some("dev")).expect("scratch");
        assert_eq!(
            fs::read(scratch.join("sdmc/3ds/save.bin")).expect("copied"),
            b"parent"
        );
        assert!(!scratch.join("worker.stderr.log").exists());
        fs::write(scratch.join("sdmc/3ds/save.bin"), b"child").expect("child write");
        assert_eq!(
            fs::read(card.join("sdmc/3ds/save.bin")).expect("parent"),
            b"parent"
        );

        store
            .discard_scratch(&scratch, ScratchEnd::Clean)
            .expect("discard");
        assert!(!scratch.exists());
        assert_eq!(store.list().expect("list"), vec!["dev".to_string()]);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn abnormal_end_keeps_logs_only_and_unknown_parent_fails_cleanly() {
        let (base, store) = store("abnormal");
        let scratch = store.create_scratch("s2", None).expect("empty scratch");
        fs::create_dir_all(scratch.join("sdmc")).expect("sdmc");
        fs::write(scratch.join("worker.stderr.log"), b"crash").expect("log");
        store
            .discard_scratch(&scratch, ScratchEnd::Abnormal)
            .expect("discard");
        assert!(!scratch.join("sdmc").exists());
        assert!(scratch.join("worker.stderr.log").exists());

        assert!(store.create_scratch("s3", Some("missing")).is_err());
        assert!(!base.join("scratch/s3").exists());
        assert!(store
            .discard_scratch(&base.join("cards"), ScratchEnd::Clean)
            .is_err());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn reopening_sweeps_unlocked_scratch_guest_data() {
        let (base, store) = store("sweep");
        let scratch = store.create_scratch("left", None).expect("scratch");
        fs::create_dir_all(scratch.join("nand")).expect("nand");
        drop(store);
        CardStore::open(&base).expect("reopen");
        assert!(!scratch.join("nand").exists());
        let _ = fs::remove_dir_all(base);
    }
}
