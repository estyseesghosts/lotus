//! Registry parity (lotus#50): the `lotus.host.*` descriptors must carry the same defaults
//! as `HostConfig::new`, and `Supervisor::new` must refuse profiles the registry rejects.

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
    assert_eq!(host.len(), 17);
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
        assert_eq!(value, &descriptor.default, "{}", descriptor.key);
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
