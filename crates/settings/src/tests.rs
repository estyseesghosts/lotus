use std::collections::BTreeSet;

use serde_json::json;

use crate::*;

fn registry() -> &'static Registry {
    Registry::builtin()
}

#[test]
fn every_inventory_setting_is_registered_once_and_classified() {
    let keys: Vec<&str> = registry()
        .descriptors()
        .iter()
        .map(|d| d.key.as_str())
        .collect();
    let unique: BTreeSet<&str> = keys.iter().copied().collect();
    assert_eq!(keys.len(), unique.len(), "duplicate keys");
    let azahar: Vec<&Descriptor> = registry()
        .descriptors()
        .iter()
        .filter(|d| d.owner == Owner::Azahar)
        .collect();
    // tools/settings_inventory.py on the pinned workers: 173 keys, 172 on AZAHAR.
    assert_eq!(azahar.len(), 173);
    let unclassified: Vec<&str> = azahar
        .iter()
        .filter(|d| d.subgroup == "Unclassified")
        .map(|d| d.key.as_str())
        .collect();
    assert!(unclassified.is_empty(), "no group for {unclassified:?}");
    for d in &azahar {
        assert!(!d.sources.is_empty(), "{} has no citation", d.key);
        assert!(
            !d.label.is_empty() && !d.description.is_empty(),
            "{}",
            d.key
        );
        assert_eq!(
            d.scopes == Scopes::NONE,
            matches!(d.availability, Availability::Excluded { .. }),
            "{}: excluded settings are settable nowhere, others everywhere",
            d.key
        );
    }
}

#[test]
fn apply_classes_match_the_worker_inventory() {
    let count = |apply| {
        registry()
            .descriptors()
            .iter()
            .filter(|d| d.owner == Owner::Azahar && d.apply == apply)
            .count()
    };
    // docs/31 counted 174 bindings with use_vsync twice (renderer); the worker now binds it
    // once.
    assert_eq!(count(Apply::Live), 19);
    assert_eq!(count(Apply::Renderer), 66);
    assert_eq!(count(Apply::Boot), 44);
    assert_eq!(count(Apply::CfgSavedata), 29);
    assert_eq!(count(Apply::Inert), 15);
}

#[test]
fn exclusions_cover_inert_locked_and_unused_renderer_settings() {
    let reason = |key: &str| match &registry().get(key).expect(key).availability {
        Availability::Excluded { reason } => reason.clone(),
        other => panic!("{key} is {other:?}"),
    };
    assert!(reason("use_gdbstub").contains("refuses"));
    assert!(reason("enable_rpc_server").contains("refuses"));
    assert!(reason("output_type").contains("bounded sink"));
    assert!(reason("enable_gamemode").contains("inert"));
    // Read only by renderer_opengl (renderer_opengl.cpp:430, :434).
    assert!(reason("pp_shader_name").contains("OpenGL"));
    // Read only by core/frontend layout code; captures use frames.h ScreenLayout.
    assert!(reason("screen_gap").contains("ScreenLayout"));
    assert!(reason("aspect_ratio").contains("ScreenLayout"));
    // Consumers in video_core proper stay available.
    for key in [
        "resolution_factor",
        "use_hw_shader",
        "graphics_api",
        "factor_3d",
        "volume",
        // Also read by vk_blit_helper.cpp.
        "texture_filter",
        // Six citations kept, so further consumers may exist: not excluded.
        "swap_screen",
    ] {
        assert_eq!(
            registry().get(key).expect(key).availability,
            Availability::Available,
            "{key}"
        );
    }
}

#[test]
fn citnlo_only_settings_are_unavailable_on_azahar_with_a_reason() {
    let only: Vec<&Descriptor> = registry()
        .descriptors()
        .iter()
        .filter(|d| {
            d.availability
                == Availability::BranchOnly {
                    branch: Branch::Experimental,
                }
        })
        .collect();
    assert_eq!(only.len(), 1, "{only:?}");
    let key = &only[0].key;
    let refused = registry()
        .validate(
            key,
            &only[0].default,
            Scope::Global,
            Some(Branch::Integration),
        )
        .unwrap_err();
    assert!(
        refused.to_string().contains("only in the CITnLO worker"),
        "{refused}"
    );
    registry()
        .validate(
            key,
            &only[0].default,
            Scope::Global,
            Some(Branch::Experimental),
        )
        .expect("available on CITnLO");
    assert!(registry()
        .validate(key, &only[0].default, Scope::Global, None)
        .is_err());
}

