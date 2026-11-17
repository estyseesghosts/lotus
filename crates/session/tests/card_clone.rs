//! Secondary copies of a card with a live primary (#28), against `lotus-mock-native`, whose
//! "guest" rewrites `sdmc/a.bin` then `sdmc/b.bin` with one counter while running. Every
//! real instant has `a == b` or `a == b + 1`; a copy taken while the guest runs can read
//! `a` before an update and `b` after it, a state that never existed.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lotus_protocol::envelope::SessionUuid;
use lotus_session::supervisor::{HostConfig, SessionKind, Supervisor};
use lotus_session::telemetry::Actor;

struct Host {
    base: PathBuf,
    host: Arc<Supervisor>,
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn host(name: &str) -> Host {
    let base = std::env::temp_dir().join(format!("lotus-clone-{name}-{}", SessionUuid::generate()));
    std::fs::create_dir_all(&base).expect("base");
    let content = base.join("demo.3dsx");
    std::fs::write(&content, b"3DSX").expect("content");
    let mut config = HostConfig::new(env!("CARGO_BIN_EXE_lotus-mock-native"), base.join("store"));
    config.allow_control = true;
    config.max_sessions = 4;
    config.assets.insert("demo".into(), content);
    Host {
        host: Arc::new(Supervisor::new(config).expect("host")),
        base,
    }
}

impl Host {
    fn store(&self) -> PathBuf {
        self.base.join("store").canonicalize().expect("store")
    }

    async fn primary(&self, card: &str) -> String {
        let root = self.store().join("cards").join(card);
        std::fs::create_dir_all(&root).expect("card");
        std::fs::write(root.join("mock-guest"), "").expect("guest writer");
        let info = self
            .host
            .create(
                SessionKind::Primary { card: card.into() },
                Some("demo"),
                Actor::Host,
            )
            .await
            .expect("primary");
        self.host.run(&info.id, Actor::Host).await.expect("run");
        // Let the guest write its first pair.
        tokio::time::sleep(Duration::from_millis(100)).await;
        info.id
    }

    async fn clone(&self, card: &str) -> std::io::Result<String> {
        self.host
            .create(
                SessionKind::Secondary {
                    parent: Some(card.into()),
                },
                None,
                Actor::Host,
            )
            .await
            .map(|info| info.id)
    }
}

fn pair(sdmc: &Path) -> (u64, u64) {
    let read = |name: &str| {
        u64::from_le_bytes(
            std::fs::read(sdmc.join(name))
                .expect("file")
                .try_into()
                .expect("8 bytes"),
        )
    };
    (read("a.bin"), read("b.bin"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn copies_of_a_running_primary_are_instants_and_it_keeps_running() {
    let fixture = host("running");
    let primary = fixture.primary("dev").await;
    let card = fixture.store().join("cards/dev/sdmc");
    // Without coordination (the previous create_scratch), copies tear: a pair that never
    // existed, or a file caught mid-rewrite (empty or locked).
    let mut torn = 0;
    for _ in 0..50 {
        let read = |name: &str| {
            std::fs::read(card.join(name))
                .ok()
                .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
                .map(u64::from_le_bytes)
        };
        torn += usize::from(match (read("a.bin"), read("b.bin")) {
            (Some(a), Some(b)) => a != b && a != b + 1,
            _ => true,
        });
    }
    assert!(
        torn > 0,
        "uncoordinated reads of a running guest never tore"
    );
    for _ in 0..20 {
        let clone = fixture.clone("dev").await.expect("clone");
        let (a, b) = pair(&fixture.store().join("scratch").join(&clone).join("sdmc"));
        assert!(a == b || a == b + 1, "copy is no real instant: a={a} b={b}");
        fixture
            .host
            .stop(&clone, Actor::Host)
            .await
            .expect("stop clone");
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let summary = fixture.host.summary(&primary).expect("primary");
    assert!(
        summary.telemetry.playing,
        "primary resumed after every copy"
    );
    assert!(!fixture
        .store()
        .join("scratch")
        .read_dir()
        .expect("scratch")
        .any(|e| {
            e.expect("entry")
                .path()
                .extension()
                .is_some_and(|x| x == "partial")
        }));
    fixture.host.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pending_guest_fs_work_advances_a_running_primary_but_never_a_paused_one() {
    let fixture = host("pending");
    let primary = fixture.primary("dev").await;
    let root = fixture.store().join("cards/dev");
    std::fs::write(root.join("mock-async-pending"), "3").expect("pending");
    let before = fixture.host.status(&primary).await.expect("status")["frames"]
        .as_u64()
        .expect("frames");
    let clone = fixture
        .clone("dev")
        .await
        .expect("settles after three frames");
    fixture.host.stop(&clone, Actor::Host).await.expect("stop");
    assert_eq!(
        std::fs::read_to_string(root.join("mock-async-pending")).expect("left"),
        "0"
    );
    let after = fixture.host.status(&primary).await.expect("status")["frames"]
        .as_u64()
        .expect("frames");
    assert!(after >= before + 3);

    // The user paused here: the guest is not advanced and the copy is refused.
    fixture
        .host
        .pause(&primary, Actor::Host)
        .await
        .expect("pause");
    let paused = fixture.host.status(&primary).await.expect("status")["frames"].clone();
    std::fs::write(root.join("mock-async-pending"), "1").expect("pending");
    let error = fixture
        .clone("dev")
        .await
        .expect_err("refused while paused");
    assert_eq!(error.kind(), std::io::ErrorKind::ResourceBusy);
    assert!(
        error.to_string().contains("paused mid-operation"),
        "{error}"
    );
    assert_eq!(
        fixture.host.status(&primary).await.expect("status")["frames"],
        paused
    );
    assert_eq!(
        fixture.host.sessions().len(),
        1,
        "no secondary was registered"
    );
    // Once settled, the paused copy succeeds and is exactly the paused pair.
    let clone = fixture.clone("dev").await.expect("clone");
    assert_eq!(
        pair(&fixture.store().join("scratch").join(&clone).join("sdmc")),
        pair(&root.join("sdmc"))
    );
    fixture.host.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn uncoordinatable_cards_are_refused_precisely() {
    let fixture = host("refuse");
    let primary = fixture.primary("old").await;
    fixture
        .host
        .pause(&primary, Actor::Host)
        .await
        .expect("pause");
    std::fs::write(fixture.store().join("cards/old/mock-no-flush"), "").expect("old worker");
    let error = fixture.clone("old").await.expect_err("old worker");
    assert!(error.to_string().contains("cannot flush"), "{error}");

    // A card whose worker lock is held by another process is not copied.
    let other = fixture.store().join("cards/other");
    std::fs::create_dir_all(other.join("sdmc")).expect("card");
    let lock = std::fs::File::create(other.join("worker.lock")).expect("lock");
    lock.try_lock().expect("held");
    let error = fixture.clone("other").await.expect_err("foreign card");
    assert!(
        error.to_string().contains("another LOTUS process"),
        "{error}"
    );
    drop(lock);
    let clone = fixture.clone("other").await.expect("released card copies");
    fixture.host.stop(&clone, Actor::Host).await.expect("stop");
    fixture.host.close().await;
}
