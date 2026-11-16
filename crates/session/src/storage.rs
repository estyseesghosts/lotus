//! Per-session private storage layout.
//!
//! Every session owns a directory tree that no other session writes to.
//! NAND, save data, logs and captures live under it. Snapshots are copied in,
//! never shared by reference, so two sessions started from the same snapshot
//! cannot corrupt each other.

use crate::SessionId;
use std::path::{Path, PathBuf};

/// Directory names created inside a session root.
pub const NAND_DIR: &str = "nand";
/// Save data directory.
pub const SAVES_DIR: &str = "saves";
/// Log directory.
pub const LOGS_DIR: &str = "logs";
/// Capture and trace directory.
pub const CAPTURES_DIR: &str = "captures";
/// Read only reference snapshots.
pub const SNAPSHOTS_DIR: &str = "snapshots";

/// Private storage layout of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageLayout {
    root: PathBuf,
}

impl StorageLayout {
    /// Create the directory tree for `id` below `base`.
    pub fn create(base: impl AsRef<Path>, id: SessionId) -> std::io::Result<Self> {
        let root = base.as_ref().join(id.to_string());
        for dir in [NAND_DIR, SAVES_DIR, LOGS_DIR, CAPTURES_DIR, SNAPSHOTS_DIR] {
            std::fs::create_dir_all(root.join(dir))?;
        }
        Ok(Self { root })
    }

    /// Session root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// NAND directory.
    pub fn nand(&self) -> PathBuf {
        self.root.join(NAND_DIR)
    }

    /// Save data directory.
    pub fn saves(&self) -> PathBuf {
        self.root.join(SAVES_DIR)
    }

    /// Log directory.
    pub fn logs(&self) -> PathBuf {
        self.root.join(LOGS_DIR)
    }

    /// Capture directory.
    pub fn captures(&self) -> PathBuf {
        self.root.join(CAPTURES_DIR)
    }

    /// Snapshot directory.
    pub fn snapshots(&self) -> PathBuf {
        self.root.join(SNAPSHOTS_DIR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_makes_all_directories() {
        let base = std::env::temp_dir().join(format!("lotus-storage-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let layout = StorageLayout::create(&base, SessionId(7)).unwrap();
        assert!(layout.nand().is_dir());
        assert!(layout.saves().is_dir());
        assert!(layout.logs().is_dir());
        assert!(layout.captures().is_dir());
        assert!(layout.snapshots().is_dir());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn layout_is_per_session() {
        let base =
            std::env::temp_dir().join(format!("lotus-storage-test-2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let a = StorageLayout::create(&base, SessionId(1)).unwrap();
        let b = StorageLayout::create(&base, SessionId(2)).unwrap();
        assert_ne!(a.root(), b.root());
        let _ = std::fs::remove_dir_all(&base);
    }
}
