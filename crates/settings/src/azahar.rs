//! Azahar descriptors from the generated inventory plus Lotus's grouping and exclusions.
//!
//! Exclusions are derived from the inventory's consumer citations where possible, so a pin
//! that gives a setting a real consumer makes it available again without an edit here.

use serde::Deserialize;
use serde_json::Value;

use crate::{Apply, Availability, Branch, Descriptor, Group, Kind, Owner, Scopes};

const INVENTORY: &str = include_str!("../azahar-inventory.json");

/// `BindSetting` refuses longer strings (`ctr_worker/settings_bindings.h`).
const MAX_STRING: usize = 4096;
/// `BindRaw` refuses larger JSON values (`ctr_worker/settings_bindings.h`).
const MAX_JSON: usize = 8192;
/// The generator keeps at most this many consumer citations per setting
/// (`tools/generate-settings.py`), so a full list may hide further consumers.
const CITATIONS_KEPT: usize = 6;

#[derive(Deserialize)]
struct Inventory {
    pins: Value,
    sources: Value,
    settings: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    default: Value,
    range: Value,
    choices: Value,
    apply: std::collections::BTreeMap<String, String>,
    consumer_sources: String,
}

pub(crate) fn descriptors() -> (Vec<Descriptor>, Value) {
    let inventory: Inventory =
        serde_json::from_str(INVENTORY).expect("azahar-inventory.json is generated valid JSON");
    let descriptors = inventory.settings.iter().map(descriptor).collect();
    (
        descriptors,
        serde_json::json!({"pins": inventory.pins, "workers": inventory.sources}),
    )
}

fn descriptor(entry: &Entry) -> Descriptor {
    let sources: Vec<String> = entry
        .consumer_sources
        .split(';')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let classes: Vec<Apply> = entry
        .apply
        .values()
        .filter_map(|c| Apply::parse(c))
        .collect();
    let apply = classes.first().copied().unwrap_or(Apply::Inert);
    let availability = match exclusion(&entry.name, apply, &sources) {
        Some(reason) => Availability::Excluded { reason },
        None => match (
            entry.apply.contains_key("integration"),
            entry.apply.contains_key("experimental"),
        ) {
            (true, false) => Availability::BranchOnly {
                branch: Branch::Integration,
            },
            (false, true) => Availability::BranchOnly {
                branch: Branch::Experimental,
            },
            _ => Availability::Available,
        },
    };
    let (group, subgroup) = group(&entry.name).unwrap_or((Group::Advanced, "Unclassified"));
    Descriptor {
        key: entry.name.clone(),
        owner: Owner::Azahar,
        group,
        subgroup,
        label: label(&entry.name),
        description: description(apply, &sources),
        kind: kind(entry),
        default: entry.default.clone(),
        scopes: if matches!(availability, Availability::Excluded { .. }) {
            Scopes::NONE
        } else {
            Scopes::ALL
        },
        apply,
        availability,
        sources,
        keywords: Vec::new(),
    }
}

fn kind(entry: &Entry) -> Kind {
    let bound = |key: &str| entry.range.get(key).cloned().unwrap_or(Value::Null);
    match entry.kind.as_str() {
        "boolean" => Kind::Boolean,
        "integer" => Kind::Integer {
            min: bound("min").as_i64(),
            max: bound("max").as_i64(),
            choices: entry
                .choices
                .as_array()
                .map(|c| c.iter().filter_map(Value::as_i64).collect()),
            nullable: false,
        },
        "float" => Kind::Float {
            min: bound("min").as_f64(),
            max: bound("max").as_f64(),
        },
        "string" if entry.name.starts_with("cfg.") => Kind::String {
            max_len: MAX_STRING,
            choices: None,
            nullable: false,
            // `cfg-savedata` bindings take exact-length lowercase hex (`ctr_worker/debug.h`).
            exact_len: entry.default.as_str().map(str::len),
            hex: true,
        },
        "string" => Kind::String {
            max_len: MAX_STRING,
            choices: None,
            nullable: false,
            exact_len: None,
            hex: false,
        },
        _ => Kind::Json {
            max_bytes: MAX_JSON,
        },
    }
}

