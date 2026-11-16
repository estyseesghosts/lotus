//! Mock worker for process/transport acceptance; this never emulates a console.
use std::io;

use lotus_protocol::control::{self, Method, Reply, Request, Status};

fn main() -> io::Result<()> {
    let mode = std::env::args().nth(1).unwrap_or_default();
    if mode == "descendant" {
        std::thread::sleep(std::time::Duration::from_secs(60));
        return Ok(());
    }
    if mode == "spawn_descendant" {
        let mut command = std::process::Command::new(std::env::current_exe()?);
        command.arg("descendant");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let descendant = command.spawn()?;
        let root = std::env::var_os("LOTUS_STORAGE_ROOT").expect("supervisor supplies root");
        std::fs::write(
            std::path::Path::new(&root).join("descendant.pid"),
            descendant.id().to_string(),
        )?;
    }
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let mut identity: Option<String> = None;
    loop {
        let request: Request = control::read(&mut input)?;
        if mode == "crash" || (mode == "crash_status" && request.method == Method::Status) {
            std::process::exit(23);
        }
        if mode == "hang" {
            std::thread::sleep(std::time::Duration::from_secs(60));
            continue;
        }
        let status = if !lotus_protocol::is_compatible(request.version_major, request.version_minor)
        {
            Status::Unsupported
        } else if request.session.len() != 32
            || !request.session.bytes().all(|c| c.is_ascii_hexdigit())
            || request.request_id == 0
        {
            Status::InvalidArgument
        } else if identity.as_ref().is_some_and(|id| id != &request.session) {
            Status::PermissionDenied
        } else if request.method != Method::Hello && identity.is_none() {
            Status::NotReady
        } else if request.epoch != 1 {
            Status::StaleEpoch
        } else if !matches!(
            request.method,
            Method::Hello | Method::Status | Method::Stop
        ) {
            Status::Unsupported
        } else {
            if request.method == Method::Hello {
                identity = Some(request.session.clone());
            }
            Status::Ok
        };
        let stopped = status == Status::Ok && request.method == Method::Stop;
        let reply = Reply {
            version_major: lotus_protocol::PROTOCOL_VERSION_MAJOR,
            version_minor: lotus_protocol::PROTOCOL_VERSION_MINOR,
            request_id: request.request_id,
            session: request.session,
            epoch: if mode == "stale" { 0 } else { 1 },
            status,
            state: if stopped { "stopped" } else { "idle" }.into(),
            backend: "mock".into(),
            data: serde_json::Value::Null,
        };
        control::write(&mut output, &reply)?;
        if stopped {
            return Ok(());
        }
    }
}
