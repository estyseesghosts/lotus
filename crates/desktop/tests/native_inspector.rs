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

fn wait(controller: &DesktopController, mut predicate: impl FnMut(&Update) -> bool) {
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

fn desktop_command(
    controller: &DesktopController,
    target: &SessionTarget,
    panel: Panel,
    command: D,
) -> DebugStatus {
    desktop_request(
        controller,
        target,
        panel,
        Command::DebugAt {
            target: target.clone(),
            panel,
            command,
        },
    )
}

fn desktop_request(
    controller: &DesktopController,
    target: &SessionTarget,
    panel: Panel,
    request: Command,
) -> DebugStatus {
    let previous = controller
        .latest
        .lock()
        .unwrap()
        .debug_requests()
        .last()
        .map(|r| r.id);
    controller.commands.send(request).unwrap();
    let status = |update: &Update| {
        update
            .debug_requests()
            .rev()
            .find(|request| {
                &request.target == target
                    && request.panel == panel
                    && previous.is_none_or(|id| request.id > id)
                    && !matches!(request.status, DebugStatus::Running)
            })
            .map(|request| request.status.clone())
    };
    wait(controller, |update| status(update).is_some());
    status(&controller.latest.lock().unwrap()).unwrap()
}

fn completed(status: DebugStatus) -> std::sync::Arc<Inspection> {
    match status {
        DebugStatus::Finished { view, .. } => view,
        status => panic!("Expected a successful native command: {status:?}"),
    }
}

fn selected_target(controller: &DesktopController) -> SessionTarget {
    let update = controller.latest.lock().unwrap();
    let summary = update
        .sessions
        .iter()
        .find(|s| s.info.id == update.selected)
        .unwrap();
    SessionTarget {
        id: summary.info.id.clone(),
        created_unix_ms: summary.info.created_unix_ms,
        epoch: summary.telemetry.epoch,
    }
}

#[test]
#[ignore = "requires LOTUS_NATIVE_WORKER and LOTUS_DEBUG_FIXTURE"]
fn kernel_explorer_observes_thread_identity_and_native_deadlock() {
    let worker = PathBuf::from(std::env::var_os("LOTUS_NATIVE_WORKER").unwrap());
    let fixture = PathBuf::from(std::env::var_os("LOTUS_DEBUG_FIXTURE").unwrap());
    let root = std::env::temp_dir().join(format!("lotus-kernel-{}", SessionUuid::generate()));
    let mut config = HostConfig::new(worker, &root);
    config.allow_control = true;
    config.assets = BTreeMap::from([
        ("fixture".into(), fixture.clone()),
        ("elf".into(), fixture.with_extension("elf")),
    ]);
    let controller = DesktopController::start(config, McpMode::Off).unwrap();
    let send = |command| controller.commands.send(command).unwrap();
    send(Command::Create {
        kind: SessionKind::Secondary { parent: None },
        asset: Some("fixture".into()),
    });
    wait(&controller, |update| {
        update
            .sessions
            .iter()
            .any(|summary| summary.info.id == update.selected)
    });
    let target = selected_target(&controller);
    send(Command::Pause(target.id.clone()));
    wait(&controller, |update| {
        target.resolve(&update.sessions).is_some_and(|summary| {
            !summary.telemetry.playing
                && summary.telemetry.state == lotus_session::SessionState::Paused
        })
    });
    let desktop = |command| desktop_command(&controller, &target, Panel::Threads, command);
    completed(desktop(D::Advance { frames: 1 }));
    *controller.inspection_selection.lock().unwrap() =
        InspectionSelection::first_page(Panel::Threads);
    controller.inspector.store(true, Ordering::Release);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mcp = controller.mcp_server();
    let debug = |command| {
        tool_json(runtime.block_on(mcp.lotus_debug(Parameters(DebugArgs {
            session: target.id.clone(),
            command,
        }))))
    };
    let native = debug(D::Registers { core: 0 });
    let thread = native["thread"].as_u64().unwrap() as u32;
    assert_ne!(thread, 0);
    let expected = debug(D::Processes);
    let process = expected["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == thread)
        .unwrap()["process"]
        .as_u64()
        .unwrap() as u32;
    wait(&controller, |update| {
        update.inspection.as_ref().is_some_and(|snapshot| {
            snapshot.target.as_ref() == Some(&target)
                && snapshot.view.data.as_ref().is_some_and(|data| {
                    data["processes"] == expected["processes"]
                        && data["threads"] == expected["threads"]
                })
        })
    });
    let scoped = |pid, command| {
        desktop_request(
            &controller,
            &target,
            Panel::Threads,
            Command::KernelAt {
                target: target.clone(),
                process: pid,
                thread,
                command,
            },
        )
    };
    let registers = D::ThreadRegisters { thread };
    let data = completed(scoped(process, registers.clone()));
    assert_eq!(
        data.data.as_ref().unwrap()["registers"],
        debug(registers)["registers"]
    );
    let backtrace = completed(scoped(process, D::Backtrace { core: 0, limit: 8 }));
    assert_eq!(
        backtrace.data.as_ref().unwrap()["frames"],
        debug(D::Backtrace { core: 0, limit: 8 })["frames"]
    );
    let old_r0 = native["registers"][0].as_u64().unwrap() as u32;
    let write = |value| D::WriteThreadRegister {
        thread,
        register: 0,
        value,
    };
    assert!(matches!(
        scoped(process.wrapping_add(1), write(!old_r0)),
        DebugStatus::Failed {
            kind: ErrorKind::NotFound,
            ..
        }
    ));
    completed(scoped(process, write(!old_r0)));
    let readback = debug(D::ThreadRegisters { thread });
    assert_eq!(readback["registers"][0], !old_r0);
    completed(scoped(process, write(old_r0)));
    let suspended = |suspended| D::Suspend {
        thread: Some(thread),
        suspended,
    };
    completed(scoped(process, suspended(true)));
    let check_suspended = |expected| {
        wait(&controller, |update| {
            update.inspection.as_ref().is_some_and(|snapshot| {
                snapshot.target.as_ref() == Some(&target)
                    && snapshot.view.data.as_ref().is_some_and(|data| {
                        data["threads"].as_array().is_some_and(|threads| {
                            threads.iter().any(|entry| {
                                entry["id"] == thread && entry["suspended"] == expected
                            })
                        })
                    })
            })
        });
    };
    check_suspended(true);
    completed(scoped(process, suspended(false)));
    check_suspended(false);
    let _ = debug(D::LoadSymbols {
        asset: "elf".into(),
    });
    let mode = debug(D::ResolveSymbol {
        query: "mode".into(),
    })["addresses"][0]
        .as_u64()
        .unwrap() as u32;
    let _ = debug(D::WriteMemory {
        address: mode,
        bytes: 6_u32.to_le_bytes().to_vec(),
    });
    completed(desktop(D::Advance { frames: 1 }));
    let deadlocks = completed(desktop(D::Deadlocks));
    let deadlocks = deadlocks.data.as_ref().unwrap();
    assert!(
        deadlocks["mutex_cycles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|cycle| cycle.as_array().unwrap().len() == 2),
        "{deadlocks}"
    );
    assert_eq!(deadlocks["wait_edges"], debug(D::Deadlocks)["wait_edges"]);
    let handles = completed(desktop(D::Handles));
    let handles = handles.data.as_ref().unwrap();
    assert!(handles["handles"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["process"] == process && entry["type"] == "Mutex"));
    assert_eq!(handles["handles"], debug(D::Handles)["handles"]);
    send(Command::Reset(target.id.clone()));
    wait(&controller, |update| {
        target.resolve(&update.sessions).is_none()
    });
    assert!(matches!(
        scoped(process, write(!old_r0)),
        DebugStatus::Expired
    ));
    drop(mcp);
    drop(controller);
    assert!(root.is_absolute() && root.starts_with(std::env::temp_dir()));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(feature = "silvestris-ui")]
#[ignore = "requires LOTUS_NATIVE_WORKER and LOTUS_DEBUG_FIXTURE"]
fn timeline_preserves_event_loss_and_citrace_provenance() {
    use lotus_protocol::debug::EventCategory;
    let worker = PathBuf::from(std::env::var_os("LOTUS_NATIVE_WORKER").unwrap());
    let fixture = PathBuf::from(std::env::var_os("LOTUS_DEBUG_FIXTURE").unwrap());
    let root = std::env::temp_dir().join(format!("lotus-timeline-{}", SessionUuid::generate()));
    let mut config = HostConfig::new(worker, &root);
    config.allow_control = true;
    config.assets = BTreeMap::from([
        ("fixture".into(), fixture.clone()),
        ("elf".into(), fixture.with_extension("elf")),
    ]);
    let controller = DesktopController::start(config, McpMode::Off).unwrap();
    let send = |command| controller.commands.send(command).unwrap();
    let create = |count| {
        send(Command::Create {
            kind: SessionKind::Secondary { parent: None },
            asset: Some("fixture".into()),
        });
        wait(&controller, |update| {
            update.sessions.len() == count
                && update
                    .sessions
                    .iter()
                    .any(|summary| summary.info.id == update.selected)
        });
        let target = selected_target(&controller);
        send(Command::Pause(target.id.clone()));
        wait(&controller, |update| {
            target.resolve(&update.sessions).is_some_and(|summary| {
                !summary.telemetry.playing
                    && summary.telemetry.state == lotus_session::SessionState::Paused
            })
        });
        target
    };
    let sibling = create(1);
    let target = create(2);
    assert_ne!(target, sibling);
    let timeline = |command| desktop_command(&controller, &target, Panel::Timeline, command);
    assert!(
        matches!(timeline(D::CitraceStop), DebugStatus::Failed { message, .. }
        if message.contains("no active CiTrace recording"))
    );
    let configure = D::EventsConfigure {
        notify: Some(vec![EventCategory::Stopped]),
        history: Some(vec![
            EventCategory::Log,
            EventCategory::Svc,
            EventCategory::Ipc,
        ]),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mcp = controller.mcp_server();
    let debug = |command| {
        tool_json(runtime.block_on(mcp.lotus_debug(Parameters(DebugArgs {
            session: target.id.clone(),
            command,
        }))))
    };
    let ack = debug(configure.clone());
    let reply = completed(timeline(configure));
    for field in ["notify", "history"] {
        assert_eq!(reply.data.as_ref().unwrap()[field], ack[field]);
    }
    completed(timeline(D::Trace { enabled: true }));
    let _ = debug(D::LoadSymbols {
        asset: "elf".into(),
    });
    let mode = debug(D::ResolveSymbol {
        query: "mode".into(),
    })["addresses"][0]
        .as_u64()
        .unwrap() as u32;
    let _ = debug(D::WriteMemory {
        address: mode,
        bytes: 8_u32.to_le_bytes().to_vec(),
    });
    completed(timeline(D::Advance { frames: 2 }));
    *controller.inspection_selection.lock().unwrap() =
        InspectionSelection::first_page(Panel::Timeline);
    controller.inspector.store(true, Ordering::Release);
    wait(&controller, |update| {
        update.inspection.as_ref().is_some_and(|snapshot| {
            snapshot.target.as_ref() == Some(&target)
                && snapshot.view.data.as_ref().is_some_and(|data| {
                    data["history_gap"] == true
                        && data["lost_total"].as_u64().is_some_and(|lost| lost > 0)
                        && data["events"].as_array().is_some_and(|events| {
                            events.iter().any(|event| event["kind"] == "guest_log")
                                && events.iter().any(|event| {
                                    event["kind"] == "svc_enter" && event["guest_ticks"].is_number()
                                })
                        })
                })
        })
    });
    completed(timeline(D::Trace { enabled: false }));
    let _ = debug(D::WriteMemory {
        address: mode,
        bytes: 0_u32.to_le_bytes().to_vec(),
    });
    let recording = completed(timeline(D::CitraceStart));
    assert_eq!(recording.data.as_ref().unwrap()["recording"], true);
    completed(desktop_command(
        &controller,
        &sibling,
        Panel::Cpu,
        D::Registers { core: 0 },
    ));
    completed(timeline(D::Advance { frames: 2 }));
    let trace = completed(timeline(D::CitraceStop));
    let trace = trace.data.as_ref().unwrap();
    assert_eq!(trace["recording"], false);
    assert_eq!(trace["artifacts"][0]["mime_type"], "application/x-citrace");
    assert!(trace["identity"]["source"]
        .as_str()
        .is_some_and(|pin| pin.len() == 40));
    let uri = trace["artifacts"][0]["uri"].as_str().unwrap();
    let destination = root.join("exported.ctf");
    send(Command::ExportTrace {
        target: target.clone(),
        uri: uri.into(),
        path: destination.clone(),
    });
    wait(&controller, |update| {
        update.pending == 0 && destination.is_file()
    });
    assert_eq!(
        std::fs::metadata(&destination).unwrap().len(),
        trace["artifacts"][0]["bytes"].as_u64().unwrap()
    );
    let replayed = timeline(D::CitraceReplay);
    assert!(
        matches!(
            replayed,
            DebugStatus::Expired | DebugStatus::Finished { .. }
        ),
        "{replayed:?}"
    );
    wait(&controller, |update| {
        target.resolve(&update.sessions).is_none()
    });
    let replay = selected_target(&controller);
    let status = completed(desktop_command(
        &controller,
        &replay,
        Panel::Timeline,
        D::CitraceStatus,
    ));
    assert_eq!(status.data.as_ref().unwrap()["gpu_replay_view"], true);
    drop(mcp);
    drop(controller);
    assert!(root.is_absolute() && root.starts_with(std::env::temp_dir()));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires LOTUS_NATIVE_WORKER and LOTUS_DEBUG_FIXTURE"]
fn snapshots_and_movies_use_the_owned_fixture() {
    let worker = PathBuf::from(std::env::var_os("LOTUS_NATIVE_WORKER").unwrap());
    let fixture = PathBuf::from(std::env::var_os("LOTUS_DEBUG_FIXTURE").unwrap());
    let root = std::env::temp_dir().join(format!("lotus-history-{}", SessionUuid::generate()));
    let mut config = HostConfig::new(worker, &root);
    config.allow_control = true;
    config.policy.rewind_sessions = Some(1);
    config.assets.insert("fixture".into(), fixture);
    config.profile.insert(
        "deterministic_async_operations".into(),
        lotus_protocol::debug::SettingValue::Boolean(true),
    );
    let controller = DesktopController::start(config, McpMode::Off).unwrap();
    let send = |command| controller.commands.send(command).unwrap();
    *controller.inspection_selection.lock().unwrap() =
        InspectionSelection::first_page(Panel::States);
    controller.inspector.store(true, Ordering::Release);
    let create = |name: &str| {
        send(Command::Create {
            kind: SessionKind::Primary { card: name.into() },
            asset: None,
        });
        wait(&controller, |update| {
            update.sessions.iter().any(|s| {
                s.info.id == update.selected
                    && matches!(&s.info.kind, SessionKind::Primary { card } if card == name)
            })
        });
        selected_target(&controller)
    };
    let load = |previous: &SessionTarget| {
        send(Command::Load {
            session: previous.id.clone(),
            asset: "fixture".into(),
        });
        wait(&controller, |update| {
            update.sessions.iter().any(|session| {
                session.info.id == previous.id && session.telemetry.epoch > previous.epoch
            })
        });
        send(Command::Pause(previous.id.clone()));
        wait(&controller, |update| {
            update.sessions.iter().any(|session| {
                session.info.id == previous.id
                    && session.telemetry.state == lotus_session::SessionState::Paused
                    && !session.telemetry.playing
            })
        });
        selected_target(&controller)
    };
    let owner = create("history");
    let command = |target: &SessionTarget, command| {
        desktop_command(&controller, target, Panel::States, command)
    };
    let prepared = completed(command(
        &owner,
        D::MovieRecord {
            name: "owned".into(),
        },
    ));
    assert_eq!(prepared.data.as_ref().unwrap()["prepared"], "record");
    let owner = load(&owner);
    completed(command(&owner, D::Advance { frames: 3 }));
    let movie = completed(command(&owner, D::MovieStop));
    assert_eq!(
        movie.data.as_ref().unwrap()["artifacts"][0]["screen"],
        "ctm"
    );
    let rewind = D::RewindConfigure {
        enabled: true,
        capacity: 4,
        budget_mib: 32,
        interval_frames: Some(2),
        compression_level: Some(-2),
        compression_queue: Some(1),
    };
    let accepted = completed(command(&owner, rewind.clone()));
    let values = accepted.data.as_ref().unwrap();
    for (key, expected) in [
        ("capacity", 4),
        ("budget_bytes", 32 << 20),
        ("interval_frames", 2),
        ("compression_level", -2),
        ("compression_queue", 1),
    ] {
        assert_eq!(values[key], expected, "{key}");
    }
    let checkpoint = "desktop-history".to_owned();
    completed(command(
        &owner,
        D::SaveState {
            name: checkpoint.clone(),
        },
    ));
    wait(&controller, |update| {
        update.inspection.as_ref().is_some_and(|snapshot| {
            snapshot.target.as_ref() == Some(&owner)
                && snapshot.view.data.as_ref().is_some_and(|data| {
                    data["checkpoints"]["states"]
                        .as_array()
                        .is_some_and(|states| states.iter().any(|name| name == &checkpoint))
                        && data["rewind_policy"]["memory_reserved_mib"][&owner.id] == 352
                })
        })
    });
    let sibling = load(&create("sibling"));
    assert!(matches!(
        command(&sibling, rewind),
        DebugStatus::Failed {
            kind: ErrorKind::ResourceBusy,
            ..
        }
    ));
    let result = command(
        &owner,
        D::LoadState {
            name: checkpoint.clone(),
        },
    );
    assert!(
        matches!(result, DebugStatus::Expired | DebugStatus::Finished { .. }),
        "{result:?}"
    );
    wait(&controller, |update| {
        owner.resolve(&update.sessions).is_none()
    });
    send(Command::Stop(owner.id.clone()));
    wait(&controller, |update| {
        update.sessions.iter().all(|s| s.info.id != owner.id)
    });
    let replay = create("history");
    let plan = completed(command(
        &replay,
        D::MoviePlay {
            name: "owned".into(),
            read_only: true,
        },
    ));
    assert_eq!(plan.data.as_ref().unwrap()["prepared"], "play");
    let replay = load(&replay);
    let playback = completed(command(&replay, D::MovieStatus));
    assert!(matches!(
        playback.data.as_ref().unwrap()["mode"].as_u64(),
        Some(2 | 3)
    ));
    completed(command(&replay, D::MovieStop));
    send(Command::Stop(replay.id.clone()));
    wait(&controller, |update| {
        update.sessions.iter().all(|s| s.info.id != replay.id)
    });
    let mismatch = create("history");
    let incompatible = command(
        &mismatch,
        D::SettingsSet {
            name: "deterministic_async_operations".into(),
            value: lotus_protocol::debug::SettingValue::Boolean(false),
        },
    );
    completed(incompatible);
    let mismatch = load(&mismatch);
    assert!(
        matches!(command(&mismatch, D::LoadState { name: checkpoint }),
        DebugStatus::Failed { message, .. } if message.contains("checkpoint pin or settings mismatch"))
    );
    drop(controller);
    assert!(root.is_absolute() && root.starts_with(std::env::temp_dir()));
    std::fs::remove_dir_all(root).unwrap();
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

fn check_gpu_capture_budget(controller: &DesktopController, owner: &SessionTarget) {
    controller
        .commands
        .send(Command::Create {
            kind: SessionKind::Secondary { parent: None },
            asset: Some("fixture".into()),
        })
        .unwrap();
    wait(controller, |update| {
        update.sessions.iter().any(|s| s.info.id != owner.id)
    });
    let sibling = {
        let update = controller.latest.lock().unwrap();
        let summary = update
            .sessions
            .iter()
            .find(|s| s.info.id != owner.id)
            .unwrap();
        SessionTarget {
            id: summary.info.id.clone(),
            created_unix_ms: summary.info.created_unix_ms,
            epoch: summary.telemetry.epoch,
        }
    };
    controller
        .commands
        .send(Command::DebugAt {
            target: sibling.clone(),
            panel: Panel::Gpu,
            command: D::GpuConfigure {
                enabled: true,
                stop_events: vec![],
                capture: Some(GpuCapture::Full),
                draw_range: None,
                max_draws: None,
                resources: None,
            },
        })
        .unwrap();
    wait(controller, |update| {
        update.debug_requests().any(|request| {
            request.target == sibling
                && request.operation == "gpu_configure"
                && matches!(
                    &request.status,
                    DebugStatus::Failed {
                        kind: ErrorKind::ResourceBusy,
                        message
                    } if message.contains(&owner.id)
                )
        })
    });
    controller
        .commands
        .send(Command::Pause(sibling.id.clone()))
        .unwrap();
    wait(controller, |update| {
        sibling.resolve(&update.sessions).is_some_and(|s| {
            !s.telemetry.playing && s.telemetry.state == lotus_session::SessionState::Paused
        })
    });
    controller
        .commands
        .send(Command::DebugAt {
            target: sibling.clone(),
            panel: Panel::Cpu,
            command: D::Registers { core: 0 },
        })
        .unwrap();
    wait(controller, |update| {
        update.debug_requests().any(|request| {
            request.target == sibling
                && request.operation == "registers"
                && matches!(&request.status, DebugStatus::Finished { view, .. }
                if view.rows.iter().any(|row| row.key == ".registers[15]"))
        })
    });
    wait(controller, |update| {
        owner
            .resolve(&update.sessions)
            .and_then(|s| s.telemetry.gpu_capture.as_ref())
            .is_some_and(|config| config.enabled)
    });
    controller
        .commands
        .send(Command::Stop(sibling.id.clone()))
        .unwrap();
    controller
        .commands
        .send(Command::Select(owner.id.clone()))
        .unwrap();
    wait(controller, |update| {
        sibling.resolve(&update.sessions).is_none() && update.inspected == owner.id
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
    controller
        .commands
        .send(Command::PatchMemory {
            target: target.clone(),
            command: D::WriteMemory {
                address: pc,
                bytes: vec![0],
            },
            original: vec![0],
        })
        .unwrap();
    wait(controller, |update| {
        update.debug_result(Panel::Memory).is_some_and(|request| {
            request.operation == "write_memory"
                && matches!(
                    &request.status,
                    DebugStatus::Failed {
                        kind: ErrorKind::PermissionDenied,
                        ..
                    }
                )
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
    let mut retry = Instant::now();
    wait(controller, |update| {
        if matches!(
            update.gdb_feedback.get(target),
            Some(GdbFeedback::Failed {
                kind: ErrorKind::WouldBlock,
                ..
            })
        ) && retry.elapsed() >= Duration::from_millis(100)
        {
            // The manager's non-blocking lifecycle call can contend with a status poll.
            gdb_action(controller, target, GdbAction::Stop);
            retry = Instant::now();
        }
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
        config.policy.gpu_capture_sessions = Some(1);
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
        let configure = D::GpuConfigure {
            enabled: true,
            stop_events: vec![],
            capture: Some(GpuCapture::Draws),
            draw_range: Some([0, u64::MAX]),
            max_draws: Some(8),
            resources: Some(false),
        };
        controller
            .commands
            .send(Command::DebugAt {
                target: target.clone(),
                panel: Panel::Gpu,
                command: configure.clone(),
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
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let server = controller.mcp_server();
        let accepted = target
            .resolve(&controller.latest.lock().unwrap().sessions)
            .unwrap()
            .telemetry
            .gpu_capture
            .clone()
            .unwrap();
        let mcp_ack = tool_json(runtime.block_on(server.lotus_debug(Parameters(DebugArgs {
            session: id.clone(),
            command: configure,
        }))));
        assert_eq!(mcp_ack["enabled"], accepted.enabled);
        assert_eq!(
            mcp_ack["capture"],
            serde_json::to_value(accepted.capture).unwrap()
        );
        assert_eq!(
            mcp_ack["stop_events"],
            serde_json::json!(accepted.stop_events)
        );
        assert_eq!(
            mcp_ack["draw_range"],
            serde_json::json!(accepted.draw_range)
        );
        assert_eq!(mcp_ack["max_draws"], accepted.max_draws);
        assert_eq!(mcp_ack["resources"], accepted.resources);
        assert_eq!(mcp_ack["epoch"], accepted.epoch);
        drop(server);
        drop(runtime);
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
        check_gpu_capture_budget(&controller, &target);
        check_gpu_comparison(&controller, &target);
        check_gpu_saved_texture(&controller, &target);
        {
            use lotus_protocol::debug::EventCategory;
            let timeline = |command| {
                completed(desktop_command(
                    &controller,
                    &target,
                    Panel::Timeline,
                    command,
                ))
            };
            let config = timeline(D::EventsConfigure {
                notify: None,
                history: Some(vec![EventCategory::Pica, EventCategory::Ipc]),
            });
            let mut after = config.data.as_ref().unwrap()["latest"].as_u64().unwrap();
            timeline(D::Trace { enabled: true });
            timeline(D::Advance { frames: 2 });
            let mut kinds = std::collections::BTreeSet::new();
            for _ in 0..8 {
                let page = timeline(D::Events { after, limit: 128 });
                let data = page.data.as_ref().unwrap();
                for event in data["events"].as_array().unwrap() {
                    if let Some(kind) = event["kind"].as_str() {
                        kinds.insert(kind.to_owned());
                    }
                }
                after = data["next"].as_u64().unwrap();
                if data["latest"] == after {
                    break;
                }
            }
            timeline(D::Trace { enabled: false });
            assert!(kinds.contains("pica") && kinds.contains("ipc"), "{kinds:?}");
        }
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
        wait(&controller, |update| {
            update
                .gdb_status
                .get(&replacement)
                .is_some_and(|status| status.state == lotus_gdb::State::Stopped)
        });
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
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mcp = controller.mcp_server();
    let gdb = runtime.block_on(mcp.lotus_gdb(Parameters(GdbArgs {
        session: target.id.clone(),
        operation: GdbOperation::Start,
        port: 0,
    })));
    assert_eq!(gdb.is_error, Some(true));
    assert!(gdb.content[0]
        .as_text()
        .unwrap()
        .text
        .contains("requires the CITnLO backend"));
    let configure = D::GpuConfigure {
        enabled: true,
        stop_events: vec![],
        capture: Some(GpuCapture::Events),
        draw_range: None,
        max_draws: None,
        resources: None,
    };
    controller
        .commands
        .send(Command::DebugAt {
            target: target.clone(),
            panel: Panel::Gpu,
            command: configure.clone(),
        })
        .unwrap();
    let refused = runtime.block_on(mcp.lotus_debug(Parameters(DebugArgs {
        session: target.id.clone(),
        command: configure,
    })));
    assert_eq!(refused.is_error, Some(true));
    assert!(refused.content[0]
        .as_text()
        .unwrap()
        .text
        .contains("gpu_capture_levels"));
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
    for (panel, command) in [
        (
            Panel::Memory,
            D::Scan {
                address: 0x100000,
                length: 4,
                width: 4,
                value: 0,
            },
        ),
        (Panel::States, D::RewindStatus),
        (
            Panel::Memory,
            D::ScanResults {
                offset: 0,
                limit: 1,
            },
        ),
        (
            Panel::Memory,
            D::ScanNarrow {
                mode: lotus_protocol::debug::ScanMode::Changed,
                value: None,
                upper: None,
            },
        ),
        (Panel::Timeline, D::CitraceStatus),
        (Panel::Threads, D::ThreadRegisters { thread: 1 }),
    ] {
        let reply = desktop_command(&controller, &target, panel, command.clone());
        let DebugStatus::Failed {
            kind: ErrorKind::Unsupported,
            message,
        } = reply
        else {
            panic!("AZAHAR {command:?} should be unsupported: {reply:?}");
        };
        assert!(message.contains("CITnLO"));
        let refused = runtime.block_on(mcp.lotus_debug(Parameters(DebugArgs {
            session: target.id.clone(),
            command,
        })));
        assert_eq!(refused.is_error, Some(true));
        assert!(refused.content[0]
            .as_text()
            .unwrap()
            .text
            .contains("CITnLO"));
    }
    drop(controller);
    assert!(root.is_absolute() && root.starts_with(std::env::temp_dir()));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires LOTUS_NATIVE_WORKER and LOTUS_DEBUG_FIXTURE with the adjacent owned ELF"]
fn cpu_steps_and_symbols_use_the_owned_arm_thumb_fixture() {
    let worker = PathBuf::from(std::env::var_os("LOTUS_NATIVE_WORKER").unwrap());
    let fixture = PathBuf::from(std::env::var_os("LOTUS_DEBUG_FIXTURE").unwrap());
    let elf = fixture.with_extension("elf");
    let output = std::process::Command::new("llvm-objdump")
        .arg("-d")
        .arg(&elf)
        .output()
        .unwrap();
    assert!(output.status.success());
    let assembly = String::from_utf8(output.stdout).unwrap();
    let line_address =
        |line: &str| u32::from_str_radix(line.trim().split_once(':').unwrap().0, 16).unwrap();
    let arm_call = line_address(
        assembly
            .lines()
            .find(|line| line.contains("<tick>") && line.contains("\tbl\t"))
            .unwrap(),
    );
    let thumb_call = line_address(
        assembly
            .lines()
            .find(|line| line.contains("<thumb_tick>") && line.contains("\tblx\t"))
            .unwrap(),
    );
    let thumb = u32::from_str_radix(
        assembly
            .lines()
            .find_map(|line| line.strip_suffix(" <thumb_tick>:"))
            .unwrap(),
        16,
    )
    .unwrap();
    let root =
        std::env::temp_dir().join(format!("lotus-cpu-arm-thumb-{}", SessionUuid::generate()));
    let mut config = HostConfig::new(worker, &root);
    config.allow_control = true;
    config.assets = BTreeMap::from([("fixture".into(), fixture), ("elf".into(), elf)]);
    let controller = DesktopController::start(config, McpMode::Off).unwrap();
    let send = |command| controller.commands.send(command).unwrap();
    send(Command::Create {
        kind: SessionKind::Secondary { parent: None },
        asset: Some("fixture".into()),
    });
    wait(&controller, |update| {
        update
            .sessions
            .iter()
            .any(|session| session.info.id == update.selected)
    });
    let target = {
        let update = controller.latest.lock().unwrap();
        let summary = update
            .sessions
            .iter()
            .find(|s| s.info.id == update.selected)
            .unwrap();
        SessionTarget {
            id: summary.info.id.clone(),
            created_unix_ms: summary.info.created_unix_ms,
            epoch: summary.telemetry.epoch,
        }
    };
    send(Command::Pause(target.id.clone()));
    wait(&controller, |update| {
        target.resolve(&update.sessions).is_some_and(|summary| {
            summary.telemetry.state == lotus_session::SessionState::Paused
                && !summary.telemetry.playing
        })
    });
    controller.inspector.store(true, Ordering::Release);

    let run = |panel, command: D, check: &dyn Fn(&Inspection) -> bool| {
        let previous = controller
            .latest
            .lock()
            .unwrap()
            .debug_result(panel)
            .map(|request| request.id);
        send(Command::DebugAt {
            target: target.clone(),
            panel,
            command,
        });
        wait(&controller, |update| {
            update.debug_result(panel).is_some_and(|request| {
                if request.target != target || Some(request.id) == previous {
                    return false;
                }
                match &request.status {
                    DebugStatus::Finished { view, .. } => {
                        assert!(check(view), "{}: {:?}", request.operation, view.rows);
                        true
                    }
                    DebugStatus::Failed { message, .. } => {
                        panic!("{}: {message}", request.operation)
                    }
                    _ => false,
                }
            })
        });
    };
    let pc_is = |address, mode: &str| {
        wait(&controller, |update| {
            update.inspection.as_ref().is_some_and(|snapshot| {
                snapshot.panel == Panel::Cpu
                    && snapshot
                        .view
                        .rows
                        .iter()
                        .any(|row| row.key == "r15" && row.value == format!("{address:08x}"))
                    && snapshot
                        .view
                        .rows
                        .iter()
                        .any(|row| row.key == "CPSR" && row.value.ends_with(mode))
            }) && target.resolve(&update.sessions).is_some_and(|summary| {
                summary.telemetry.state == lotus_session::SessionState::Paused
            })
        })
    };
    let requested = |address: u32, view: &Inspection| {
        view.rows
            .iter()
            .any(|row| row.key == ".address" && row.value == address.to_string())
    };
    run(
        Panel::Result,
        D::LoadSymbols {
            asset: "elf".into(),
        },
        &|view| view.rows.iter().any(|row| row.key == ".elf_sha256"),
    );
    run(
        Panel::Cpu,
        D::RunTo {
            core: 0,
            address: arm_call,
        },
        &|view| requested(arm_call, view),
    );
    pc_is(arm_call, "ARM");
    run(Panel::Cpu, D::StepOver { core: 0 }, &|view| {
        requested(arm_call + 4, view)
    });
    pc_is(arm_call + 4, "ARM");
    run(
        Panel::Cpu,
        D::RunTo {
            core: 0,
            address: thumb_call,
        },
        &|view| requested(thumb_call, view),
    );
    pc_is(thumb_call, "ARM");
    run(Panel::Cpu, D::Step { core: 0, count: 1 }, &|view| {
        view.rows
            .iter()
            .any(|row| row.key == ".registers[15]" && row.value == format!("{thumb:08x}"))
    });
    pc_is(thumb, "Thumb");
    wait(&controller, |update| {
        update.inspection.as_ref().is_some_and(|snapshot| {
            snapshot
                .view
                .rows
                .iter()
                .any(|row| row.key == "ELF symbol" && row.value.contains("thumb_tick"))
                && snapshot
                    .view
                    .rows
                    .iter()
                    .any(|row| row.lane == "PC" && row.value.contains("push"))
        })
    });
    // Execute the Thumb frame-pointer prologue first so the backtrace uses this frame.
    run(Panel::Cpu, D::Step { core: 0, count: 2 }, &|_| true);
    run(Panel::Cpu, D::Backtrace { core: 0, limit: 5 }, &|view| {
        view.rows.iter().any(|row| row.key == ".elf_sha256")
    });
    run(Panel::Cpu, D::StepOut { core: 0 }, &|view| {
        requested(thumb_call + 4, view)
    });
    pc_is(thumb_call + 4, "ARM");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let server = controller.mcp_server();
    let debug_mcp = |command| {
        tool_json(runtime.block_on(server.lotus_debug(Parameters(DebugArgs {
            session: target.id.clone(),
            command,
        }))))
    };
    let watched = debug_mcp(D::ResolveSymbol {
        query: "watched".into(),
    })["addresses"][0]
        .as_u64()
        .unwrap() as u32;
    for address in [arm_call, watched] {
        let read = D::ReadMemory { address, length: 4 };
        let bytes = debug_mcp(read.clone())["bytes"].as_array().unwrap().clone();
        run(Panel::Memory, read, &|view| {
            bytes.iter().enumerate().all(|(index, expected)| {
                view.rows.iter().any(|row| {
                    row.key == format!(".bytes[{index}]")
                        && row.value.parse::<u8>().ok().map(u64::from) == expected.as_u64()
                })
            })
        });
    }
    let read = D::ReadMemory {
        address: watched,
        length: 1,
    };
    let actual = debug_mcp(read.clone())["bytes"][0].as_u64().unwrap() as u8;
    send(Command::PatchMemory {
        target: target.clone(),
        command: D::WriteMemory {
            address: watched,
            bytes: vec![!actual],
        },
        original: vec![!actual],
    });
    wait(&controller, |update| {
        update.debug_result(Panel::Memory).is_some_and(|request|
        request.operation == "write_memory" &&
        matches!(&request.status, DebugStatus::Failed { kind: ErrorKind::InvalidData, message }
            if message.contains("Guest bytes changed")))
    });
    assert_eq!(debug_mcp(read)["bytes"][0], actual);
    let watched_value = || {
        let data = debug_mcp(D::ReadMemory {
            address: watched,
            length: 4,
        });
        u32::from_le_bytes(std::array::from_fn(|i| {
            data["bytes"][i].as_u64().unwrap() as u8
        }))
    };
    let initial = watched_value();
    let scan = D::Scan {
        address: watched,
        length: 4,
        width: 4,
        value: u64::from(initial),
    };
    let expected = debug_mcp(scan.clone())["matches"].clone();
    assert_eq!(expected[0]["address"], watched);
    let generation = std::cell::Cell::new(0);
    run(Panel::Memory, scan.clone(), &|view| {
        let data = view.data.as_ref().unwrap();
        generation.set(data["scan_generation"].as_u64().unwrap());
        data["matches"] == expected && data["total"] == 1
    });
    let page = D::ScanResults {
        offset: 0,
        limit: 1,
    };
    assert_eq!(debug_mcp(page.clone())["matches"], expected);
    assert_eq!(debug_mcp(page.clone())["matches"], expected);
    run(Panel::Memory, page.clone(), &|view| {
        view.data.as_ref().unwrap()["matches"] == expected
    });
    run(Panel::Memory, D::Advance { frames: 1 }, &|_| true);
    let changed = watched_value();
    assert_ne!(changed, initial);
    let narrow = D::ScanNarrow {
        mode: lotus_protocol::debug::ScanMode::Changed,
        value: None,
        upper: None,
    };
    send(Command::ScanAt {
        target: target.clone(),
        command: narrow,
        generation: generation.get(),
    });
    wait(&controller, |update| {
        update.debug_result(Panel::Memory).is_some_and(|request|
        request.operation == "scan_narrow" && matches!(&request.status,
            DebugStatus::Finished { view, .. } if view.data.as_ref().unwrap()["matches"][0]["value"] == changed)
    )
    });
    let current_generation = controller
        .latest
        .lock()
        .unwrap()
        .debug_result(Panel::Memory)
        .and_then(|request| match &request.status {
            DebugStatus::Finished { view, .. } => {
                view.data.as_ref().unwrap()["scan_generation"].as_u64()
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(debug_mcp(scan)["total"], 0);
    send(Command::ScanAt {
        target: target.clone(),
        command: page,
        generation: current_generation,
    });
    wait(&controller, |update| {
        update.debug_result(Panel::Memory).is_some_and(|request|
        request.operation == "scan_results" && matches!(&request.status,
            DebugStatus::Failed { kind: ErrorKind::InvalidInput, message } if message.contains("replaced"))
    )
    });
    let canary = debug_mcp(D::ResolveSymbol {
        query: "canary".into(),
    })["addresses"][0]
        .as_u64()
        .unwrap() as u32;
    let _ = debug_mcp(D::WriteMemory {
        address: canary,
        bytes: (watched + 4).to_le_bytes().to_vec(),
    });
    let pointer = D::PointerChain {
        base: canary,
        offsets: vec![-4],
    };
    let expected = debug_mcp(pointer.clone());
    assert_eq!(expected["address"], watched);
    run(Panel::Memory, pointer, &|view| {
        view.data.as_ref().unwrap()["hops"] == expected["hops"]
    });
    send(Command::DebugAt {
        target: target.clone(),
        panel: Panel::Memory,
        command: D::PointerChain {
            base: 0,
            offsets: vec![0],
        },
    });
    wait(&controller, |update| {
        update.debug_result(Panel::Memory).is_some_and(|request|
        matches!(&request.status, DebugStatus::Failed { message, .. } if message.contains("unmapped"))
    )
    });
    drop(server);
    drop(runtime);

    send(Command::Reset(target.id.clone()));
    wait(&controller, |update| {
        target.resolve(&update.sessions).is_none()
            && update.inspection.as_ref().is_some_and(|snapshot| {
                snapshot
                    .target
                    .as_ref()
                    .is_some_and(|replacement| replacement.epoch != target.epoch)
                    && snapshot.view.rows.iter().all(|row| row.key != "ELF symbol")
            })
    });
    let reset = selected_target(&controller);
    assert!(matches!(
        desktop_command(&controller, &reset, Panel::Memory, D::ScanResults { offset: 0, limit: 1 }),
        DebugStatus::Failed { kind: ErrorKind::InvalidInput, message } if message.contains("Start a new scan")
    ));
    send(Command::Stop(target.id));
    wait(&controller, |update| update.sessions.is_empty());
    drop(controller);
    assert!(root.is_absolute() && root.starts_with(std::env::temp_dir()));
    std::fs::remove_dir_all(root).unwrap();
}

fn tool_error(result: CallToolResult) -> String {
    assert_eq!(result.is_error, Some(true), "{:?}", result.content);
    result.content[0].as_text().unwrap().text.clone()
}

/// `llvm-nm` address of a defined symbol, as an independent reference.
fn nm_address(elf: &std::path::Path, name: &str) -> u32 {
    let output = std::process::Command::new("llvm-nm")
        .arg(elf)
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    let line = text
        .lines()
        .find(|line| line.ends_with(&format!(" {name}")))
        .unwrap_or_else(|| panic!("{name} missing from {}", elf.display()));
    u32::from_str_radix(&line[..8], 16).unwrap()
}

/// `llvm-addr2line` file and line for an address, as an independent reference.
fn addr2line(elf: &std::path::Path, address: u32) -> (String, u32) {
    let output = std::process::Command::new("llvm-addr2line")
        .arg("-e")
        .arg(elf)
        .arg(format!("{address:#x}"))
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    let (file, line) = text.trim().rsplit_once(':').unwrap();
    let line = line.split_whitespace().next().unwrap().parse().unwrap();
    (file.replace('\\', "/"), line)
}

/// Result-panel rows under `prefix`, decoded from their JSON text.
fn result_rows(view: &Inspection, prefix: &str) -> BTreeMap<String, Value> {
    view.rows
        .iter()
        .filter(|row| row.key.starts_with(prefix))
        .map(|row| (row.key.clone(), serde_json::from_str(&row.value).unwrap()))
        .collect()
}

/// The same flattening the Result panel applies to a reply.
fn flatten(value: &Value, key: String, out: &mut BTreeMap<String, Value>) {
    match value {
        Value::Object(map) => map
            .iter()
            .for_each(|(name, value)| flatten(value, format!("{key}.{name}"), out)),
        Value::Array(values) => values
            .iter()
            .enumerate()
            .for_each(|(i, value)| flatten(value, format!("{key}[{i}]"), out)),
        _ => {
            out.insert(key, value.clone());
        }
    }
}

fn flattened(value: &Value, key: &str) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    flatten(value, key.into(), &mut out);
    out
}

fn sha256_file(path: &std::path::Path) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(std::fs::read(path).unwrap()))
}

#[test]
#[ignore = "requires LOTUS_NATIVE_WORKER and LOTUS_DEBUG_FIXTURE(_GCC, _SHIFTED) builds with adjacent ELFs"]
fn source_identity_binds_only_the_loaded_build() {
    let worker = PathBuf::from(std::env::var_os("LOTUS_NATIVE_WORKER").unwrap());
    let clang = PathBuf::from(std::env::var_os("LOTUS_DEBUG_FIXTURE").unwrap());
    let gcc = PathBuf::from(std::env::var_os("LOTUS_DEBUG_FIXTURE_GCC").unwrap());
    let shifted = PathBuf::from(std::env::var_os("LOTUS_DEBUG_FIXTURE_SHIFTED").unwrap());
    let (clang_elf, gcc_elf, shifted_elf) = (
        clang.with_extension("elf"),
        gcc.with_extension("elf"),
        shifted.with_extension("elf"),
    );
    let root = std::env::temp_dir().join(format!("lotus-source-id-{}", SessionUuid::generate()));
    std::fs::create_dir_all(&root).unwrap();
    // The worker re-reads this file on reset, so overwriting it is a rebuild.
    let rebuild = root.join("rebuild.3dsx");
    std::fs::copy(&clang, &rebuild).unwrap();
    let mut config = HostConfig::new(worker, root.join("host"));
    config.allow_control = true;
    config.assets = BTreeMap::from([
        ("rebuild".into(), rebuild.clone()),
        ("gcc".into(), gcc.clone()),
        ("clang_elf".into(), clang_elf.clone()),
        ("gcc_elf".into(), gcc_elf.clone()),
        ("shifted_elf".into(), shifted_elf.clone()),
    ]);
    let controller = DesktopController::start(config, McpMode::Off).unwrap();
    let send = |command| controller.commands.send(command).unwrap();
    let create = |asset: &str, known: &[String]| {
        send(Command::Create {
            kind: SessionKind::Secondary { parent: None },
            asset: Some(asset.into()),
        });
        wait(&controller, |update| {
            !known.contains(&update.selected)
                && update
                    .sessions
                    .iter()
                    .any(|summary| summary.info.id == update.selected)
        });
        let target = selected_target(&controller);
        send(Command::Pause(target.id.clone()));
        wait(&controller, |update| {
            target.resolve(&update.sessions).is_some_and(|summary| {
                !summary.telemetry.playing
                    && summary.telemetry.state == lotus_session::SessionState::Paused
            })
        });
        target
    };
    let a = create("rebuild", &[]);
    let b = create("gcc", std::slice::from_ref(&a.id));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mcp = controller.mcp_server();
    let call = |target: &SessionTarget, command| {
        runtime.block_on(mcp.lotus_debug(Parameters(DebugArgs {
            session: target.id.clone(),
            command,
        })))
    };
    let load = |asset: &str| D::LoadSymbols {
        asset: asset.into(),
    };
    let refused = |target: &SessionTarget, asset: &str| {
        let message = tool_error(call(target, load(asset)));
        match desktop_command(&controller, target, Panel::Result, load(asset)) {
            DebugStatus::Failed {
                kind,
                message: desktop,
            } => {
                assert_eq!(kind, ErrorKind::InvalidData);
                assert_eq!(desktop, message);
            }
            status => panic!("{asset} bound to the wrong build: {status:?}"),
        }
        message
    };

    // A different compiler and a changed layout both fail the byte comparison.
    assert!(refused(&a, "gcc_elf").contains("differs from guest memory"));
    assert!(refused(&a, "shifted_elf").contains("differs from guest memory"));
    assert!(refused(&b, "clang_elf").contains("differs from guest memory"));
    let probe = D::Symbolize {
        addresses: vec![0x0010_0000],
    };
    assert!(tool_error(call(&a, probe.clone())).contains("load an approved ELF"));

    let bound = completed(desktop_command(
        &controller,
        &a,
        Panel::Result,
        load("clang_elf"),
    ));
    let row = |key: &str| result_rows(&bound, key).remove(key).unwrap();
    let bound = serde_json::json!({
        "mapping": row(".mapping"), "elf_sha256": row(".elf_sha256"),
        "content": {"sha256": row(".content.sha256")}, "process": row(".process"),
        "generation": row(".generation"),
        "sections": (0..)
            .map(|i| result_rows(&bound, &format!(".sections[{i}].")))
            .take_while(|fields| !fields.is_empty())
            .map(|fields| fields
                .into_iter()
                .map(|(key, value)| (key.rsplit('.').next().unwrap().to_string(), value))
                .collect::<serde_json::Map<_, _>>())
            .collect::<Vec<_>>(),
    });
    assert_eq!(bound["mapping"], "verified_bytes");
    assert_eq!(bound["elf_sha256"], sha256_file(&clang_elf));
    assert_eq!(bound["content"]["sha256"], sha256_file(&clang));
    assert!(bound["process"].as_u64().unwrap() > 0);
    let sections = bound["sections"].as_array().unwrap();
    let section = |name: &str| sections.iter().find(|s| s["name"] == name).unwrap();
    assert_eq!(section(".text")["verified"], true);
    assert_eq!(section(".rodata")["verified"], true);
    assert_eq!(section(".data")["verified"], false);
    let bound_b = tool_json(call(&b, load("gcc_elf")));
    assert_eq!(bound_b["elf_sha256"], sha256_file(&gcc_elf));
    assert_eq!(bound_b["content"]["sha256"], sha256_file(&gcc));

    // A/B builds share symbol names; each session resolves against its own build.
    for (target, elf, image) in [(&a, &clang_elf, &bound), (&b, &gcc_elf, &bound_b)] {
        let resolve = D::ResolveSymbol {
            query: "thumb_tick".into(),
        };
        let via_mcp = tool_json(call(target, resolve.clone()));
        assert_eq!(via_mcp["addresses"][0], nm_address(elf, "thumb_tick"));
        let via_desktop = completed(desktop_command(&controller, target, Panel::Result, resolve));
        assert_eq!(
            result_rows(&via_desktop, ".addresses"),
            flattened(&via_mcp["addresses"], ".addresses")
        );

        let functions = ["tick", "thumb_tick", "main", "lotus_tick_body"];
        let mut addresses: Vec<u32> = functions.iter().map(|f| nm_address(elf, f)).collect();
        addresses.extend([nm_address(elf, "watched"), 0x0800_0000]);
        let command = D::Symbolize {
            addresses: addresses.clone(),
        };
        let via_mcp = tool_json(call(target, command.clone()));
        let via_desktop = completed(desktop_command(&controller, target, Panel::Result, command));
        assert_eq!(
            result_rows(&via_desktop, ".locations"),
            flattened(&via_mcp["locations"], ".locations")
        );
        assert_eq!(via_mcp["image"]["process"], image["process"]);
        let locations = via_mcp["locations"].as_array().unwrap();
        for (index, function) in functions.iter().enumerate() {
            let (file, line) = addr2line(elf, addresses[index]);
            let location = &locations[index];
            assert_eq!(location["section"], ".text", "{function}");
            assert_eq!(location["line"], line, "{function} in {}", elf.display());
            let reported = location["file"].as_str().unwrap().replace('\\', "/");
            assert!(file.ends_with("fixture.c") && reported.ends_with("fixture.c"));
        }
        let data = &locations[functions.len()];
        assert_eq!(data["section"], ".data");
        assert_eq!(data["symbol"], "watched");
        assert_eq!(data["line"], Value::Null);
        let outside = &locations[functions.len() + 1];
        assert_eq!(outside["section"], Value::Null);
        assert_eq!(outside["symbol"], Value::Null);
    }

    // A rebuild with a changed layout: reset re-reads the file and drops the binding.
    std::fs::copy(&shifted, &rebuild).unwrap();
    tool_json(call(&a, D::Reset));
    let stale = tool_error(call(&a, probe.clone()));
    assert!(
        stale.contains("ELF binding dropped: guest epoch"),
        "{stale}"
    );
    let old = tool_error(call(&a, load("clang_elf")));
    assert!(old.contains("differs from guest memory"), "{old}");
    let rebound = tool_json(call(&a, load("shifted_elf")));
    assert_eq!(rebound["content"]["sha256"], sha256_file(&shifted));
    assert!(rebound["generation"].as_u64() > bound["generation"].as_u64());
    let thumb_tick = |target| {
        tool_json(call(
            target,
            D::ResolveSymbol {
                query: "thumb_tick".into(),
            },
        ))["addresses"][0]
            .clone()
    };
    assert_eq!(thumb_tick(&a), nm_address(&shifted_elf, "thumb_tick"));
    // The sibling's binding is unaffected by A's reset.
    assert_eq!(thumb_tick(&b), nm_address(&gcc_elf, "thumb_tick"));

    // svcExitProcess removes B's process and its mappings within the same epoch.
    let mode = nm_address(&gcc_elf, "mode");
    tool_json(call(
        &b,
        D::WriteMemory {
            address: mode,
            bytes: vec![9, 0, 0, 0],
        },
    ));
    tool_json(call(&b, D::Advance { frames: 2 }));
    let processes = tool_json(call(&b, D::Processes));
    assert!(
        processes["processes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["id"] != bound_b["process"]),
        "{processes}"
    );
    let exited = tool_error(call(&b, probe));
    assert!(
        exited.contains("no longer maps the bound ELF sections"),
        "{exited}"
    );
}
