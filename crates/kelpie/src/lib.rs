//! Kelpie, a shep dog that runs Claude Code workers from a planned work item
//! to a merged pull request
//!
//! Each project runs as a sheep, kelpie's project runner, under kelpie's own
//! shepherd. `CONTEXT.md` at the repo root holds the vocabulary, and
//! `docs/design-log.md` the design.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

pub mod adapters;
pub mod confine;
pub mod dog;
pub mod lease;
pub mod ports;
pub mod profile;
pub mod runner;
pub mod settings;
pub mod sheep;
pub mod state;
pub mod work_item;
pub mod worktree;

#[cfg(test)]
mod test;
