use std::collections::BTreeMap;
use std::path::PathBuf;

use lotus_mcp::tool_arguments::{
    AdvanceArgs, ControlArgs, ControlOperation, CreateArgs, KindArg, SessionArgs,
};
use lotus_mcp::LotusServer;
use lotus_protocol::envelope::SessionUuid;
use lotus_session::native_session::NativeSession;
use lotus_session::supervisor::HostConfig;
use rmcp::handler::server::wrapper::Parameters;

fn create(asset: &str) -> CreateArgs {
    CreateArgs {
        kind: KindArg::Secondary,
        card: None,
        asset: Some(asset.into()),
    }
}

#[tokio::test]
#[ignore = "requires external native worker and homebrew fixture"]
async fn mcp_matches_native_api_and_scopes_assets_sessions_resources() {
    let worker = PathBuf::from(std::env::var_os("LOTUS_NATIVE_WORKER").expect("native executable"));
    let fixture = PathBuf::from(std::env::var_os("LOTUS_FIXTURE").expect("homebrew fixture"));
    let root = std::env::temp_dir().join(format!("lotus-mcp-{}", SessionUuid::generate()));
    let mut config = HostConfig::new(worker.clone(), root.join("mcp"));
    config.assets = BTreeMap::from([("fixture".into(), fixture.clone())]);
    config.allow_control = true;
    let mut read_only = config.clone();
    read_only.allow_control = false;
    let denied = LotusServer::configured(read_only, BTreeMap::new()).expect("read-only server");
    assert_eq!(
        denied
            .lotus_create_session(Parameters(create("fixture")))
            .await
            .is_error,
        Some(true)
    );
    let server = LotusServer::configured(config, BTreeMap::new()).expect("configured server");
    assert_eq!(
        server
            .lotus_create_session(Parameters(create(&fixture.display().to_string())))
            .await
            .is_error,
        Some(true)
    );
    let created = server
        .lotus_create_session(Parameters(create("fixture")))
        .await;
    assert_ne!(
        created.is_error,
        Some(true),
        "create failed: {:?}",
        created.content
    );
    let data: serde_json::Value =
        serde_json::from_str(&created.content[0].as_text().expect("tool JSON").text)
            .expect("session result");
    let session = data["session"].as_str().expect("owned session").to_string();
    assert_eq!(
        server
            .lotus_session_status(Parameters(SessionArgs {
                session: "unowned".into()
            }))
            .await
            .is_error,
        Some(true)
    );
    assert_ne!(
        server
            .lotus_run_until(Parameters(AdvanceArgs {
                session: session.clone(),
                frames: 60
            }))
            .await
            .is_error,
        Some(true)
    );
    let capture = server
        .lotus_capture(Parameters(SessionArgs {
            session: session.clone(),
        }))
        .await;
    assert_eq!(capture.content.len(), 2);
    let mut reference = NativeSession::spawn(&worker, &root.join("reference"))
        .await
        .expect("shared native API");
    reference.load(&fixture).await.expect("same content");
    reference.advance(60).await.expect("same checkpoint");
    let frames = reference.capture().await.expect("reference pixels");
    for (index, link) in capture.content.iter().enumerate() {
        let resource = link
            .as_resource_link()
            .expect("artifact resource, no inline image");
        let bytes = server
            .resource_bytes(&resource.uri)
            .await
            .expect("scoped resource");
        assert_eq!(bytes, frames[index].bgra);
    }
    assert!(server
        .resource_bytes("lotus://artifact/../../worker.stderr.log")
        .await
        .is_err());
    assert_ne!(
        server
            .lotus_control(Parameters(ControlArgs {
                session,
                operation: ControlOperation::Stop
            }))
            .await
            .is_error,
        Some(true)
    );
    reference.stop().await.expect("reference shutdown");
    std::fs::remove_dir_all(root).expect("closed process roots");
}
