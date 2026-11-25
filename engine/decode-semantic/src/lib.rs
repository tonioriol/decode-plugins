//! Semantic code search for the command line and for MCP clients, over the `decode-semantic-core` engine.
//!
//! [`model`] fetches and verifies the embedding model, [`files`] lists the files of a folder worth indexing,
//! [`engine`] keeps one persisted index per folder up to date and searches them, [`mcp`] serves that search
//! over stdio, and [`serve`] serves one search over the same-named folders of many projects.

pub mod engine;
pub mod mcp;
pub mod model;
pub mod serve;

pub use decode_semantic_core::files;

use std::path::PathBuf;

/// Where the model and the indexes live: `$SEMBLE_RS_HOME`, else `$XDG_CACHE_HOME/semble-rs`, else
/// `~/.cache/semble-rs`.
pub fn default_storage() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("SEMBLE_RS_HOME") {
        return Some(PathBuf::from(home));
    }
    if let Some(cache) = std::env::var_os("XDG_CACHE_HOME") {
        return Some(PathBuf::from(cache).join("semble-rs"));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache").join("semble-rs"))
}
