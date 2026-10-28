//! The lease dog: [`dog`]
//!
//! Uses only `shep-kelpie-core`. The runner, the adapters and the verbs use this
//! one, and it names none of them.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

pub mod dog;

// The modules of `shep-kelpie-core` the dog uses, each kept under the name it had here
use shep_kelpie_core::{
    agent_board, agents, canonical, forwarder, home, ipc, lease, metrics, pacer, ports, review_bot,
    settings, shep_home, shepherd, skills, spawn, status, usage, webhook,
};

// The stand-ins the dog's tests use, from core and `test_support`, kept under the
// name the rig had here
#[cfg(test)]
mod test {
    pub use shep_kelpie_core::test::{
        EPOCH, FakeClock, FakeMeter, FakeShepherd, LeaseReads, RecordedSink, utilization,
    };
    pub use shep_kelpie_test_support::{CALL_PATIENCE, write_in, write_script};
}
