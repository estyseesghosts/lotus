//! Registry parity (lotus#50): the `lotus.host.*` descriptors must carry the same defaults
//! as `HostConfig::new`, the `lotus.debug.*` bounds must be `DebugCommand::validate`'s
//! (lotus#56), and `Supervisor::new` must refuse profiles the registry rejects.

use std::collections::BTreeMap;

use lotus_protocol::debug::SettingValue;
use lotus_session::supervisor::{validate_profile, HostConfig, MAX_SESSIONS_LIMIT};
use lotus_settings::{Kind, Owner, Registry};

#[test]
fn host_descriptor_defaults_match_host_config() {
    let config = serde_json::to_value(HostConfig::new("worker.exe", "storage")).unwrap();
    let host: Vec<_> = Registry::builtin()
        .descriptors()
        .iter()
        .filter(|d| d.owner == Owner::Host)
        .collect();
    assert_eq!(host.len(), 22);
    for descriptor in host {
        let path = descriptor.key.strip_prefix("lotus.host.").unwrap();
        let mut value = &config;
        for part in path.split('.') {
            value = &value[part];
        }
        // Nullable budgets default to null, so a missing field cannot be told apart by value:
        // check the field exists on the parent object.
        let (parent, field) = path.rsplit_once('.').unwrap_or(("", path));
        let mut object = &config;
        for part in parent.split('.').filter(|p| !p.is_empty()) {
            object = &object[part];
        }
        assert!(
            object.get(field).is_some(),
            "{path} missing from HostConfig"
        );
        // The storage root has no default: every host configuration names one.
        if path != "storage_root" {
            assert_eq!(value, &descriptor.default, "{}", descriptor.key);
        }
    }
    let Kind::Integer { max, .. } = &Registry::builtin()
        .get("lotus.host.max_sessions")
        .unwrap()
        .kind
    else {
        panic!()
    };
    assert_eq!(*max, Some(MAX_SESSIONS_LIMIT as i64));
}

#[test]
fn supervisor_refuses_profiles_the_registry_rejects() {
    let profile = |key: &str, value: SettingValue| BTreeMap::from([(key.to_string(), value)]);
    validate_profile(&profile("graphics_api", SettingValue::Integer(2))).expect("vulkan");
    validate_profile(&profile("frame_limit", SettingValue::Integer(100))).expect("frame limit");
    validate_profile(&profile("is_new_3ds", SettingValue::Boolean(true))).expect("new 3ds");
    for (key, value, needle) in [
        (
            "no_such_setting",
            SettingValue::Integer(1),
            "unknown setting",
        ),
        ("use_gdbstub", SettingValue::Boolean(false), "refuses"),
        ("is_new_3ds", SettingValue::Integer(1), "invalid value"),
        (
            "cpu_clock_percentage",
            SettingValue::Integer(401),
            "outside",
        ),
        (
            "lotus.host.prefer_vulkan",
            SettingValue::Boolean(true),
            "Lotus setting",
        ),
    ] {
        let error = validate_profile(&profile(key, value)).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{key}");
        assert!(error.to_string().contains(needle), "{key}: {error}");
    }
}

/// lotus#56: every value a debugger-default descriptor accepts is one the request it fills
/// accepts, and the values just past its bounds are refused by both.
#[test]
fn debug_default_bounds_match_request_validation() {
    use lotus_protocol::debug::DebugCommand;
    use lotus_session::settings_store::DebugDefaults;
    let registry = Registry::builtin();
    let enable = DebugCommand::RewindConfigure {
        enabled: true,
        capacity: 8,
        budget_mib: 64,
        interval_frames: None,
        compression_level: None,
        compression_queue: None,
    };
    let features = ["rewind_compression_queue", "gpu_capture_levels"]
        .into_iter()
        .map(String::from)
        .collect();
    let mut checked = 0;
    for descriptor in registry
        .descriptors()
        .iter()
        .filter(|d| d.owner == Owner::Debugger)
    {
        let Kind::Integer {
            min: Some(min),
            max: Some(max),
            ..
        } = descriptor.kind
        else {
            continue;
        };
        for (value, accepted) in [(min - 1, false), (min, true), (max, true), (max + 1, false)] {
            assert_eq!(
                descriptor.validate(&serde_json::json!(value)).is_ok(),
                accepted,
                "{} {value}",
                descriptor.key
            );
            // Out-of-range values cannot reach DebugDefaults, so build the request directly.
            let mut request = enable.clone();
            if let DebugCommand::RewindConfigure {
                interval_frames,
                compression_level,
                compression_queue,
                ..
            } = &mut request
            {
                match descriptor.key.as_str() {
                    "lotus.debug.rewind.interval_frames" => {
                        *interval_frames = u32::try_from(value).ok();
                    }
                    "lotus.debug.rewind.compression_level" => {
                        *compression_level = i8::try_from(value).ok();
                    }
                    "lotus.debug.rewind.compression_queue" => {
                        *compression_queue = u8::try_from(value).ok();
                    }
                    other => panic!("unchecked debugger default {other}"),
                }
            }
            let representable =
                serde_json::to_value(&request).unwrap() != serde_json::to_value(&enable).unwrap();
            assert_eq!(
                representable && request.validate().is_ok(),
                accepted,
                "{} {value}",
                descriptor.key
            );
        }
        checked += 1;
    }
    assert_eq!(checked, 3);
    let capture = registry.get("lotus.debug.gpu.capture").unwrap();
    let Kind::String {
        choices: Some(levels),
        ..
    } = &capture.kind
    else {
        panic!()
    };
    for level in levels {
        let defaults = DebugDefaults {
            gpu_capture: Some(serde_json::from_value(serde_json::json!(level)).unwrap()),
            ..DebugDefaults::default()
        };
        let gpu = DebugCommand::GpuConfigure {
            enabled: true,
            stop_events: vec![],
            capture: None,
            draw_range: None,
            max_draws: None,
            resources: None,
        };
        let filled = defaults.fill(&gpu, &features).expect(level);
        filled.validate().expect(level);
    }
}
