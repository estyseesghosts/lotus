//! Counters for the window side of the desktop (lotus#74), lock-free so that reading them
//! never depends on the GPUI refresh task, the window's render or the shared `Update` lock.
//!
//! The controller's tick publishes [`UiMetrics::snapshot`] as `frontend_metrics.ui` in
//! `lotus_status`, next to its own `ticks` and preview counters. Together they separate the
//! stages a frame passes: worker frames → controller preview task (`preview.count`,
//! `published_preview_sequence`) → window poll (`polls_*`) → installed images
//! (`installed_preview_sequence`) → render (`renders`). Ages are measured at snapshot time.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Instant;

#[derive(Clone, Copy, serde::Serialize)]
pub(crate) struct PreviewIdentity {
    pub epoch: u64,
    pub sequence: u64,
    pub worker_sequence: u64,
}

#[derive(Clone, serde::Serialize)]
pub(crate) struct RenderedTile {
    pub session: String,
    pub summary_epoch: u64,
    pub guest_frames: u64,
    pub preview: Option<PreviewIdentity>,
}

#[derive(Clone, serde::Serialize)]
struct RenderedWorkspace {
    revision: u64,
    tiles: Vec<RenderedTile>,
}

pub struct UiMetrics {
    base: Instant,
    polls_started: AtomicU64,
    polls_completed: AtomicU64,
    /// Polls that found the shared `Update` lock poisoned and recovered it.
    polls_recovered: AtomicU64,
    last_poll_us: AtomicU64,
    previews_installed: AtomicU64,
    installed_preview_sequence: AtomicU64,
    renders: AtomicU64,
    last_render_us: AtomicU64,
    rendered_workspace: Mutex<Option<RenderedWorkspace>>,
    activations: AtomicU64,
    deactivations: AtomicU64,
    last_activation_us: AtomicU64,
    active: AtomicBool,
}

impl Default for UiMetrics {
    fn default() -> Self {
        Self {
            base: Instant::now(),
            polls_started: AtomicU64::new(0),
            polls_completed: AtomicU64::new(0),
            polls_recovered: AtomicU64::new(0),
            last_poll_us: AtomicU64::new(0),
            previews_installed: AtomicU64::new(0),
            installed_preview_sequence: AtomicU64::new(0),
            renders: AtomicU64::new(0),
            last_render_us: AtomicU64::new(0),
            rendered_workspace: Mutex::default(),
            activations: AtomicU64::new(0),
            deactivations: AtomicU64::new(0),
            last_activation_us: AtomicU64::new(0),
            active: AtomicBool::new(false),
        }
    }
}

impl UiMetrics {
    fn now_us(&self) -> u64 {
        u64::try_from(self.base.elapsed().as_micros()).unwrap_or(u64::MAX)
    }

    fn age_ms(&self, at_us: &AtomicU64) -> Option<f64> {
        let at = at_us.load(Ordering::Relaxed);
        (at != 0).then(|| self.now_us().saturating_sub(at) as f64 / 1e3)
    }

    pub fn poll_started(&self) {
        self.polls_started.fetch_add(1, Ordering::Relaxed);
        self.last_poll_us
            .store(self.now_us().max(1), Ordering::Relaxed);
    }

    pub fn poll_completed(&self) {
        self.polls_completed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn preview_installed(&self, sequence: u64) {
        self.previews_installed.fetch_add(1, Ordering::Relaxed);
        self.installed_preview_sequence
            .fetch_max(sequence, Ordering::Relaxed);
    }

    pub(crate) fn rendered(&self, revision: u64, tiles: Vec<RenderedTile>) {
        self.renders.fetch_add(1, Ordering::Relaxed);
        self.last_render_us
            .store(self.now_us().max(1), Ordering::Relaxed);
        if let Ok(mut workspace) = self.rendered_workspace.try_lock() {
            *workspace = Some(RenderedWorkspace { revision, tiles });
        }
    }

    pub fn activation(&self, active: bool) {
        let counter = if active {
            &self.activations
        } else {
            &self.deactivations
        };
        counter.fetch_add(1, Ordering::Relaxed);
        self.active.store(active, Ordering::Relaxed);
        self.last_activation_us
            .store(self.now_us().max(1), Ordering::Relaxed);
    }

    /// Lock the shared state for a poll. A panic elsewhere while holding it poisons the
    /// mutex. The window recovers it like the controller does
    /// (`lock` in desktop_controller.rs), so subsequent publications can still be read.
    /// Each recovery is counted and logged.
    pub fn lock_update<'a, T>(&self, latest: &'a Mutex<T>) -> MutexGuard<'a, T> {
        latest.lock().unwrap_or_else(|poisoned: PoisonError<_>| {
            let count = self.polls_recovered.fetch_add(1, Ordering::Relaxed) + 1;
            eprintln!("lotus-desktop: shared window state was poisoned; recovered ({count})");
            latest.clear_poison();
            poisoned.into_inner()
        })
    }

    #[must_use]
    pub fn snapshot(&self) -> serde_json::Value {
        let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        let rendered_workspace = self
            .rendered_workspace
            .try_lock()
            .ok()
            .and_then(|workspace| workspace.clone());
        serde_json::json!({
            "polls_started": load(&self.polls_started),
            "polls_completed": load(&self.polls_completed),
            "polls_recovered": load(&self.polls_recovered),
            "poll_age_ms": self.age_ms(&self.last_poll_us),
            "previews_installed": load(&self.previews_installed),
            "installed_preview_sequence": load(&self.installed_preview_sequence),
            "renders": load(&self.renders),
            "render_age_ms": self.age_ms(&self.last_render_us),
            "rendered_workspace": rendered_workspace,
            "activations": load(&self.activations),
            "deactivations": load(&self.deactivations),
            "activation_age_ms": self.age_ms(&self.last_activation_us),
            "active": self.active.load(Ordering::Relaxed),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn a_poisoned_update_is_recovered_counted_and_cleared() {
        let metrics = UiMetrics::default();
        let latest = Arc::new(Mutex::new(1u32));
        let poisoner = latest.clone();
        let _ = std::thread::spawn(move || {
            let mut guard = poisoner.lock().expect("lock");
            *guard = 2;
            panic!("poison the shared state");
        })
        .join();
        assert!(latest.is_poisoned());
        // Every following poll still reads and writes the state.
        for value in 3..6 {
            let mut guard = metrics.lock_update(&latest);
            assert_eq!(*guard, value - 1);
            *guard = value;
        }
        assert!(!latest.is_poisoned());
        assert_eq!(metrics.snapshot()["polls_recovered"], 1);
    }

    #[test]
    fn ages_are_absent_until_the_first_event() {
        let metrics = UiMetrics::default();
        assert!(metrics.snapshot()["poll_age_ms"].is_null());
        metrics.poll_started();
        metrics.rendered(0, Vec::new());
        metrics.activation(true);
        let snapshot = metrics.snapshot();
        assert!(snapshot["poll_age_ms"].as_f64().is_some());
        assert_eq!(snapshot["activations"], 1);
        assert_eq!(snapshot["active"], true);
    }

    #[test]
    fn installing_previews_in_tile_order_keeps_the_latest_global_sequence() {
        let metrics = UiMetrics::default();
        metrics.preview_installed(10);
        metrics.preview_installed(9);
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot["previews_installed"], 2);
        assert_eq!(snapshot["installed_preview_sequence"], 10);
    }
}
