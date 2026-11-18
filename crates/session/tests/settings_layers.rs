//! lotus#51 acceptance against the mock worker: saved global and card settings, secondary
//! snapshots and runtime overrides, and what reaches each worker.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lotus_session::supervisor::{HostConfig, SessionKind, SettingsTarget, Supervisor};
use lotus_session::telemetry::Actor;
use serde_json::{json, Value};

struct Host {
    host: Arc<Supervisor>,
    store: PathBuf,
}

fn host(name: &str, configure: impl FnOnce(&mut HostConfig)) -> Host {
    let base = std::env::temp_dir().join(format!(
        "lotus-layers-{name}-{}",
        lotus_protocol::envelope::SessionUuid::generate()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let content = base.join("demo.3dsx");
    std::fs::write(&content, b"3DSX").unwrap();
    let mut config = HostConfig::new(env!("CARGO_BIN_EXE_lotus-mock-native"), base.join("store"));
    config.allow_control = true;
    config.max_sessions = 6;
    config.assets.insert("demo".into(), content);
    configure(&mut config);
    let host = Arc::new(Supervisor::new(config).unwrap());
    let store = base.join("store").canonicalize().unwrap();
    for card in ["a", "b"] {
        std::fs::create_dir_all(store.join("cards").join(card)).unwrap();
    }
    Host { host, store }
}

/// Settings the worker in `root` holds (mock `settings_list`, recorded at every apply).
fn worker_settings(root: &Path) -> Value {
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(root.join("native-manifest.json")).unwrap()).unwrap();
    manifest["settings"].clone()
}

fn card(name: &str) -> SettingsTarget {
    SettingsTarget::Card { card: name.into() }
}

fn session(id: &str) -> SettingsTarget {
    SettingsTarget::Session { session: id.into() }
}

impl Host {
    async fn set(&self, target: &SettingsTarget, key: &str, value: Option<Value>) {
        self.host
            .set_setting(target, key, value, Actor::Host)
            .await
            .unwrap_or_else(|error| panic!("{target:?} {key}: {error}"));
    }
    async fn effective(&self, target: &SettingsTarget, key: &str) -> (Value, Value) {
        let report = self.host.settings(target).await.unwrap();
        let entry = &report["settings"][key];
        (entry["value"].clone(), entry["source"].clone())
    }
    async fn create(&self, kind: SessionKind) -> String {
        self.host
            .create(kind, Some("demo"), Actor::Host)
            .await
            .unwrap()
            .id
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cards_inherit_global_and_secondaries_keep_their_snapshot() {
    let fixture = host("inherit", |_| {});
    let key = "cpu_clock_percentage";
    fixture
        .set(&SettingsTarget::Global, key, Some(json!(150)))
        .await;
    fixture.set(&card("a"), key, Some(json!(50))).await;
    assert_eq!(
        fixture.effective(&card("b"), key).await,
        (json!(150), json!("global"))
    );

    let a = fixture
        .create(SessionKind::Primary { card: "a".into() })
        .await;
    let b = fixture
        .create(SessionKind::Primary { card: "b".into() })
        .await;
    let card_root = |name: &str| fixture.store.join("cards").join(name);
    assert_eq!(worker_settings(&card_root("a"))[key], json!(50));
    assert_eq!(worker_settings(&card_root("b"))[key], json!(150));

    // A global change reaches inheriting cards for their next start, never a running worker.
    fixture
        .set(&SettingsTarget::Global, key, Some(json!(200)))
        .await;
    assert_eq!(worker_settings(&card_root("b"))[key], json!(150));
    let report = fixture.host.settings(&session(&b)).await.unwrap();
    assert_eq!(report["pending"], json!([key]));
    assert_eq!(report["settings"][key]["value"], json!(150));
    let report = fixture.host.settings(&session(&a)).await.unwrap();
    assert_eq!(report["pending"], json!([]));
    assert_eq!(report["session"]["card"], json!("a"));

    // A primary session is edited through its card.
    let error = fixture
        .host
        .set_setting(&session(&a), "volume", Some(json!(0.5)), Actor::Host)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);

    // A secondary cloned from card a starts with a's resolution.
    let clone = fixture
        .create(SessionKind::Secondary {
            parent: Some("a".into()),
        })
        .await;
    let clone_root = fixture.store.join("scratch").join(&clone);
    assert_eq!(worker_settings(&clone_root)[key], json!(50));
    assert!(!clone_root.join("lotus-settings.json").exists());
    // Runtime overrides reach its worker only.
    fixture
        .set(&session(&clone), "volume", Some(json!(0.25)))
        .await;
    assert_eq!(worker_settings(&clone_root)["volume"], json!(0.25));
    assert_eq!(
        fixture.effective(&session(&clone), "volume").await,
        (json!(0.25), json!("runtime"))
    );
    assert_eq!(
        fixture.effective(&card("a"), "volume").await.1,
        json!("default")
    );
    assert_eq!(
        fixture.effective(&SettingsTarget::Global, "volume").await.1,
        json!("default")
    );
    // Later saved changes do not reach the secondary's snapshot.
    fixture.set(&card("a"), key, Some(json!(75))).await;
    assert_eq!(
        fixture.effective(&session(&clone), key).await,
        (json!(50), json!("card"))
    );
    // Removing the runtime override re-applies the creation value.
    fixture.set(&session(&clone), "volume", None).await;
    let default = lotus_settings::Registry::builtin()
        .get("volume")
        .unwrap()
        .default
        .clone();
    assert_eq!(worker_settings(&clone_root)["volume"], default);
    fixture
        .set(&session(&clone), "volume", Some(json!(0.75)))
        .await;
    fixture
        .host
        .reset_settings(&session(&clone), Actor::Host)
        .await
        .unwrap();
    assert_eq!(worker_settings(&clone_root)["volume"], default);
    fixture.host.stop(&clone, Actor::Host).await.unwrap();
    assert!(!clone_root.exists(), "nothing of the secondary persists");

    // A parentless secondary inherits global defaults only.
    let plain = fixture
        .create(SessionKind::Secondary { parent: None })
        .await;
    assert_eq!(
        worker_settings(&fixture.store.join("scratch").join(&plain))[key],
        json!(200)
    );

    // Removing a card override resumes inheritance, also for a new supervisor.
    fixture.set(&card("a"), key, None).await;
    for name in [a, b, plain] {
        fixture.host.stop(&name, Actor::Host).await.unwrap();
    }
    fixture.host.close().await;
    let restarted = Supervisor::new({
        let mut config = HostConfig::new(
            env!("CARGO_BIN_EXE_lotus-mock-native"),
            fixture.store.clone(),
        );
        config.allow_control = true;
        config
    })
    .unwrap();
    let report = restarted.settings(&card("a")).await.unwrap();
    assert_eq!(report["settings"][key]["value"], json!(200));
    assert_eq!(report["settings"][key]["source"], json!("global"));
    restarted
        .reset_settings(&SettingsTarget::Global, Actor::Host)
        .await
        .unwrap();
    let report = restarted.settings(&card("b")).await.unwrap();
    assert_eq!(report["settings"][key]["source"], json!("default"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_profile_is_managed_and_unknown_cards_are_refused() {
    let fixture = host("managed", |config| {
        config.profile.insert(
            "graphics_api".into(),
            lotus_protocol::debug::SettingValue::Integer(0),
        );
    });
    let report = fixture
        .host
        .settings(&SettingsTarget::Global)
        .await
        .unwrap();
    assert_eq!(report["managed"], json!(["graphics_api"]));
    assert_eq!(
        report["settings"]["graphics_api"]["source"],
        json!("managed")
    );
    for target in [SettingsTarget::Global, card("a")] {
        let error = fixture
            .host
            .set_setting(&target, "graphics_api", Some(json!(2)), Actor::Host)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }
    let secondary = fixture
        .create(SessionKind::Secondary { parent: None })
        .await;
    let error = fixture
        .host
        .set_setting(
            &session(&secondary),
            "graphics_api",
            Some(json!(2)),
            Actor::Host,
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    let error = fixture.host.settings(&card("nope")).await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    fixture.host.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_only_hosts_cannot_change_settings() {
    let fixture = host("readonly", |config| config.allow_control = false);
    fixture
        .host
        .settings(&SettingsTarget::Global)
        .await
        .expect("reads are allowed");
    let error = fixture
        .host
        .set_setting(
            &SettingsTarget::Global,
            "volume",
            Some(json!(0.5)),
            Actor::Host,
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
}
