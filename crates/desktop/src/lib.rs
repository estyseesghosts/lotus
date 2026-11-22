//! LOTUS desktop: a GPUI window over the shared session supervisor.
//!
//! The GPUI layer owns presentation only. It never reaches into a native
//! worker: it renders owned snapshots the controller publishes, and sends
//! commands the same supervisor methods MCP uses.

#[doc(hidden)]
pub mod debugger_coverage;
#[cfg(feature = "silvestris-ui")]
mod debugger_palette;
pub mod desktop_controller;
pub mod gauges;
pub mod icons;
pub mod input_routing;
pub mod inspection;
pub mod layout;
pub mod lotus_window;
mod pica_texture;
pub mod preferences;
pub mod presence;
pub mod screen_geometry;
pub mod session_tree;
pub mod settings_window;
pub mod theme;
pub mod tiling;
#[cfg(feature = "silvestris-ui")]
pub mod ui_framework;
pub mod ui_metrics;

#[cfg(windows)]
pub mod local_mcp;
#[cfg(feature = "silvestris-ui")]
pub mod navigation;
