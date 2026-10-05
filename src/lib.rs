//! Kelpie, a shep dog that runs Claude Code workers from a planned work item
//! to a merged pull request
//!
//! Each project runs as a sheep, kelpie's project runner, under kelpie's own
//! shepherd. `CONTEXT.md` at the repo root holds the vocabulary, and
//! `docs/design-log.md` the design.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

pub mod adapters;
pub mod agents;
pub mod board;
pub mod coderabbit;
pub mod codex;
pub mod confine;
pub mod cubic;
pub mod doctor;
pub mod dog;
pub mod fence;
pub mod flock;
pub mod forwarder;
pub mod guard;
pub mod home;
pub mod lease;
pub mod local_paths;
pub mod pacer;
pub mod ports;
pub mod profile;
pub mod review_bot;
pub mod runner;
pub mod schema;
pub mod settings;
pub mod sheep;
pub mod shep_home;
pub mod shepherd;
pub mod skills;
pub mod state;
pub mod tools;
pub mod totp;
mod trim;
pub mod upgrade;
pub mod webhook;
pub mod work_item;
pub mod worktree;

#[cfg(test)]
mod test;