#[test]
fn values_are_checked_against_type_range_and_choices() {
    let r = registry();
    let check =
        |key: &str, value| r.validate(key, &value, Scope::Primary, Some(Branch::Integration));
    check("use_cpu_jit", json!(false)).expect("bool");
    assert!(check("use_cpu_jit", json!(1)).is_err());
    // cpu_clock_percentage: Setting range 5..=400 (settings.h).
    let Kind::Integer { min, max, .. } = &r.get("cpu_clock_percentage").unwrap().kind else {
        panic!()
    };
    assert_eq!((*min, *max), (Some(5), Some(400)));
    check("cpu_clock_percentage", json!(100)).expect("in range");
    assert!(check("cpu_clock_percentage", json!(401)).is_err());
    assert!(check("cpu_clock_percentage", json!(4)).is_err());
    assert!(check("cpu_clock_percentage", json!(50.5)).is_err());
    // Enum: GraphicsAPI choices from settings.h.
    check("graphics_api", json!(2)).expect("vulkan");
    assert!(check("graphics_api", json!(99)).is_err());
    // Float.
    check("volume", json!(0.5)).expect("float");
    assert!(check("volume", json!("loud")).is_err());
    // CFG blocks: exact-length lowercase hex.
    let cfg = r
        .descriptors()
        .iter()
        .find(|d| d.key.starts_with("cfg."))
        .expect("cfg block");
    let default = cfg.default.as_str().unwrap().to_string();
    r.validate(
        &cfg.key,
        &json!(default),
        Scope::Primary,
        Some(Branch::Integration),
    )
    .expect("default");
    let short = &default[..default.len() - 2];
    assert!(r
        .validate(
            &cfg.key,
            &json!(short),
            Scope::Primary,
            Some(Branch::Integration)
        )
        .is_err());
    let upper = default.to_ascii_uppercase().replace('0', "A");
    assert!(r
        .validate(
            &cfg.key,
            &json!(upper),
            Scope::Primary,
            Some(Branch::Integration)
        )
        .is_err());
    // Unknown and excluded keys.
    assert_eq!(
        check("no_such_setting", json!(1)).unwrap_err(),
        SettingError::Unknown {
            key: "no_such_setting".into()
        }
    );
    assert!(matches!(
        check("use_gdbstub", json!(false)).unwrap_err(),
        SettingError::Unavailable { .. }
    ));
}

#[test]
fn scopes_filter_host_and_desktop_settings_to_global() {
    let r = registry();
    for d in r.descriptors().iter().filter(|d| d.owner != Owner::Azahar) {
        // Input layouts (lotus#63) layer like worker settings; host and desktop do not.
        let expected = if d.owner == Owner::Input {
            Scopes::ALL
        } else {
            Scopes::GLOBAL
        };
        assert_eq!(d.scopes, expected, "{}", d.key);
        assert!(d.key.starts_with("lotus."), "{}", d.key);
        d.validate(&d.default).expect(&d.key);
    }
    assert!(matches!(
        r.validate(
            "lotus.host.prefer_vulkan",
            &json!(true),
            Scope::Primary,
            None
        )
        .unwrap_err(),
        SettingError::Scope { .. }
    ));
    r.validate("lotus.host.max_sessions", &json!(16), Scope::Global, None)
        .expect("limit");
    assert!(r
        .validate("lotus.host.max_sessions", &json!(17), Scope::Global, None)
        .is_err());
    r.validate(
        "lotus.host.policy.rewind_sessions",
        &json!(null),
        Scope::Global,
        None,
    )
    .expect("unset budget");
    assert!(r
        .validate(
            "lotus.host.policy.workload",
            &json!("turbo"),
            Scope::Global,
            None
        )
        .is_err());
    let primary: Vec<_> = r.for_scope(Scope::Primary).collect();
    assert!(primary
        .iter()
        .all(|d| matches!(d.owner, Owner::Azahar | Owner::Input)));
    assert!(primary
        .iter()
        .all(|d| d.available_on(Some(Branch::Experimental)).is_ok()));
}

