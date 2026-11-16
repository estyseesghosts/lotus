//! LOTUS desktop: a GPUI window over the shared session supervisor.
//!
//! The GPUI layer owns presentation only. It never reaches into a native
//! worker: it renders owned snapshots the controller publishes, and sends
//! commands the same supervisor methods MCP uses.

pub mod desktop_controller;
pub mod gauges;
pub mod inspection;
pub mod lotus_window;
pub mod screen_geometry;
pub mod session_tree;
pub mod theme;
