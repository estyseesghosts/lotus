//! lotus#51 acceptance against the mock worker: saved global and card settings, secondary
//! snapshots and runtime overrides, and what reaches each worker.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lotus_session::supervisor::{
    HostConfig, SessionKind, SettingStatus, SettingsTarget, Supervisor,
};
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

    // A global live change reaches only the mounted card that inherits the value.
    fixture
        .set(&SettingsTarget::Global, key, Some(json!(200)))
        .await;
    assert_eq!(worker_settings(&card_root("b"))[key], json!(200));
    assert_eq!(worker_settings(&card_root("a"))[key], json!(50));
    let report = fixture.host.settings(&session(&b)).await.unwrap();
    assert_eq!(report["pending"], json!([]));
    assert_eq!(report["settings"][key]["value"], json!(200));
    let report = fixture.host.settings(&session(&a)).await.unwrap();
    assert_eq!(report["pending"], json!([]));
    assert_eq!(report["session"]["card"], json!("a"));

    let change = fixture
        .host
        .set_setting(&card("b"), "graphics_api", Some(json!(0)), Actor::Host)
        .await
        .unwrap();
    assert_eq!(change[0].status, SettingStatus::Pending);
    let report = fixture.host.settings(&card("b")).await.unwrap();
    assert_eq!(report["settings"]["graphics_api"]["value"], json!(0));
    assert_eq!(
        report["effective_settings"]["graphics_api"]["value"],
        json!(2)
    );
    assert_eq!(report["pending"], json!(["graphics_api"]));

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
async fn rejected_live_update_keeps_saved_value_and_old_effective_value() {
    let fixture = host("rejected", |_| {});
    let target = card("a");
    fixture.set(&target, "volume", Some(json!(0.25))).await;
    let id = fixture
        .create(SessionKind::Primary { card: "a".into() })
        .await;
    let refusal = fixture.store.join("cards/a/mock-reject-debug");
    std::fs::write(&refusal, "").unwrap();
    let changes = fixture
        .host
        .set_setting(&target, "volume", Some(json!(0.75)), Actor::Host)
        .await
        .unwrap();
    assert_eq!(changes[0].status, SettingStatus::Rejected);
    assert!(changes[0].reason.as_ref().unwrap().contains("Unsupported"));
    let report = fixture.host.settings(&target).await.unwrap();
    assert_eq!(report["settings"]["volume"]["value"], json!(0.75));
    assert_eq!(report["effective_settings"]["volume"]["value"], json!(0.25));
    assert_eq!(report["pending"], json!(["volume"]));
    fixture.host.stop(&id, Actor::Host).await.unwrap();
    std::fs::remove_file(refusal).unwrap();
    let replacement = fixture
        .create(SessionKind::Primary { card: "a".into() })
        .await;
    let report = fixture.host.settings(&target).await.unwrap();
    assert_eq!(report["effective_settings"]["volume"]["value"], json!(0.75));
    assert_eq!(report["pending"], json!([]));
    fixture.host.stop(&replacement, Actor::Host).await.unwrap();
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

/// lotus#63: input layouts use the same layers, resolve live for running sessions and never
/// reach a worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn input_layouts_layer_live_and_stay_out_of_workers() {
    use lotus_input::router::bits;
    let fixture = host("input", |_| {});
    let key = |k: &str| json!([{"device": "keyboard", "key": k}]);
    let bits_of = |id: &str, k: &str| fixture.host.input_layout(id).unwrap().0.key_bits(k);

    let a = fixture
        .create(SessionKind::Primary { card: "a".into() })
        .await;
    let b = fixture
        .create(SessionKind::Primary { card: "b".into() })
        .await;
    assert_eq!(bits_of(&a, "z"), bits::A, "builtin default");

    // A global edit reaches running, non-overriding primaries at once.
    let revision = fixture.host.settings_revision();
    fixture
        .set(
            &SettingsTarget::Global,
            "lotus.input.bind.a",
            Some(key("j")),
        )
        .await;
    assert!(fixture.host.settings_revision() > revision);
    fixture
        .set(&card("b"), "lotus.input.bind.a", Some(key("m")))
        .await;
    assert_eq!(bits_of(&a, "j"), bits::A);
    assert_eq!(bits_of(&b, "m"), bits::A, "card difference");
    assert_eq!(bits_of(&b, "j"), 0);
    // A later global edit keeps b's explicit difference.
    fixture
        .set(
            &SettingsTarget::Global,
            "lotus.input.bind.a",
            Some(key("k")),
        )
        .await;
    assert_eq!((bits_of(&a, "k"), bits_of(&b, "m")), (bits::A, bits::A));
    // Input keys are never pending: they are already in effect.
    let report = fixture.host.settings(&session(&b)).await.unwrap();
    assert_eq!(report["pending"], json!([]));

    // A secondary's runtime override: its layout only, not its worker, not card a.
    let clone = fixture
        .create(SessionKind::Secondary {
            parent: Some("a".into()),
        })
        .await;
    let clone_root = fixture.store.join("scratch").join(&clone);
    fixture
        .set(&session(&clone), "lotus.input.bind.b", Some(key("n")))
        .await;
    fixture
        .set(
            &session(&clone),
            "lotus.input.c_stick.invert_y",
            Some(json!(true)),
        )
        .await;
    assert_eq!(bits_of(&clone, "n"), bits::B);
    assert!(fixture.host.input_layout(&clone).unwrap().1.sticks[1].invert_y);
    assert_eq!(
        bits_of(&clone, "k"),
        bits::A,
        "inherits global through card a"
    );
    assert_eq!(bits_of(&a, "n"), 0);
    let worker = worker_settings(&clone_root);
    assert!(
        worker
            .as_object()
            .unwrap()
            .keys()
            .all(|k| !k.starts_with("lotus.")),
        "{worker}"
    );
    assert_eq!(
        fixture.effective(&card("a"), "lotus.input.bind.b").await.1,
        json!("default")
    );
    fixture.host.stop(&clone, Actor::Host).await.unwrap();
    let again = fixture
        .create(SessionKind::Secondary {
            parent: Some("a".into()),
        })
        .await;
    assert_eq!(
        bits_of(&again, "n"),
        0,
        "the override vanished with its secondary"
    );
    assert_eq!(bits_of(&again, "x"), bits::B);

    // Selecting a saved preset, then deleting it, falls back to default with a problem.
    fixture
        .host
        .change_input_preset(lotus_session::settings_store::PresetChange::Duplicate {
            from: "xbox-labels".into(),
            to: "mine".into(),
        })
        .unwrap();
    fixture
        .set(&card("a"), "lotus.input.preset", Some(json!("mine")))
        .await;
    assert_eq!(fixture.host.input_layout(&a).unwrap().0.preset, "mine");
    fixture
        .host
        .change_input_preset(lotus_session::settings_store::PresetChange::Delete {
            name: "mine".into(),
        })
        .unwrap();
    let (resolved, _) = fixture.host.input_layout(&a).unwrap();
    assert_eq!(resolved.preset, "default");
    assert_eq!(resolved.problems.len(), 1);

    for id in [a, b, again] {
        fixture.host.stop(&id, Actor::Host).await.unwrap();
    }
    fixture.host.close().await;
}
