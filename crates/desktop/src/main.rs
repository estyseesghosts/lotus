//! Windowed LOTUS.
//!
//! On Windows `--mcp` bridges stdio to a shared desktop. `--mcp-host`
//! opens that desktop directly, and `--mcp-stdio` serves one client.
use gpui::{App, AppContext, Bounds};
#[cfg(not(feature = "silvestris-ui"))]
use gpui::{TitlebarOptions, WindowBounds, WindowOptions};
use lotus_desktop::desktop_controller::McpMode;
use lotus_desktop::lotus_window::LotusWindow;
use lotus_session::supervisor::{parse_asset_dir, HostConfig};

const USAGE: &str = "usage: lotus-desktop [--mcp | --mcp-host (Windows) | --mcp-stdio] [host.json] [--asset-dir handle=path ...] [--theme aqua-light|aqua-dark|geist-light|geist-dark (silvestris-ui)]";

#[cfg(windows)]
gpui::actions!(lotus, [QuitSharedHost]);

fn main() {
    let mut mcp = McpMode::Off;
    #[cfg(windows)]
    let mut bridge = false;
    let mut config_path = None;
    let mut asset_dirs = Vec::new();
    #[cfg(feature = "silvestris-ui")]
    let mut initial_theme = None;
    let mut arguments = std::env::args_os().skip(1);
    while let Some(argument) = arguments.next() {
        if argument == "--theme" {
            #[cfg(feature = "silvestris-ui")]
            {
                initial_theme = arguments.next().and_then(|value| {
                    lotus_desktop::ui_framework::parse_theme(&value.to_string_lossy())
                });
                if initial_theme.is_none() {
                    eprintln!("--theme expects a supported theme\n{USAGE}");
                    std::process::exit(2);
                }
            }
            #[cfg(not(feature = "silvestris-ui"))]
            {
                eprintln!("--theme requires a build with silvestris-ui");
                std::process::exit(2);
            }
        } else if argument == "--asset-dir" {
            let parsed = arguments
                .next()
                .ok_or_else(|| "--asset-dir expects handle=path".to_string())
                .and_then(|value| parse_asset_dir(&value.to_string_lossy()));
            match parsed {
                Ok(entry) => asset_dirs.push(entry),
                Err(error) => {
                    eprintln!("{error}\n{USAGE}");
                    std::process::exit(2);
                }
            }
        } else if argument == "--mcp-stdio" {
            require_first_mcp_mode(mcp);
            mcp = McpMode::Stdio;
        } else if argument == "--mcp" {
            require_first_mcp_mode(mcp);
            #[cfg(windows)]
            {
                bridge = true;
                mcp = McpMode::Shared;
            }
            #[cfg(not(windows))]
            {
                mcp = McpMode::Stdio;
            }
        } else if cfg!(windows) && argument == "--mcp-host" {
            #[cfg(windows)]
            {
                require_first_mcp_mode(mcp);
                mcp = McpMode::Shared;
            }
        } else if argument == "--help" || argument == "-h" {
            eprintln!("{USAGE}");
            return;
        } else if config_path.is_none() {
            config_path = Some(argument);
        } else {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
    let config = config_path.map(|path| {
        let text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            eprintln!(
                "cannot read host configuration {}: {error}",
                path.to_string_lossy()
            );
            std::process::exit(2);
        });
        let mut config = serde_json::from_str::<HostConfig>(&text).unwrap_or_else(|error| {
            eprintln!("invalid host configuration: {error}");
            std::process::exit(2);
        });
        config.asset_directories.extend(asset_dirs.iter().cloned());
        config
    });
    if config.is_none() && !asset_dirs.is_empty() {
        eprintln!("--asset-dir needs a host.json\n{USAGE}");
        std::process::exit(2);
    }
    #[cfg(windows)]
    let _host_lock = if mcp == McpMode::Shared || bridge {
        let Some(config) = config.as_ref() else {
            eprintln!("shared MCP needs a host.json");
            std::process::exit(2);
        };
        let endpoint =
            lotus_desktop::local_mcp::Endpoint::for_config(config).unwrap_or_else(shared_mcp_error);
        if bridge {
            let arguments: Vec<_> = std::env::args_os()
                .skip(1)
                .map(|argument| {
                    if argument == "--mcp" {
                        "--mcp-host".into()
                    } else {
                        argument
                    }
                })
                .collect();
            let executable = std::env::current_exe().unwrap_or_else(shared_mcp_error);
            endpoint
                .bridge(&executable, &arguments)
                .unwrap_or_else(shared_mcp_error);
            return;
        }
        let Some(lock) = endpoint.claim_host().unwrap_or_else(shared_mcp_error) else {
            eprintln!("LOTUS already has a shared desktop for this storage root. Open it from the taskbar.");
            return;
        };
        Some(lock)
    } else {
        None
    };
    gpui_platform::application().run(move |cx: &mut App| {
        #[cfg(windows)]
        if mcp == McpMode::Shared {
            cx.bind_keys([gpui::KeyBinding::new("ctrl-shift-q", QuitSharedHost, None)]);
            cx.on_action(|_: &QuitSharedHost, cx| cx.quit());
        }
        #[cfg(feature = "silvestris-ui")]
        if let Err(error) = lotus_desktop::ui_framework::initialize(initial_theme, cx) {
            eprintln!("cannot initialize Silvestris: {error}");
            cx.quit();
            return;
        }
        let bounds = Bounds::centered(None, gpui::size(gpui::px(1280.0), gpui::px(880.0)), cx);
        let options = {
            #[cfg(feature = "silvestris-ui")]
            {
                silvestris::chrome::window_options(
                    "LOTUS",
                    bounds,
                    gpui::size(gpui::px(720.), gpui::px(560.)),
                    cx,
                )
            }
            #[cfg(not(feature = "silvestris-ui"))]
            {
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("LOTUS".into()),
                        ..Default::default()
                    }),
                    window_min_size: Some(gpui::size(gpui::px(720.0), gpui::px(560.0))),
                    ..Default::default()
                }
            }
        };
        let build = |window: &mut gpui::Window, cx: &mut App| {
            cx.new(|cx| LotusWindow::new(config, mcp, window, cx))
        };
        #[cfg(feature = "silvestris-ui")]
        #[cfg_attr(not(windows), allow(unused_variables))]
        let handle =
            silvestris::chrome::open_window(options, cx, build).expect("open LOTUS window");
        #[cfg(not(feature = "silvestris-ui"))]
        #[cfg_attr(not(windows), allow(unused_variables))]
        let handle = cx.open_window(options, build).expect("open LOTUS window");
        #[cfg(windows)]
        let _ = handle.update(cx, |_, window, _| ensure_visible(window));
        cx.activate(true);
    });
}

