//! The wire types a runner speaks, shared by `shep trigger` and its socket
//!
//! A [`Request`] is one action read; a [`Reply`] is the body the runner answers
//! with, which serialises as what `shep trigger` returns today. [`Status`] and
//! its mirrors are the owned form of what the runner's loop posts, so a reader
//! can hold one. [`frame`] is one line of either way.
//!
//! Uses only `shep-kelpie-core`, `serde` and `serde_json`. The runner builds
//! these from its own borrowed types in `runner::trigger::wire`.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

pub mod frame;
pub mod reply;
pub mod request;
pub mod status;

pub use reply::{Event, Reply};
pub use request::{Request, RuleAction, RuleAnswer};
pub use status::Status;
