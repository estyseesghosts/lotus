//! Read-only shader cache seeds for new worker roots (#30, opt-in `HostConfig::shader_seed`).
//!
//! Azahar keeps its caches under the user directory's `shaders/` (`SHADER_DIR`,
//! `common/common_paths.h`); for Vulkan the driver pipeline cache is
//! `shaders/vulkan/pipeline/<program>-<vendor><device>.bin`, written by
//! `PipelineCache::SaveDriverPipelineDiskCache` at renderer shutdown and checked by
//! `IsCacheValid` (header, vendor and device ID) before use, which deletes an invalid file
//! (`video_core/renderer_vulkan/vk_pipeline_cache.cpp`).
//!
//! A seed is `<storage>/shader-seed/<worker sha256>/shaders/`. A new root without `shaders/`
//! gets a private copy; the worker writes only its copy, never the seed. After a **clean**
//! stop the supervisor publishes the root's files into the seed, one file at a time through
//! a temporary name and a rename, serialized within the host. A crashed worker never
//! publishes. Keying by worker binary keeps a rebuilt worker from reading another build's
//! caches; the driver's own header check covers GPU and driver changes.
use std::fs;
use std::io;
use std::path::Path;

/// Largest seed tree copied into a root.
const MAX_SEED_BYTES: u64 = 512 << 20;

/// Copy the seed for `worker_sha` into `root/shaders` unless the root already has caches.
/// Returns `seeded`, `cold` (no seed yet) or `kept`.
pub fn seed(seeds: &Path, worker_sha: &str, root: &Path) -> io::Result<&'static str> {
    let target = root.join("shaders");
    if target.exists() {
        return Ok("kept");
    }
    let source = seeds.join(worker_sha).join("shaders");
    if !source.is_dir() {
        return Ok("cold");
    }
    let staging = root.join("shaders.seeding");
    let _ = fs::remove_dir_all(&staging);
    let mut budget = MAX_SEED_BYTES;
    let copied =
        copy_files(&source, &staging, &mut budget).and_then(|()| fs::rename(&staging, &target));
    if let Err(error) = copied {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    Ok("seeded")
}

/// Publish files of a cleanly stopped root into the seed, replacing files whose size or
/// modification time differ. Each file is renamed into place, so a concurrent seeding
/// copies either the old or the new file, never a partial one.
pub fn publish(seeds: &Path, worker_sha: &str, root: &Path) -> io::Result<()> {
    let source = root.join("shaders");
    if !source.is_dir()
        || worker_sha.len() != 64
        || !worker_sha.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Ok(());
    }
    publish_tree(&source, &seeds.join(worker_sha).join("shaders"))
}

fn publish_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            publish_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            let metadata = entry.metadata()?;
            let current = fs::metadata(&target).ok();
            let unchanged = current.is_some_and(|current| {
                current.len() == metadata.len()
                    && current.modified().ok() >= metadata.modified().ok()
            });
            if unchanged {
                continue;
            }
            // Hosts can share a storage directory. A unique temporary path keeps
            // simultaneous publishers from writing through each other's handle.
            let nonce = lotus_protocol::envelope::SessionUuid::generate();
            let temporary = to.join(format!("{nonce}.publishing"));
            let published =
                fs::copy(entry.path(), &temporary).and_then(|_| fs::rename(&temporary, &target));
            if let Err(error) = published {
                let _ = fs::remove_file(&temporary);
                return Err(error);
            }
        }
    }
    Ok(())
}

fn copy_files(from: &Path, to: &Path, budget: &mut u64) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_files(&entry.path(), &target, budget)?;
        } else if kind.is_file() && !entry.file_name().to_string_lossy().ends_with(".publishing") {
            *budget = budget.checked_sub(entry.metadata()?.len()).ok_or_else(|| {
                io::Error::new(io::ErrorKind::QuotaExceeded, "shader seed exceeds 512 MiB")
            })?;
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_are_private_copies_published_only_by_the_caller() {
        let base = std::env::temp_dir().join(format!(
            "lotus-seed-{}",
            lotus_protocol::envelope::SessionUuid::generate()
        ));
        let seeds = base.join("seeds");
        let sha = "a".repeat(64);
        let first = base.join("first");
        assert_eq!(seed(&seeds, &sha, &first).expect("cold"), "cold");
        fs::create_dir_all(first.join("shaders/vulkan/pipeline")).expect("dirs");
        fs::write(first.join("shaders/vulkan/pipeline/0-1.bin"), b"cache v1").expect("cache");
        publish(&seeds, &sha, &first).expect("publish");

        let second = base.join("second");
        assert_eq!(seed(&seeds, &sha, &second).expect("seed"), "seeded");
        let copy = second.join("shaders/vulkan/pipeline/0-1.bin");
        assert_eq!(fs::read(&copy).expect("copy"), b"cache v1");
        // The worker writes its private copy; the seed is untouched until a publish.
        fs::write(&copy, b"cache v2, longer").expect("worker write");
        let seeded = seeds.join(&sha).join("shaders/vulkan/pipeline/0-1.bin");
        assert_eq!(fs::read(&seeded).expect("seed"), b"cache v1");
        publish(&seeds, &sha, &second).expect("publish");
        assert_eq!(fs::read(&seeded).expect("seed"), b"cache v2, longer");
        // A root with caches keeps them; another binary has its own (empty) seed.
        assert_eq!(seed(&seeds, &sha, &second).expect("kept"), "kept");
        assert_eq!(
            seed(&seeds, &"b".repeat(64), &base.join("third")).expect("other"),
            "cold"
        );
        let _ = fs::remove_dir_all(base);
    }
}
