//! Inspector/controller acceptance; no GUI input or screenshot access required.
use lotus_desktop::{
    desktop_controller::{
        Command, DebugStatus, DesktopController, GdbAction, GdbFeedback, McpMode, Update,
    },
    inspection::{InspectionSelection, Panel, SessionTarget},
};
use lotus_mcp::tool_arguments::{DebugArgs, GdbArgs, GdbOperation};
use lotus_protocol::{
    debug::{DebugCommand as D, GpuCapture},
    envelope::SessionUuid,
};
use lotus_session::supervisor::{HostConfig, SessionKind};
use rmcp::{handler::server::wrapper::Parameters, model::CallToolResult};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::{ErrorKind, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

fn wait(controller: &DesktopController, predicate: impl Fn(&Update) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        {
            let value = controller.latest.lock().unwrap();
            if predicate(&value) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "inspector deadline: {}",
                value.error
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn gdb_packet(socket: &mut TcpStream, request: &str) -> Vec<u8> {
    let checksum = request.bytes().fold(0, u8::wrapping_add);
    write!(socket, "${request}#{checksum:02x}").unwrap();
    let mut prefix = [0; 2];
    socket.read_exact(&mut prefix).unwrap();
    assert_eq!(&prefix, b"+$");
    let mut reply = Vec::new();
    loop {
        let mut byte = [0];
        socket.read_exact(&mut byte).unwrap();
        if byte[0] == b'#' {
            break;
        }
        assert!(reply.len() < 512, "unexpected RSP reply size");
        reply.push(byte[0]);
    }
    let mut checksum = [0; 2];
    socket.read_exact(&mut checksum).unwrap();
    let sum = reply.iter().copied().fold(0, u8::wrapping_add);
    assert_eq!(&checksum, format!("{sum:02x}").as_bytes());
    socket.write_all(b"+").unwrap();
    reply
}

fn tool_json(result: CallToolResult) -> Value {
    assert_ne!(result.is_error, Some(true), "{:?}", result.content);
    serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap()
}

fn gdb_action(controller: &DesktopController, target: &SessionTarget, action: GdbAction) {
    controller
        .commands
        .send(Command::Gdb {
            target: target.clone(),
            action,
        })
        .unwrap();
}

fn listener(controller: &DesktopController, target: &SessionTarget) -> SocketAddr {
    wait(controller, |update| {
        update
            .gdb_status
            .get(target)
            .is_some_and(|status| status.address.is_some())
    });
    controller.latest.lock().unwrap().gdb_status[target]
        .address
        .unwrap()
}

fn attach(address: SocketAddr) -> TcpStream {
    let mut socket = TcpStream::connect(address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    assert!(gdb_packet(&mut socket, "qSupported").starts_with(b"PacketSize="));
    socket
}

fn port_closed(address: SocketAddr) -> bool {
    TcpStream::connect_timeout(&address, Duration::from_millis(50)).is_err()
}

fn wait_port_closed(address: SocketAddr) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !port_closed(address) {
        assert!(
            Instant::now() < deadline,
            "GDB port remained open: {address}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn check_gdb_ownership(controller: &DesktopController, target: &SessionTarget, pc: u32) {
    let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    gdb_action(
        controller,
        target,
        GdbAction::Start {
            port: occupied.local_addr().unwrap().port(),
        },
    );
    wait(controller, |update| {
        matches!(
            update.gdb_feedback.get(target),
            Some(GdbFeedback::Failed {
                kind: ErrorKind::AddrInUse,
                ..
            })
        )
    });
    drop(occupied);

    gdb_action(controller, target, GdbAction::Start { port: 0 });
    let address = listener(controller, target);
    assert!(address.ip().is_loopback() && address.port() != 0);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let server = controller.mcp_server();
    let mcp = |operation| {
        runtime.block_on(server.lotus_gdb(Parameters(GdbArgs {
            session: target.id.clone(),
            operation,
            port: 0,
        })))
    };
    let status = tool_json(mcp(GdbOperation::Status));
    assert_eq!(status["address"], address.to_string());
    assert_eq!(status["backend_supported"], true);
    assert_eq!(status["leased"], false);
    let conflict = mcp(GdbOperation::Start);
    assert_eq!(conflict.is_error, Some(true));
    assert!(conflict.content[0]
        .as_text()
        .unwrap()
        .text
        .contains(&address.to_string()));

    let mut socket = attach(address);
    wait(controller, |update| {
        target
            .resolve(&update.sessions)
            .is_some_and(|summary| summary.telemetry.desktop_debug_lease_blocked)
    });
    let status = tool_json(mcp(GdbOperation::Status));
    assert_eq!(status["connected"], true);
    assert_eq!(status["leased"], true);
    let mutation = runtime.block_on(server.lotus_debug(Parameters(DebugArgs {
        session: target.id.clone(),
        command: D::Advance { frames: 1 },
    })));
    assert_eq!(mutation.is_error, Some(true));
    assert!(mutation.content[0]
        .as_text()
        .unwrap()
        .text
        .contains("debug lease held by another client"));

    *controller.inspection_selection.lock().unwrap() =
        InspectionSelection::first_page(Panel::Breakpoints);
    controller
        .commands
        .send(Command::DebugAt {
            target: target.clone(),
            panel: Panel::Breakpoints,
            command: D::Advance { frames: 1 },
        })
        .unwrap();
    wait(controller, |update| {
        update.debug_result(Panel::Breakpoints).is_some_and(|request| {
            matches!(&request.status, DebugStatus::Failed { kind: ErrorKind::PermissionDenied, message }
                if message.contains("debug lease held by another client"))
        })
    });
    controller
        .commands
        .send(Command::DebugAt {
            target: target.clone(),
            panel: Panel::Memory,
            command: D::ReadMemory {
                address: pc,
                length: 16,
            },
        })
        .unwrap();
    wait(controller, |update| {
        update.debug_result(Panel::Memory).is_some_and(|request| {
            matches!(&request.status, DebugStatus::Finished { view, .. }
                if view.rows.iter().any(|row| row.key == ".bytes[15]"))
        })
    });
    assert_eq!(gdb_packet(&mut socket, "D"), b"OK");
    wait(controller, |update| {
        target
            .resolve(&update.sessions)
            .is_some_and(|summary| !summary.telemetry.desktop_debug_lease_blocked)
    });
    drop(socket);
    let socket = attach(address);
    assert_eq!(tool_json(mcp(GdbOperation::Status))["leased"], true);
    drop(socket); // Disconnect without a protocol detach.
    let deadline = Instant::now() + Duration::from_secs(5);
    while tool_json(mcp(GdbOperation::Status))["leased"] == true {
        assert!(
            Instant::now() < deadline,
            "GDB did not release the lease after disconnect"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let stopped = tool_json(mcp(GdbOperation::Stop));
    assert_eq!(stopped["stopped"], true);
    assert!(port_closed(address));

    let started = tool_json(mcp(GdbOperation::Start));
    let started_address: SocketAddr = started["address"].as_str().unwrap().parse().unwrap();
    assert_eq!(listener(controller, target), started_address);
    drop(server);
    drop(runtime); // The desktop still owns the listener.
    let _socket = attach(started_address);
    gdb_action(controller, target, GdbAction::Stop);
    wait(controller, |update| {
        update
            .gdb_status
            .get(target)
            .is_some_and(|status| status.state == lotus_gdb::State::Stopped)
            && target
                .resolve(&update.sessions)
                .is_some_and(|summary| !summary.telemetry.desktop_debug_lease_blocked)
    });
    assert!(port_closed(started_address));
    controller
        .commands
        .send(Command::DebugAt {
            target: target.clone(),
            panel: Panel::Breakpoints,
            command: D::Advance { frames: 1 },
        })
        .unwrap();
    wait(controller, |update| {
        update
            .debug_result(Panel::Breakpoints)
            .is_some_and(|request| matches!(&request.status, DebugStatus::Finished { .. }))
    });
}

#[test]
#[ignore = "requires LOTUS_NATIVE_WORKER and LOTUS_FIXTURE"]
fn inspectors_use_shared_debugger_and_parse_large_traces_in_background() {
    let worker = PathBuf::from(std::env::var_os("LOTUS_NATIVE_WORKER").unwrap());
    let fixture = PathBuf::from(std::env::var_os("LOTUS_FIXTURE").unwrap());
    for new in [false, true] {
        let root = std::env::temp_dir().join(format!("lotus-inspect-{}", SessionUuid::generate()));
        let mut config = HostConfig::new(&worker, &root);
        config.allow_control = true;
        config.assets = BTreeMap::from([("fixture".into(), fixture.clone())]);
        config.profile.insert(
            "is_new_3ds".into(),
            lotus_protocol::debug::SettingValue::Boolean(new),
        );
        let controller = DesktopController::start(config, McpMode::Off).unwrap();
        controller
            .commands
            .send(Command::Create {
                kind: SessionKind::Primary {
                    card: "inspect".into(),
                },
                asset: Some("fixture".into()),
            })
            .unwrap();
        wait(&controller, |update| !update.selected.is_empty());
        let id = controller.latest.lock().unwrap().selected.clone();
        controller
            .commands
            .send(Command::Pause(id.clone()))
            .unwrap();
        controller.inspector.store(true, Ordering::Release);
        wait(&controller, |update| {
            update.inspection.as_ref().is_some_and(|snapshot| {
                snapshot
                    .target
                    .as_ref()
                    .is_some_and(|target| target.is_current(&update.inspected, &update.sessions))
                    && snapshot.panel == Panel::Cpu
                    && snapshot.view.rows.iter().any(|r| r.key == "r15")
            })
        });
        let pc = {
            let update = controller.latest.lock().unwrap();
            let view = &update.inspection.as_ref().unwrap().view;
            u32::from_str_radix(
                &view.rows.iter().find(|r| r.key == "r15").unwrap().value,
                16,
            )
            .unwrap()
        };
        *controller.inspection_selection.lock().unwrap() =
            InspectionSelection::first_page(Panel::Threads);
        wait(&controller, |update| {
            update.inspection.as_ref().is_some_and(|snapshot| {
                snapshot.panel == Panel::Threads
                    && snapshot
                        .view
                        .rows
                        .iter()
                        .any(|r| r.key.starts_with(".threads["))
            })
        });
        *controller.inspection_selection.lock().unwrap() =
            InspectionSelection::first_page(Panel::Settings);
        wait(&controller, |update| {
            update.inspection.as_ref().is_some_and(|snapshot| {
                snapshot.panel == Panel::Settings
                    && snapshot.offset == Some(0)
                    && snapshot.view.next.is_some()
            })
        });
        let (target, next, first_setting) = {
            let update = controller.latest.lock().unwrap();
            let snapshot = update.inspection.as_ref().unwrap();
            (
                snapshot.target.clone().unwrap(),
                snapshot.view.next.unwrap(),
                snapshot.view.rows[0].key.clone(),
            )
        };
        *controller.inspection_selection.lock().unwrap() =
            InspectionSelection::with_offset(Panel::Settings, &target, next);
        wait(&controller, |update| {
            update.inspection.as_ref().is_some_and(|snapshot| {
                snapshot.panel == Panel::Settings
                    && snapshot.target.as_ref() == Some(&target)
                    && snapshot.offset == Some(next)
                    && snapshot
                        .view
                        .rows
                        .iter()
                        .all(|row| row.key != first_setting)
                    && !snapshot.view.rows.is_empty()
            })
        });
        *controller.inspection_selection.lock().unwrap() =
            InspectionSelection::first_page(Panel::Result);
        controller
            .commands
            .send(Command::DebugAt {
                target: target.clone(),
                panel: Panel::Result,
                command: D::ReadMemory {
                    address: pc,
                    length: 16,
                },
            })
            .unwrap();
        wait(&controller, |update| {
            update.debug_result(Panel::Result).is_some_and(|request| {
                matches!(&request.status, DebugStatus::Finished { view, .. }
                    if view.rows.iter().any(|r| r.key == ".bytes[15]"))
            })
        });
        #[cfg(feature = "silvestris-ui")]
        {
            *controller.inspection_selection.lock().unwrap() =
                InspectionSelection::first_page(Panel::Commands);
            controller
                .commands
                .send(Command::DebugAt {
                    target: target.clone(),
                    panel: Panel::Commands,
                    command: D::ReadMemory {
                        address: pc,
                        length: 8,
                    },
                })
                .unwrap();
            wait(&controller, |update| {
                update.debug_result(Panel::Commands).is_some_and(|request| {
                    request.target == target
                        && request.operation == "read_memory"
                        && matches!(&request.status, DebugStatus::Finished { view, .. }
                            if view.rows.iter().any(|row| row.key == ".bytes[7]"))
                })
            });
        }
        *controller.inspection_selection.lock().unwrap() =
            InspectionSelection::first_page(Panel::Gpu);
        controller
            .commands
            .send(Command::DebugAt {
                target: target.clone(),
                panel: Panel::Gpu,
                command: D::GpuConfigure {
                    enabled: true,
                    stop_events: vec![],
                    capture: Some(GpuCapture::Draws),
                    draw_range: None,
                    max_draws: Some(8),
                    resources: None,
                },
            })
            .unwrap();
        wait(&controller, |update| {
            update
                .debug_result(Panel::Gpu)
                .is_some_and(|request| matches!(&request.status, DebugStatus::Finished { .. }))
        });
        controller
            .commands
            .send(Command::DebugAt {
                target: target.clone(),
                panel: Panel::Gpu,
                command: D::Advance { frames: 30 },
            })
            .unwrap();
        wait(&controller, |update| {
            update.inspection.as_ref().is_some_and(|snapshot| {
                snapshot.panel == Panel::Gpu
                    && snapshot.view.rows.iter().any(|row| row.action.is_some())
            })
        });
        let command = {
            let update = controller.latest.lock().unwrap();
            update
                .inspection
                .as_ref()
                .unwrap()
                .view
                .rows
                .iter()
                .find_map(|row| row.action.clone())
                .unwrap()
        };
        assert!(matches!(command, D::GpuDraw { .. }));
        controller
            .commands
            .send(Command::DebugAt {
                target: target.clone(),
                panel: Panel::Gpu,
                command,
            })
            .unwrap();
        wait(&controller, |update| {
            update.debug_result(Panel::Gpu).is_some_and(|request| {
                matches!(request.command.as_ref(), D::GpuDraw { .. })
                    && matches!(&request.status, DebugStatus::Finished { view, .. }
                        if view.rows.iter().any(|row| row.key.starts_with(".capture.")))
            })
        });
        assert_eq!(
            controller.inspection_selection.lock().unwrap().panel,
            Panel::Gpu
        );
        check_gdb_ownership(&controller, &target, pc);
        gdb_action(&controller, &target, GdbAction::Start { port: 0 });
        let before_reset = listener(&controller, &target);
        controller
            .commands
            .send(Command::Reset(id.clone()))
            .unwrap();
        wait(&controller, |update| {
            target.resolve(&update.sessions).is_none()
        });
        wait_port_closed(before_reset);
        gdb_action(&controller, &target, GdbAction::Start { port: 0 });
        let replacement = {
            let update = controller.latest.lock().unwrap();
            update
                .gdb_status
                .keys()
                .find(|current| current.id == id)
                .unwrap()
                .clone()
        };
        assert_ne!(replacement.epoch, target.epoch);
        gdb_action(&controller, &replacement, GdbAction::Start { port: 0 });
        let until_stop = listener(&controller, &replacement);
        controller.inspector.store(false, Ordering::Release);
        *controller.inspection_selection.lock().unwrap() =
            InspectionSelection::first_page(Panel::Result);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("large.jsonl");
        std::fs::write(
            &path,
            "{\"kind\":\"ipc\",\"sequence\":1,\"guest_ticks\":100}\n".repeat(100000),
        )
        .unwrap();
        controller
            .commands
            .send(Command::OpenTrace { path })
            .unwrap();
        wait(&controller, |update| {
            update.inspection.as_ref().is_some_and(|snapshot| {
                snapshot.target.is_none()
                    && snapshot.view.rows.len() == 100000
                    && snapshot.view.lost == 0
            })
        });
        controller.commands.send(Command::Stop(id)).unwrap();
        wait(&controller, |update| {
            update.sessions.is_empty() && update.pending == 0
        });
        wait_port_closed(until_stop);
        assert!(controller.latest.lock().unwrap().error.is_empty());
        drop(controller);
        let metadata: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.join("cards/inspect/native-manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(metadata["lifecycle"], "closed");
        assert!(metadata["pid"].is_null());
        assert!(root.is_absolute() && root.starts_with(std::env::temp_dir()));
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
#[ignore = "requires LOTUS_AZAHAR_WORKER and LOTUS_FIXTURE"]
fn gdb_reports_unsupported_on_azahar() {
    let worker = PathBuf::from(std::env::var_os("LOTUS_AZAHAR_WORKER").unwrap());
    let fixture = PathBuf::from(std::env::var_os("LOTUS_FIXTURE").unwrap());
    let root = std::env::temp_dir().join(format!("lotus-gdb-azahar-{}", SessionUuid::generate()));
    let mut config = HostConfig::new(worker, &root);
    config.allow_control = true;
    config.assets.insert("fixture".into(), fixture);
    let controller = DesktopController::start(config, McpMode::Off).unwrap();
    controller
        .commands
        .send(Command::Create {
            kind: SessionKind::Secondary { parent: None },
            asset: Some("fixture".into()),
        })
        .unwrap();
    wait(&controller, |update| !update.gdb_status.is_empty());
    let target = controller
        .latest
        .lock()
        .unwrap()
        .gdb_status
        .keys()
        .next()
        .unwrap()
        .clone();
    {
        let update = controller.latest.lock().unwrap();
        assert_eq!(update.gdb_status[&target].backend, "AZAHAR");
        assert_eq!(update.gdb_status[&target].backend_support(), Some(false));
    }
    gdb_action(&controller, &target, GdbAction::Start { port: 0 });
    wait(&controller, |update| {
        matches!(
            update.gdb_feedback.get(&target),
            Some(GdbFeedback::Failed { kind: ErrorKind::Unsupported, message })
            if message.contains("requires the CITnLO backend")
        )
    });
    assert_eq!(
        controller.latest.lock().unwrap().gdb_status[&target].address,
        None
    );
    drop(controller);
    assert!(root.is_absolute() && root.starts_with(std::env::temp_dir()));
    std::fs::remove_dir_all(root).unwrap();
}
