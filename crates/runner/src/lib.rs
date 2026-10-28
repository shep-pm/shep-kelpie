//! The project runner: [`runner`], and the rig its tests and the verbs' tests use
//!
//! Uses `shep-kelpie-core`, the leaf crates and `shep-kelpie-protocol`, so it
//! reaches the real world through core's port traits and names no adapter. The
//! rig, behind `test-support`, is the one place that does, for the real
//! `GpuCurl`, `LocalReviewer` and `AppVoice`.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

pub mod runner;

// The modules of `shep-kelpie-core` the runner uses, each kept under the name it had here
use shep_kelpie_core::{
    agent_board, agents, board, bridge, dep_sources, fence, forge_ledger, guard, home, hooks, ipc,
    lease, local_paths, local_tickets, metrics, owner_only, pacer, package_managers, pm_answer,
    ports, profile, review_bot, settings, skills, state, ticket, totp, transcripts, usage, webhook,
    work_item, work_kind,
};

// What only the runner's tests and the rig use of core
#[cfg(test)]
use shep_kelpie_core::confine;
#[cfg(any(test, feature = "test-support"))]
use shep_kelpie_core::{coderabbit, codex, cubic};

// The wire types the runner answers with, each module kept under its own name
use shep_kelpie_protocol::{reply, request, status};

// The leaf crates, each module kept under the name it had here
use shep_kelpie_facts::{mcp_use, request_use, tool_error};
use shep_kelpie_process::{spawn, terminal, worktree};
#[cfg(test)]
use shep_kelpie_trim::trim;
use shep_kelpie_worker_folder::worker_folder;

// What only the rig uses of the adapters, the one place the runner names them
#[cfg(any(test, feature = "test-support"))]
use shep_kelpie_adapters::{adapters, github};

#[cfg(test)]
mod github_tests;
#[cfg(any(test, feature = "test-support"))]
pub mod test;
