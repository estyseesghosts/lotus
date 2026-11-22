//! Inspector/controller acceptance; no GUI input or screenshot access required.
use lotus_desktop::{
    desktop_controller::{
        Command, DebugStatus, DesktopController, GdbAction, GdbFeedback, McpMode, Update,
    },
    inspection::{DrawArtifact, Inspection, InspectionSelection, Panel, SessionTarget},
};
use lotus_mcp::tool_arguments::{DebugArgs, GdbArgs, GdbOperation, ReleaseArtifactsArgs};
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
                "inspector deadline: {} · GDB status {:?} · requests {:?}",
                value.error,
                value.gdb_status,
                value.gdb_feedback
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn check_gpu_comparison(controller: &DesktopController, target: &SessionTarget) {
    let send = |command| {
        controller
            .commands
            .send(Command::DebugAt {
                target: target.clone(),
                panel: Panel::Gpu,
                command,
            })
            .unwrap()
    };
    send(D::GpuConfigure {
        enabled: true,
        stop_events: vec![],
        capture: Some(GpuCapture::Draws),
        draw_range: None,
        max_draws: Some(4096),
        resources: Some(false),
    });
    wait(controller, |update| {
        target
            .resolve(&update.sessions)
            .and_then(|summary| summary.telemetry.gpu_capture.as_ref())
            .is_some_and(|ack| ack.max_draws == 4096)
    });
    send(D::Advance { frames: 2 });
    let snapshot_ids = |update: &Update| -> Vec<u64> {
        update
            .inspection
            .as_ref()
            .and_then(|snapshot| snapshot.view.draws.as_deref())
            .into_iter()
            .flatten()
            .filter(|header| header.snapshot == Some(true))
            .map(|header| header.draw)
            .take(2)
            .collect()
    };
    wait(controller, |update| snapshot_ids(update).len() == 2);
    let ids = snapshot_ids(&controller.latest.lock().unwrap());
    let load = |draw| -> DrawArtifact {
        send(D::GpuDraw { draw });
        let pick = |update: &Update| {
            let request = update.debug_result(Panel::Gpu)?;
            if !matches!(request.native_command(), Some(D::GpuDraw { draw: id }) if *id == draw) {
                return None;
            }
            match &request.status {
                DebugStatus::Finished { view, .. } => view.draw_artifact.clone(),
                DebugStatus::Failed { message, .. } => {
                    panic!("Draw {draw} capture failed: {message}")
                }
                _ => None,
            }
        };
        wait(controller, |update| pick(update).is_some());
        pick(&controller.latest.lock().unwrap()).unwrap()
    };
    let (a, b) = (load(ids[0]), load(ids[1]));
    controller
        .commands
        .send(Command::CompareGpuDraws { a, b })
        .unwrap();
    let pair = format!("A {} → B {}", ids[0], ids[1]);
    wait(controller, |update| {
        update.debug_result(Panel::Gpu).is_some_and(|request| {
            request.operation == "compare_draws"
                && matches!(&request.status, DebugStatus::Finished { view, .. }
                    if view.rows[0].key == pair
                        && view.rows.iter().any(|row| row.key == "resources"
                            && row.value == "Unavailable for A and B"))
        })
    });
}

