//! Durable investigation identity, revision conflicts and restart behavior (lotus#97).
use lotus_protocol::debug::DebugCommand;
use lotus_protocol::envelope::SessionUuid;
use lotus_session::investigation::{
    ArtifactReference, ArtifactState, InvestigationDetails, ReferenceState, SessionReference,
    SourceLocation,
};
use lotus_session::supervisor::{HostConfig, SessionKind, Supervisor};
use lotus_session::telemetry::Actor;
use std::io::ErrorKind;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn references_follow_worker_identity_and_metadata_survives_restart() {
    let root =
        std::env::temp_dir().join(format!("lotus-investigation-{}", SessionUuid::generate()));
    std::fs::create_dir_all(&root).unwrap();
    let content = root.join("fixture.3dsx");
    std::fs::write(&content, b"3DSX").unwrap();
    let mut config = HostConfig::new(env!("CARGO_BIN_EXE_lotus-mock-native"), root.join("store"));
    config.allow_control = true;
    config.assets.insert("fixture".into(), content);
    let host = Arc::new(Supervisor::new(config.clone()).unwrap());
    let session = host
        .create(
            SessionKind::Secondary { parent: None },
            Some("fixture"),
            Actor::Desktop,
        )
        .await
        .unwrap();
    let sibling = host
        .create(
            SessionKind::Secondary { parent: None },
            None,
            Actor::Desktop,
        )
        .await
        .unwrap();
    let initial = host.summary(&session.id).unwrap().telemetry;
    let target = SessionReference {
        session: session.id.clone(),
        created_unix_ms: session.created_unix_ms,
        epoch: initial.epoch,
    };
    let details = InvestigationDetails {
        title: "Track frame divergence".into(),
        purpose: "Compare the same frame from two profiles".into(),
        sessions: vec![target],
        sources: vec![SourceLocation {
            file: "src/main.c".into(),
            line: 12,
        }],
        artifacts: vec![ArtifactReference {
            uri: format!("lotus://artifact/{}", SessionUuid::generate()),
            sha256: "f".repeat(64),
        }],
        ..InvestigationDetails::default()
    };
    let record = host.create_investigation(details.clone()).unwrap();
    let another = host.create_investigation(details.clone()).unwrap();
    for id in [&record.id, &another.id] {
        let view = host.investigation(id).unwrap();
        assert_eq!(view.session_states, [ReferenceState::Live]);
        assert_eq!(view.artifact_states, [ArtifactState::Missing]);
    }
    let after_reads = host.summary(&session.id).unwrap().telemetry;
    assert_eq!(after_reads.input_owner, initial.input_owner);
    assert_eq!(
        after_reads.desktop_debug_lease_blocked,
        initial.desktop_debug_lease_blocked
    );
    assert_eq!(after_reads.pid, initial.pid);

    // Each writer opens its own catalog lock handle. Only one can publish revision 2.
    let contender = |next_steps: &str| {
        let host = host.clone();
        let id = record.id.clone();
        let revision = record.revision;
        let mut details = details.clone();
        details.next_steps = next_steps.into();
        tokio::task::spawn_blocking(move || host.update_investigation(&id, revision, details))
    };
    let (a, b) = tokio::join!(
        contender("compare top screen"),
        contender("compare lower screen")
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    let saved = match (a, b) {
        (Ok(saved), Err(conflict)) | (Err(conflict), Ok(saved)) => {
            assert_eq!(conflict.kind(), ErrorKind::WouldBlock);
            assert!(conflict.to_string().contains("current revision 2"));
            saved
        }
        other => panic!("expected one successful edit and one conflict: {other:?}"),
    };
    assert_eq!(host.investigation(&saved.id).unwrap().record, saved);

    host.debug(&session.id, &DebugCommand::Reset, Actor::Desktop)
        .await
        .unwrap();
    assert_eq!(
        host.investigation(&saved.id).unwrap().session_states,
        [ReferenceState::EpochChanged]
    );
    assert!(host
        .summary(&sibling.id)
        .unwrap()
        .telemetry
        .state
        .is_active());
    assert!(host
        .close()
        .await
        .into_iter()
        .all(|(_, result)| result.is_ok()));
    drop(host);

    config.allow_control = false;
    let reopened = Supervisor::new(config).unwrap();
    assert!(reopened.sessions().is_empty());
    let view = reopened.investigation(&saved.id).unwrap();
    assert_eq!(view.record, saved);
    assert_eq!(view.session_states, [ReferenceState::Expired]);
    assert!(!view.writable);
    assert_eq!(
        reopened.create_investigation(details).unwrap_err().kind(),
        ErrorKind::PermissionDenied
    );
    assert_eq!(reopened.investigations().unwrap().len(), 2);
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn unknown_catalog_version_survives_attempted_write() {
    let root = std::env::temp_dir().join(format!(
        "lotus-investigation-version-{}",
        SessionUuid::generate()
    ));
    let mut config = HostConfig::new(env!("CARGO_BIN_EXE_lotus-mock-native"), &root);
    config.allow_control = true;
    let host = Supervisor::new(config).unwrap();
    let details = InvestigationDetails {
        title: "Replay".into(),
        ..InvestigationDetails::default()
    };
    host.create_investigation(details.clone()).unwrap();
    let path = root.join("investigations/catalog.json");
    let newer = br#"{"version":2,"schema_change":["preserve me"]}"#;
    std::fs::write(&path, newer).unwrap();
    assert!(host.create_investigation(details).is_err());
    assert_eq!(std::fs::read(path).unwrap(), newer);
    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}
