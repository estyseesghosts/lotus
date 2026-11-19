//! Exclusive `worker.lock` acquisition (#116).
//!
//! On Unix, std's `File::try_lock` is `flock`, which belongs to the open file description.
//! A `fork` on another thread copies the descriptor into the child until its `exec` closes it
//! (`O_CLOEXEC`), so a lock can stay held briefly after its owner closed it; ~300 µs was
//! measured with parallel worker spawns. `WouldBlock` is retried for [`SETTLE`] before it is
//! reported. A lock held by a live process still fails, only later. Windows handles are not
//! inherited by `std` children, so there it is a single attempt.

use std::fs::{File, TryLockError};

/// Longest wait for a transiently inherited copy to close.
#[cfg(unix)]
pub(crate) const SETTLE: std::time::Duration = std::time::Duration::from_millis(250);

pub(crate) fn try_lock(file: &File) -> Result<(), TryLockError> {
    #[cfg(unix)]
    {
        let start = std::time::Instant::now();
        loop {
            match file.try_lock() {
                Err(TryLockError::WouldBlock) if start.elapsed() < SETTLE => {
                    std::thread::sleep(std::time::Duration::from_micros(200));
                }
                result => return result,
            }
        }
    }
    #[cfg(not(unix))]
    file.try_lock()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Duration;

    fn lock_file(name: &str) -> (std::path::PathBuf, File) {
        let dir =
            std::env::temp_dir().join(format!("lotus-root-lock-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("worker.lock");
        let file = File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        (dir, file)
    }

    #[test]
    fn lock_released_by_a_short_lived_holder_is_acquired() {
        let (dir, holder) = lock_file("transient");
        holder.try_lock().unwrap();
        let contender = File::options()
            .read(true)
            .write(true)
            .open(dir.join("worker.lock"))
            .unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(5));
            drop(holder);
        });
        try_lock(&contender).expect("lock freed within SETTLE");
        release.join().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lock_held_for_longer_is_refused() {
        let (dir, holder) = lock_file("held");
        holder.try_lock().unwrap();
        let contender = File::options()
            .read(true)
            .write(true)
            .open(dir.join("worker.lock"))
            .unwrap();
        assert!(matches!(
            try_lock(&contender),
            Err(TryLockError::WouldBlock)
        ));
        drop(holder);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
