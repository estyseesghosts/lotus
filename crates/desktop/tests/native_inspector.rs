//! Inspector/controller acceptance; no GUI input or screenshot access required.
use lotus_desktop::{
    desktop_controller::{Command, DebugStatus, DesktopController, McpMode, Update},
    inspection::{InspectionSelection, Panel, SessionTarget},
};
use lotus_protocol::{
    debug::{DebugCommand as D, GpuCapture},
    envelope::SessionUuid,
};
use lotus_session::supervisor::{HostConfig, SessionKind};
use std::{
    collections::BTreeMap,
    io::{ErrorKind, Read, Write},
    net::TcpStream,
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

fn check_gdb_ownership(controller: &DesktopController, target: &SessionTarget, pc: u32) {
    let gdb_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let gdb = gdb_runtime
        .block_on(lotus_gdb::Server::start(
            controller.host().clone(),
            target.id.clone(),
            0,
        ))
        .unwrap();
    let mut socket = TcpStream::connect(gdb.address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    assert!(gdb_packet(&mut socket, "qSupported").starts_with(b"PacketSize="));

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
    drop(socket);
    drop(gdb);
    drop(gdb_runtime);
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