fn check_gpu_saved_texture(controller: &DesktopController, target: &SessionTarget) {
    let finish = |operation: &'static str| {
        let prior = controller
            .latest
            .lock()
            .unwrap()
            .debug_result(Panel::Gpu)
            .map(|item| item.id);
        move |command: Command| {
            controller.commands.send(command).unwrap();
            wait(controller, |update| {
                update.debug_result(Panel::Gpu).is_some_and(|request| {
                    if request.operation != operation || Some(request.id) == prior {
                        return false;
                    }
                    match &request.status {
                        DebugStatus::Failed { message, .. } => {
                            panic!("{operation} failed: {message}")
                        }
                        DebugStatus::Finished { .. } => true,
                        _ => false,
                    }
                })
            });
            controller
                .latest
                .lock()
                .unwrap()
                .debug_result(Panel::Gpu)
                .unwrap()
                .status
                .clone()
        }
    };
    let run = |operation, command| {
        finish(operation)(Command::DebugAt {
            target: target.clone(),
            panel: Panel::Gpu,
            command,
        })
    };
    run(
        "gpu_configure",
        D::GpuConfigure {
            enabled: true,
            stop_events: vec![2],
            capture: Some(GpuCapture::Full),
            draw_range: None,
            max_draws: None,
            resources: None,
        },
    );
    let DebugStatus::Finished { view: stopped, .. } = run("advance", D::Advance { frames: 1 })
    else {
        unreachable!()
    };
    let draw = stopped
        .rows
        .iter()
        .find(|row| row.key == ".stop.draw")
        .expect("PICA draw stop")
        .value
        .parse::<u64>()
        .unwrap();
    let DebugStatus::Finished { view, .. } = run("gpu_draw", D::GpuDraw { draw }) else {
        unreachable!()
    };
    let artifact = view.draw_artifact.clone().unwrap();
    let resource = view
        .rows
        .iter()
        .find_map(|row| {
            if row.value != "\"texture\"" {
                return None;
            }
            row.key
                .strip_prefix(".capture.resources[")?
                .strip_suffix("].kind")?
                .parse::<usize>()
                .ok()
        })
        .expect("fixture draw contains a frozen texture");
    let field = |view: &Inspection, key: &str| -> u32 {
        let path = format!(".capture.resources[{resource}].{key}");
        view.rows
            .iter()
            .find(|row| row.key == path)
            .unwrap()
            .value
            .parse()
            .unwrap()
    };
    let (address, width, height, format) = (
        field(&view, "address"),
        field(&view, "width"),
        field(&view, "height"),
        field(&view, "format"),
    );
    let preview = || {
        let DebugStatus::Finished {
            image: Some(image), ..
        } = finish("preview_saved_texture")(Command::PreviewGpuTexture {
            draw: artifact.clone(),
            resource,
        })
        else {
            panic!("missing frozen image")
        };
        image.pixels.as_bytes(0).unwrap().to_vec()
    };
    let live = || {
        let DebugStatus::Finished {
            image: Some(image), ..
        } = run(
            "surface",
            D::Surface {
                address,
                width,
                height,
                format,
            },
        )
        else {
            panic!("missing reference image")
        };
        image.pixels.as_bytes(0).unwrap().to_vec()
    };
    let saved = preview();
    assert_eq!(saved.len(), (width * height * 4) as usize);
    assert_eq!(saved, live());
    let virtual_alias = field(&view, "virtual_aliases[0]");
    let DebugStatus::Finished { view: memory, .. } = run(
        "read_memory",
        D::ReadMemory {
            address: virtual_alias,
            length: 4,
        },
    ) else {
        unreachable!()
    };
    let original: Vec<u8> = (0..4)
        .map(|index| {
            memory
                .rows
                .iter()
                .find(|row| row.key == format!(".bytes[{index}]"))
                .unwrap()
                .value
                .parse()
                .unwrap()
        })
        .collect();
    let changed = original.iter().map(|byte| !byte).collect();
    run(
        "write_memory",
        D::WriteMemory {
            address: virtual_alias,
            bytes: changed,
        },
    );
    assert_ne!(saved, live());
    assert_eq!(saved, preview());
    run(
        "write_memory",
        D::WriteMemory {
            address: virtual_alias,
            bytes: original,
        },
    );
    let trace = view
        .rows
        .iter()
        .find_map(|row| match row.action {
            Some(D::ShaderTrace { draw, vertex: 1 }) => Some(D::ShaderTrace { draw, vertex: 1 }),
            _ => None,
        })
        .expect("second captured vertex");
    let DebugStatus::Finished { view: trace, .. } = run("shader_trace", trace) else {
        unreachable!()
    };
    assert!(trace
        .rows
        .iter()
        .any(|row| row.key == ".capture.vertex" && row.value == "1"));
    let uri = serde_json::from_str(
        &view
            .rows
            .iter()
            .find(|row| row.key == ".artifacts[0].uri")
            .unwrap()
            .value,
    )
    .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let released = tool_json(
        runtime.block_on(
            controller
                .mcp_server()
                .lotus_release_artifacts(Parameters(ReleaseArtifactsArgs { uris: vec![uri] })),
        ),
    );
    assert_eq!(released["released"], 1);
    controller
        .commands
        .send(Command::PreviewGpuTexture {
            draw: artifact,
            resource,
        })
        .unwrap();
    wait(controller, |update| {
        update.debug_result(Panel::Gpu).is_some_and(|request| {
            request.operation == "preview_saved_texture"
                && matches!(&request.status, DebugStatus::Failed { message, .. } if message.contains("expired"))
        })
    });
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
        config.profile.insert(
            "use_hw_shader".into(),
            lotus_protocol::debug::SettingValue::Boolean(false),
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
                    && snapshot.view.rows.iter().any(|r| r.key == "CPSR")
                    && snapshot
                        .view
                        .rows
                        .iter()
                        .any(|r| r.key == "instruction" && r.lane == "PC")
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
        wait(&controller, |update| {
            update
                .inspection
                .as_ref()
                .and_then(|snapshot| snapshot.view.gpu.as_ref())
                .is_some_and(|gpu| gpu.command_path == "batched" && gpu.snapshots_taken == Some(0))
                && target
                    .resolve(&update.sessions)
                    .is_some_and(|summary| summary.telemetry.gpu_capture.is_none())
        });
        controller
            .commands
            .send(Command::DebugAt {
                target: target.clone(),
                panel: Panel::Gpu,
                command: D::GpuConfigure {
                    enabled: true,
                    stop_events: vec![],
                    capture: Some(GpuCapture::Draws),
                    draw_range: Some([0, u64::MAX]),
                    max_draws: Some(8),
                    resources: Some(false),
                },
            })
            .unwrap();
        wait(&controller, |update| {
            update
                .debug_result(Panel::Gpu)
                .is_some_and(|request| matches!(&request.status, DebugStatus::Finished { .. }))
                && target
                    .resolve(&update.sessions)
                    .and_then(|summary| summary.telemetry.gpu_capture.as_ref())
                    .is_some_and(|ack| {
                        ack.epoch == target.epoch
                            && ack.enabled
                            && ack.capture == GpuCapture::Draws
                            && ack.draw_range == Some([0, u64::MAX])
                            && ack.max_draws == 8
                            && !ack.resources
                    })
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
                    && snapshot.view.draws.as_ref().is_some_and(|draws| {
                        !draws.is_empty()
                            && draws.iter().all(|draw| {
                                draw.command.is_some()
                                    && draw.vertices.is_some()
                                    && draw.snapshot.is_some()
                            })
                    })
                    && snapshot.view.gpu.as_ref().is_some_and(|gpu| {
                        gpu.command_path == "per_command"
                            && gpu.snapshots_taken == Some(8)
                            && gpu.snapshots_skipped_budget.is_some_and(|count| count > 0)
                    })
            })
        });
        let command = {
            let update = controller.latest.lock().unwrap();
            update
                .inspection
                .as_ref()
                .unwrap()
                .view
                .draws
                .as_ref()
                .unwrap()
                .iter()
                .map(|header| D::GpuDraw { draw: header.draw })
                .next()
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
                matches!(request.native_command(), Some(D::GpuDraw { .. }))
                    && matches!(&request.status, DebugStatus::Finished { view, .. }
                        if view.rows.iter().any(|row| row.key.starts_with(".capture.")))
            })
        });
        assert_eq!(
            controller.inspection_selection.lock().unwrap().panel,
            Panel::Gpu
        );
        check_gpu_comparison(&controller, &target);
        check_gpu_saved_texture(&controller, &target);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let server = controller.mcp_server();
        let reply = tool_json(runtime.block_on(server.lotus_debug(Parameters(DebugArgs {
            session: id.clone(),
            command: D::GpuConfigure {
                enabled: true,
                stop_events: vec![2, 7],
                capture: Some(GpuCapture::Draws),
                draw_range: Some([10, 20]),
                max_draws: Some(4096),
                resources: Some(true),
            },
        }))));
        wait(&controller, |update| {
            target
                .resolve(&update.sessions)
                .and_then(|summary| summary.telemetry.gpu_capture.as_ref())
                .is_some_and(|ack| {
                    ack.enabled
                        && ack.capture == GpuCapture::Draws
                        && ack.stop_events == [2, 7]
                        && ack.draw_range == Some([10, 20])
                        && ack.max_draws == 4096
                        && ack.resources
                        && ack.epoch == target.epoch
                })
        });
        assert_eq!(reply["stop_events"], serde_json::json!([2, 7]));
        controller
            .commands
            .send(Command::DebugAt {
                target: target.clone(),
                panel: Panel::Gpu,
                command: D::GpuConfigure {
                    enabled: false,
                    stop_events: vec![],
                    capture: Some(GpuCapture::Counters),
                    draw_range: None,
                    max_draws: None,
                    resources: None,
                },
            })
            .unwrap();
        wait(&controller, |update| {
            target
                .resolve(&update.sessions)
                .and_then(|summary| summary.telemetry.gpu_capture.as_ref())
                .is_some_and(|ack| !ack.enabled && ack.stop_events.is_empty())
                && update
                    .inspection
                    .as_ref()
                    .and_then(|snapshot| snapshot.view.gpu.as_ref())
                    .is_some_and(|gpu| gpu.command_path == "batched")
        });
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
        *controller.inspection_selection.lock().unwrap() =
            InspectionSelection::first_page(Panel::Cpu);
        controller
            .commands
            .send(Command::Pause(id.clone()))
            .unwrap();
        wait(&controller, |update| {
            replacement
                .resolve(&update.sessions)
                .is_some_and(|session| {
                    session.telemetry.state == lotus_session::SessionState::Paused
                        && !session.telemetry.playing
                })
        });
        controller
            .commands
            .send(Command::DebugAt {
                target: replacement.clone(),
                panel: Panel::Cpu,
                command: D::WriteRegister {
                    core: 0,
                    register: 15,
                    value: 0,
                },
            })
            .unwrap();
        wait(&controller, |update| {
            update.inspection.as_ref().is_some_and(|snapshot| {
                snapshot.target.as_ref() == Some(&replacement)
                    && snapshot.panel == Panel::Cpu
                    && snapshot.view.rows.iter().any(|row| {
                        row.key == "Disassembly unavailable" && row.value.contains("unmapped")
                    })
                    && snapshot
                        .view
                        .rows
                        .iter()
                        .any(|row| row.key == "r15" && row.value == "00000000")
                    && !snapshot
                        .view
                        .rows
                        .iter()
                        .any(|row| row.key == "instruction")
            })
        });
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
    controller
        .commands
        .send(Command::DebugAt {
            target: target.clone(),
            panel: Panel::Gpu,
            command: D::GpuConfigure {
                enabled: true,
                stop_events: vec![],
                capture: Some(GpuCapture::Events),
                draw_range: None,
                max_draws: None,
                resources: None,
            },
        })
        .unwrap();
    wait(&controller, |update| {
        update.debug_result(Panel::Gpu).is_some_and(|request| {
            matches!(
                &request.status, DebugStatus::Failed { kind: ErrorKind::Unsupported, message }
                if message.contains("gpu_capture_levels")
            )
        })
    });
    assert!(target
        .resolve(&controller.latest.lock().unwrap().sessions)
        .is_some_and(|summary| summary.telemetry.gpu_capture.is_none()));
    drop(controller);
    assert!(root.is_absolute() && root.starts_with(std::env::temp_dir()));
    std::fs::remove_dir_all(root).unwrap();
}