/// Why a setting is not offered, or `None`.
fn exclusion(name: &str, apply: Apply, sources: &[String]) -> Option<String> {
    let locked = match name {
        "use_gdbstub" => Some(
            "the worker refuses to enable Azahar's GDB stub (ctr_worker/debug.h settings_set); \
             Lotus serves GDB itself (crates/gdb)",
        ),
        "gdbstub_port" => {
            Some("read only with use_gdbstub on (core.cpp), which the worker refuses")
        }
        "enable_rpc_server" => Some(
            "the worker refuses to start Azahar's UDP RPC server (ctr_worker/debug.h settings_set)",
        ),
        "use_custom_storage" => Some(
            "the worker refuses custom storage paths; each session has its own root \
             (ctr_worker/debug.h settings_set)",
        ),
        "output_type" => Some(
            "worker audio always uses its bounded sink, output_type 6 \
             (ctr_worker/debug.h settings_set)",
        ),
        _ => None,
    };
    if let Some(reason) = locked {
        return Some(reason.to_string());
    }
    if apply == Apply::Inert {
        return Some(
            "no consumer in core, video_core or audio_core at the pinned source (apply class inert)"
                .into(),
        );
    }
    let complete = !sources.is_empty() && sources.len() < CITATIONS_KEPT;
    if complete && sources.iter().all(|s| s.contains("/renderer_opengl/")) {
        return Some(
            "read only by the OpenGL renderer; workers run the software or Vulkan renderer \
             (ctr_worker/debug.h graphics_api)"
                .into(),
        );
    }
    if complete
        && sources
            .iter()
            .all(|s| s.starts_with("src/core/frontend/") || s.contains("/renderer_opengl/"))
    {
        return Some(
            "Azahar window layout; the worker captures each screen with its own single-screen \
             layout and 3D off (ctr_worker/frames.h ScreenLayout)"
                .into(),
        );
    }
    None
}

