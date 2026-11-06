//! LightCraft's MCP server.
//!
//! [Model Context Protocol](https://modelcontextprotocol.io) over stdio: newline-delimited
//! JSON-RPC 2.0, hand-written (no async runtime). The server exposes LightCraft as MCP tools and
//! resources and forwards everything to a [`Backend`]:
//!
//! - [`Remote`] talks to a running desktop app through its loopback JSON-lines control channel
//!   (`lightcraft --control 7980`): one `{"id","method","params"}` line in, one
//!   `{"id","ok","result"|"error"}` line out (see `docs/control-protocol.md`).
//! - [`Headless`] hosts an in-process [`lightcraft_engine::Session`] and answers the same
//!   control-channel method names itself (rendering with the engine's pipeline and encoding with
//!   `lightcraft-codecs`), so agents can develop photos and look at the result without a window.
//!
//! Entry points: [`Server::serve_stdio`] (process stdin), [`Server::serve`] (caller-managed
//! streams), and [`Server::handle_line`] (one message).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod backend;
mod base64;
mod headless;
mod server;
mod tools;

pub use backend::{Backend, ProgressHook, Remote};
pub use base64::{base64_decode, base64_encode};
pub use headless::{Headless, encode_image, expand_paths, write_image};
pub use server::{PROTOCOL_VERSION, Server};
pub use tools::{COMMAND_TOOL_PREFIX, ToolResult, call_tool, command_tool_name, helper_tools, tool_definitions};

/// Default control-channel address of the desktop app (`lightcraft --control 7980`).
pub const DEFAULT_ADDR: &str = "127.0.0.1:7980";

#[cfg(test)]
mod tests;

/// Held by tests that start a child process (a copy of the test binary) and by tests that run a
/// loopback socket server, so the two never overlap. On Windows the child can inherit a socket the
/// server just accepted; when the child exits, Windows closes its copy abortively and the test's
/// connection is reset ("An existing connection was forcibly closed"), which failed the
/// `backend` reply tests whenever they ran alongside a child-process test.
#[cfg(test)]
pub(crate) fn serial_sockets_and_children() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
