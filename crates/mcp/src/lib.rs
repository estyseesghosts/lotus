//! Local MCP adapter for shared native session operations.
pub mod evidence;
mod screenshot;
pub mod server;
pub mod tool_arguments;
pub use server::{Frontend, LotusServer};
pub use tool_arguments::ValidateScenarioArgs;