/// Group and subgroup by intent. `None` leaves a setting unclassified, which the tests refuse.
fn group(name: &str) -> Option<(Group, &'static str)> {
    use Group::*;
    if name.starts_with("cfg.") {
        return Some((Emulation, "System settings (CFG savedata)"));
    }
    Some(match name {
        "use_cpu_jit" | "use_fastinterp" | "cpu_clock_percentage" => (Emulation, "CPU"),
        "is_new_3ds"
        | "region_value"
        | "lle_applets"
        | "lle_modules"
        | "enable_required_online_lle_modules"
        | "delay_start_for_lle_modules"
        | "toggle_unique_data_console_type"
        | "apply_region_free_patch"
        | "plugin_loader_enabled"
        | "allow_plugin_loader"
        | "use_artic_base_controller"
        | "enable_exception_handler" => (Emulation, "System"),
        "init_clock"
        | "init_time"
        | "init_time_offset"
        | "init_ticks_type"
        | "init_ticks_override" => (Emulation, "Clock"),
        "frame_limit" | "turbo_limit" => (Emulation, "Speed"),
        "use_virtual_sd"
        | "compress_cia_installs"
        | "async_fs_operations"
        | "deterministic_async_operations" => (SessionsStorage, "Guest storage"),
        "graphics_api"
        | "physical_device"
        | "use_gles"
        | "use_hw_shader"
        | "shaders_accurate_mul"
        | "use_shader_jit"
        | "resolution_factor"
        | "antialiasing"
        | "texture_filter"
        | "texture_sampling"
        | "filter_mode"
        | "use_disk_shader_cache"
        | "async_shader_compilation"
        | "async_presentation"
        | "spirv_shader_gen"
        | "disable_spirv_optimizer"
        | "use_vsync"
        | "use_display_refresh_rate_detection"
        | "use_limit_frames_ahead_of_gpu"
        | "use_skip_duplicate_frames"
        | "disable_right_eye_render"
        | "simulate_3ds_gpu_timings"
        | "delay_game_render_thread_us"
        | "pp_shader_name" => (GraphicsDisplay, "Renderer"),
        "dump_textures" | "custom_textures" | "preload_textures" | "async_custom_loading" => {
            (GraphicsDisplay, "Custom textures")
        }
        "factor_3d"
        | "render_3d"
        | "swap_eyes_3d"
        | "render_3d_which_display"
        | "mono_render_option"
        | "anaglyph_shader_name"
        | "cardboard_screen_size"
        | "cardboard_x_shift"
        | "cardboard_y_shift" => (GraphicsDisplay, "Stereoscopic 3D"),
        "layout_option"
        | "swap_screen"
        | "upright_screen"
        | "secondary_display_layout"
        | "layouts_to_cycle"
        | "large_screen_proportion"
        | "screen_gap"
        | "small_screen_position"
        | "use_integer_scaling"
        | "aspect_ratio"
        | "bg_red"
        | "bg_green"
        | "bg_blue"
        | "portrait_layout_option"
        | "custom_second_layer_opacity" => (GraphicsDisplay, "Screen layout"),
        name if name.starts_with("custom_")
            || name.starts_with("screen_top_")
            || name.starts_with("screen_bottom_") =>
        {
            (GraphicsDisplay, "Screen layout")
        }
        "audio_emulation"
        | "enable_audio_stretching"
        | "enable_realtime_audio"
        | "volume"
        | "audio_muted"
        | "output_type"
        | "output_device"
        | "bypass_audio_effects"
        | "simulate_headphones_plugged" => (AudioDevices, "Audio"),
        "input_type" | "input_device" => (AudioDevices, "Microphone"),
        "camera_name" | "camera_config" | "camera_flip" => (AudioDevices, "Camera"),
        "battery_state_source" | "battery_charging" | "battery_level" | "steps_per_hour" => {
            (AudioDevices, "Battery and pedometer")
        }
        "current_input_profile"
        | "current_input_profile_index"
        | "input_profiles"
        | "touch_from_button_maps" => (Controls, "Azahar input profiles"),
        "renderer_debug"
        | "pica_debugging"
        | "dump_command_buffers"
        | "record_frame_times"
        | "instant_debug_log"
        | "log_filter"
        | "log_regex_filter"
        | "use_gdbstub"
        | "gdbstub_port" => (DebuggingTracing, "Emulator diagnostics"),
        "output_format"
        | "format_options"
        | "video_encoder"
        | "video_encoder_options"
        | "video_bitrate"
        | "audio_encoder"
        | "audio_encoder_options"
        | "audio_bitrate" => (Advanced, "Video dumping"),
        "enable_rpc_server" | "web_api_url" | "network_token" | "use_custom_storage"
        | "enable_gamemode" => (Advanced, "Azahar host integration"),
        _ => return None,
    })
}

fn label(name: &str) -> String {
    if let Some(block) = name.strip_prefix("cfg.") {
        return format!("CFG block 0x{}", block.to_ascii_uppercase());
    }
    let words: Vec<String> = name
        .split('_')
        .map(|word| match word {
            "cpu" => "CPU".into(),
            "jit" => "JIT".into(),
            "lle" => "LLE".into(),
            "gpu" => "GPU".into(),
            "sd" => "SD".into(),
            "cia" => "CIA".into(),
            "3ds" => "3DS".into(),
            "3d" => "3D".into(),
            "api" => "API".into(),
            "hw" => "hardware".into(),
            "spirv" => "SPIR-V".into(),
            "gles" => "GLES".into(),
            "pp" => "post-processing".into(),
            "bg" => "background".into(),
            "rpc" => "RPC".into(),
            "gdbstub" => "GDB stub".into(),
            "fs" => "FS".into(),
            "us" => "(µs)".into(),
            "fastinterp" => "fast interpreter".into(),
            "vsync" => "VSync".into(),
            "url" => "URL".into(),
            other => other.to_string(),
        })
        .collect();
    let mut text = words.join(" ");
    if let Some(first) = text.get(0..1) {
        text = first.to_uppercase() + &text[1..];
    }
    text
}

fn description(apply: Apply, sources: &[String]) -> String {
    let when = match apply {
        Apply::Live => "Applies to a running session",
        Apply::Renderer => "Takes effect when the session's renderer is created",
        Apply::Boot => "Takes effect when content is loaded",
        Apply::CfgSavedata => "Written to the emulated console's CFG savedata before loading",
        _ => "Not read by the emulator",
    };
    match sources.first() {
        Some(source) => format!("{when}. Read by {source}."),
        None => format!("{when}."),
    }
}
