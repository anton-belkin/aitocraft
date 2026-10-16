//! AItoCraft: one MCP server, configured once in Claude Desktop / Cowork and Claude Code, for every
//! document open in the ArtCraft Craft apps (GridCraft, WordCraft, DeckCraft). See PROTOCOL.md.

pub mod app;
pub mod channel;
pub mod http;
pub mod hub;
pub mod install;
pub mod registry;
pub mod stdio;
pub mod util;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;
