//! LOTUS desktop: a GPUI window over the shared session supervisor.
//!
//! The GPUI layer owns presentation only. It never reaches into a native
//! worker: it renders owned snapshots the controller publishes, and sends
//! commands the same supervisor methods MCP uses.

pub mod desktop_controller;
pub mod gauges;
pub mod icons;
pub mod input_routing;
pub mod inspection;
pub mod layout;
pub mod lotus_window;
pub mod preferences;
pub mod presence;
pub mod screen_geometry;
pub mod session_tree;
pub mod settings_window;
pub mod theme;
pub mod tiling;
pub mod ui_metrics;
