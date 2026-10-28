//! Kelpie, a shep dog that runs Claude Code workers from a planned work item
//! to a merged pull request
//!
//! Each project runs as a sheep, kelpie's project runner, under kelpie's own
//! shepherd. `GLOSSARY.md` at the repo root holds the vocabulary, and
//! `docs/design/` the design, read from its README.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

pub mod agent_verbs;
pub mod doctor;
pub mod flock;
pub mod issues;
pub mod schema;
pub mod sheep;
pub mod upgrade;

// The modules of `shep-kelpie-core`, each kept under the name it had here
pub use shep_kelpie_core::{
    agent_board, agents, board, bridge, browse, coderabbit, codex, confine, cubic, fence,
    forge_ledger, forwarder, guard, home, hooks, ipc, lease, local_paths, local_tickets, metrics,
    pacer, package_managers, pm_answer, ports, profile, review_bot, settings, shep_home, shepherd,
    skills, state, ticket, totp, usage, watch, webhook, work_item, work_kind,
};

// The real ports and the forge, each kept under the name it had here
pub use shep_kelpie_adapters::{adapters, github};

// The dog, kept under the name it had here
pub use shep_kelpie_dog::dog;

// The runner, kept under the name it had here
pub use shep_kelpie_runner::runner;

// The leaf crates under `crates/`, each module kept under the name it had here
pub use shep_kelpie_disk::disk;
pub use shep_kelpie_facts::{mcp_use, request_use, tool_error};
pub use shep_kelpie_process::{spawn, status, terminal, tools, workflows, worktree};
pub use shep_kelpie_tree::{interrupt, tree};
pub use shep_kelpie_trim::trim;
pub use shep_kelpie_worker_folder::worker_folder;

// The rig, gathered from the crates that own its parts
#[cfg(test)]
mod test;

#[cfg(test)]
mod tests {
    // `tool_error` is in a crate whose own name is not the binary's, so it holds the
    // binary's name as a constant, and this keeps the two the same.
    #[test]
    fn the_program_tool_error_reads_as_kelpies_own_is_this_binary() {
        assert_eq!(
            shep_kelpie_facts::tool_error::KELPIE_PROGRAM,
            env!("CARGO_PKG_NAME")
        );
    }
}
