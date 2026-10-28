//! The processes kelpie starts: [`spawn`], the [`terminal`], [`tools`] and git
//! [`worktree`]s, and how one ended: [`status`]
//!
//! Uses no other member of the workspace.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

mod canonical;
pub mod spawn;
pub mod status;
pub mod terminal;
pub mod tools;
pub mod workflows;
pub mod worktree;

pub use canonical::canonical;
