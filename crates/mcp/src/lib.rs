//! Local MCP adapter for shared native session operations.
pub mod server;
pub mod tool_arguments;
pub use server::{Frontend, LotusServer};
pub use tool_arguments::ValidateScenarioArgs;
