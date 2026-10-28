//! Kelpie's vocabulary, settings and ports, and the stand-ins that name only them
//!
//! Uses only the leaf crates below it. The runner, the dog, the adapters and the
//! binary use this one.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

pub mod agent_board;
pub mod agents;
pub mod board;
pub mod bridge;
pub mod browse;
pub mod coderabbit;
pub mod codex;
pub mod confine;
pub mod cubic;
pub mod dep_sources;
pub mod fence;
pub mod forge_ledger;
pub mod forwarder;
pub mod guard;
pub mod home;
pub mod hooks;
pub mod ipc;
pub mod lease;
pub mod local_paths;
pub mod local_tickets;
pub mod metrics;
pub mod pacer;
pub mod package_managers;
pub mod pm_answer;
pub mod ports;
pub mod profile;
pub mod review_bot;
pub mod settings;
pub mod shep_home;
pub mod shepherd;
pub mod skills;
pub mod state;
pub mod ticket;
pub mod totp;
pub mod transcripts;
pub mod usage;
pub mod watch;
pub mod webhook;
pub mod work_item;
pub mod work_kind;

// The leaf crates under `crates/`, each module kept under the name it had here
pub use shep_kelpie_facts::{mcp_use, request_use, tool_error};
pub use shep_kelpie_owner_only::owner_only;
pub use shep_kelpie_process::{spawn, status, terminal, tools, worktree};
pub use shep_kelpie_tree::{interrupt, tree};
pub use shep_kelpie_trim::trim;
pub use shep_kelpie_worker_folder::worker_folder;

#[cfg(test)]
mod parity;
#[cfg(any(test, feature = "test-support"))]
pub mod test;
