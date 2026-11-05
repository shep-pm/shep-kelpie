//! The lease dog: [`dog`]
//!
//! Uses only `shep-kelpie-core`. The runner, the adapters and the verbs use this
//! one, and it names none of them.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

pub mod dog;

// The modules of `shep-kelpie-core` the dog uses, each kept under the name it had here
use shep_kelpie_core::{
    agent_board, agents, api_refusal, board, canonical, forwarder, home, ipc, lease, metrics,
    pacer, ports, review_bot, settings, shep_home, shepherd, skills, spawn, state, status, usage,
    webhook,
};

// The stand-ins the dog's tests use, from core and `test_support`, kept under the
// name the rig had here
#[cfg(test)]
mod test {
    pub use shep_kelpie_core::test::{
        EPOCH, FakeClock, FakeMeter, FakeShepherd, LeaseReads, RecordedSink, utilization,
    };
    pub use shep_kelpie_test_support::{CALL_PATIENCE, write_in, write_script};

    /// The agents the dog's tests have run on since kelpie stopped shipping any that names a
    /// model: `sonnet-high` and `opus-high`, two Claude Code implementers, in a folder of their own
    pub fn claude_implementers() -> (tempfile::TempDir, crate::agents::Agents) {
        let dir = tempfile::tempdir().unwrap();
        for (name, model, tier) in [
            ("sonnet-high", "claude-sonnet-5-5", "standard"),
            ("opus-high", "claude-opus-5-5", "heavy"),
        ] {
            let text = format!(
                "---\nrole: implementer\nharness: claude-code\nmodel: {model}\neffort: high\n\
                 tier: {tier}\n---\n"
            );
            write_in(dir.path(), &format!("{name}.md"), &text);
        }
        let agents = crate::agents::Agents::load(dir.path()).unwrap();
        (dir, agents)
    }
}