#[test]
fn every_group_has_settings_and_schema_lists_them() {
    let r = registry();
    let schema = r.schema(None);
    let groups = schema["groups"].as_array().unwrap();
    assert_eq!(groups.len(), Group::ALL.len());
    let listed: usize = groups
        .iter()
        .map(|g| g["settings"].as_array().unwrap().len())
        .sum();
    assert_eq!(listed, r.descriptors().len());
    for group in Group::ALL {
        let present = r.descriptors().iter().any(|d| d.group == group);
        // MCP gets its own settings with the automation settings (#56); every other group
        // already has some.
        assert!(present, "{group:?} has no settings");
    }
    assert!(schema["generated_from"]["workers"]["integration"]["worker_sha256"].is_string());
}

#[test]
fn drift_names_unknown_and_missing_keys() {
    let r = registry();
    let names: Vec<&str> = r
        .descriptors()
        .iter()
        .filter(|d| d.owner == Owner::Azahar)
        .filter(|d| d.available_on(Some(Branch::Experimental)).is_ok() || true)
        .map(|d| d.key.as_str())
        .collect();
    assert!(r
        .drift(Branch::Experimental, names.iter().copied())
        .is_empty());
    // The CITnLO-only key is unknown to an AZAHAR registry view.
    let drift = r.drift(Branch::Integration, names.iter().copied());
    assert_eq!(drift.unknown_to_registry.len(), 1);
    assert!(drift.missing_from_worker.is_empty());
    let drift = r.drift(Branch::Experimental, ["volume", "brand_new"]);
    assert_eq!(drift.unknown_to_registry, ["brand_new"]);
    assert_eq!(drift.missing_from_worker.len(), 172);
}

#[test]
fn input_bindings_are_validated_by_the_layout_parser() {
    let r = registry();
    let ok = json!([{"device": "keyboard", "key": "z"}, {"device": "button", "button": "east"}]);
    for scope in [Scope::Global, Scope::Primary, Scope::SecondaryRuntime] {
        r.validate("lotus.input.bind.a", &ok, scope, None)
            .expect("bindings");
    }
    r.validate("lotus.input.bind.a", &json!([]), Scope::Global, None)
        .expect("unbind");
    r.validate("lotus.input.bind.a", &json!(null), Scope::Global, None)
        .expect("inherit");
    for bad in [
        json!({"device": "keyboard"}),
        json!([{"device": "wheel"}]),
        json!([{"device": "button", "button": "paddle"}]),
        json!(vec![json!({"device": "keyboard", "key": "z"}); 9]),
        json!([{"device": "keyboard", "key": "space"}]),
        json!([{"device": "keyboard", "key": "z"}, {"device": "keyboard", "key": "."}]),
        json!([{"device": "keyboard", "key": "tab"}]),
        json!([{"device": "keyboard", "key": "escape"}]),
        json!([{"device": "keyboard", "key": "control"}]),
    ] {
        assert!(
            r.validate("lotus.input.bind.a", &bad, Scope::Global, None)
                .is_err(),
            "{bad}"
        );
    }
    let ids: Vec<String> = lotus_input::layout::Action::ALL
        .iter()
        .map(|a| format!("lotus.input.bind.{}", a.id()))
        .collect();
    for id in &ids {
        assert_eq!(r.get(id).unwrap().owner, Owner::Input, "{id}");
    }
    assert!(r
        .validate(
            "lotus.input.circle_pad.deadzone",
            &json!(1.0),
            Scope::Global,
            None
        )
        .is_err());
    r.validate("lotus.input.threshold", &json!(0.5), Scope::Primary, None)
        .expect("threshold");
}
