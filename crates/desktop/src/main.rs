//! Windowed LOTUS.
//!
//! `lotus-desktop [--mcp] [host.json]`. With `--mcp` the window also serves the
//! full MCP toolset on stdio over the same sessions, so an agent can launch
//! LOTUS as its MCP server while a person watches and uses the same window.
//! stdout then carries only MCP frames.
use gpui::{App, AppContext, Bounds, TitlebarOptions, WindowBounds, WindowOptions};
use lotus_desktop::desktop_controller::McpMode;
use lotus_desktop::lotus_window::LotusWindow;
use lotus_session::supervisor::HostConfig;

const USAGE: &str = "usage: lotus-desktop [--mcp] [host.json]";

fn main() {
    let mut mcp = McpMode::Off;
    let mut config_path = None;
    for argument in std::env::args_os().skip(1) {
        if argument == "--mcp" {
            mcp = McpMode::Stdio;
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
        serde_json::from_str::<HostConfig>(&text).unwrap_or_else(|error| {
            eprintln!("invalid host configuration: {error}");
            std::process::exit(2);
        })
    });
    gpui_platform::application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, gpui::size(gpui::px(1280.0), gpui::px(880.0)), cx);
        let handle = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("LOTUS".into()),
                        ..Default::default()
                    }),
                    window_min_size: Some(gpui::size(gpui::px(720.0), gpui::px(560.0))),
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| LotusWindow::new(config, mcp, window, cx)),
            )
            .expect("open LOTUS window");
        #[cfg(windows)]
        let _ = handle.update(cx, |_, window, _| ensure_visible(window));
        cx.activate(true);
    });
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