fn require_first_mcp_mode(mode: McpMode) {
    if mode != McpMode::Off {
        eprintln!("Choose one MCP mode.\n{USAGE}");
        std::process::exit(2);
    }
}

#[cfg(windows)]
fn shared_mcp_error<T>(error: std::io::Error) -> T {
    eprintln!("LOTUS shared MCP: {error}");
    std::process::exit(1);
}

/// Show the window if the launching process hid it.
///
/// A parent that sets `STARTF_USESHOWWINDOW` with `SW_HIDE` (Node's `windowsHide`, used by
/// MCP clients spawning stdio servers) makes Windows apply `SW_HIDE` to the process's first
/// show request, which here is GPUI's `SetWindowPlacement` in `WindowsWindow::new`
/// (gpui_windows `window.rs`). A later `ShowWindow` is honoured (tests/test_window_visible.py).
#[cfg(windows)]
fn ensure_visible(window: &gpui::Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::{
        Foundation::HWND,
        UI::WindowsAndMessaging::{IsWindowVisible, ShowWindow, SW_SHOWNORMAL},
    };
    // gpui::Window has an inherent `window_handle()`; call the raw-window-handle trait.
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(win32) = handle.as_raw() else {
        return;
    };
    let hwnd = HWND(win32.hwnd.get() as *mut core::ffi::c_void);
    // SAFETY: `hwnd` is the live top-level window GPUI just created on this thread.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        }
    }
}
