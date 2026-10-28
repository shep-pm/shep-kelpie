//! The real ports and the forge: [`adapters`] and [`github`]
//!
//! Uses only `shep-kelpie-core` and the leaf crates. The runner, the dog and the
//! verbs use this one, and it names none of them.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

pub mod adapters;
pub mod github;

// The modules of `shep-kelpie-core` the adapters use, each kept under the name it had here
use shep_kelpie_core::{
    board, bridge, browse, confine, dep_sources, fence, forwarder, guard, home, hooks, ipc, lease,
    metrics, ports, profile, random, review_bot, settings, skills, ticket, transcripts, webhook,
};

// What only the adapters' tests use of core
#[cfg(test)]
use shep_kelpie_core::{coderabbit, codex, cubic, local_paths, work_item};

// The leaf crates, each module kept under the name it had here
use shep_kelpie_facts::tool_error;
use shep_kelpie_owner_only::owner_only;
use shep_kelpie_process::{spawn, worktree};
// The live MCP tests start the code graph server kelpie's tools hold.
#[cfg(all(test, unix))]
use shep_kelpie_process::tools;
use shep_kelpie_tree::tree;
use shep_kelpie_trim::trim;

#[cfg(any(test, feature = "test-support"))]
pub mod test;
