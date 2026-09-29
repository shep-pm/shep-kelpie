//! CodeRabbit's rounds: the review bot round with CodeRabbit's profile, run
//! through the runner's stand-ins

pub(super) mod tests;

pub(super) use super::review_bot::{ANSWER_WAIT, DONE_SETTLE, HEARD_WAIT, REVIEW_WAIT};
pub(super) use crate::coderabbit::{FULL_REVIEW, LABEL};
