//! The rig's Codex, posting in the shapes it was recorded posting on
//! shep-pm/shep-kelpie#234, and its review and refusal as its app documents them

use super::coderabbit::FakeCodeRabbit;
use crate::codex::LOGIN;
use crate::ports::Timestamp;
use crate::review_bot::{Comment, Reaction, Review, Thread};

impl FakeCodeRabbit {
    /// Codex reviews `head` of pull request `number` at `at`, opening a
    /// thread for each finding, written as `P2|Title|text`
    pub(crate) fn codex_review(&self, number: u64, head: &str, at: u64, findings: &[&str]) {
        self.post_as(number, LOGIN.rest, |seen| {
            seen.reviews.push(Review {
                commit: head.to_owned(),
                body: "### 💡 Codex Review\n\nHere are some automated review suggestions for \
                       this pull request."
                    .into(),
                at: Timestamp(at),
            });
            let first = seen.threads.len();
            seen.threads
                .extend(findings.iter().enumerate().map(|(i, finding)| {
                    let mut parts = finding.splitn(3, '|');
                    let (level, title, text) = (
                        parts.next().unwrap_or_default(),
                        parts.next().unwrap_or_default(),
                        parts.next().unwrap_or_default(),
                    );
                    Thread {
                        id: format!("PRRT_codex_{number}_{}", first + i),
                        resolved: false,
                        path: "work.txt".into(),
                        line: Some(1),
                        body: format!(
                            "**<sub><sub>![{level} Badge](https://img.shields.io/badge/{level}-orange?style=flat)</sub></sub>  {title}**\n\n\
                             {text}\n\nUseful? React with 👍 / 👎."
                        ),
                    }
                }));
        });
    }

    /// Codex answers a summon on pull request `number` at `at` the way it
    /// was recorded answering on shep-pm/shep-kelpie#234
    pub(crate) fn codex_start(&self, number: u64, at: u64) {
        self.post_as(number, LOGIN.rest, |seen| {
            seen.comments.push(Comment {
                body: "To use Codex here, [create an environment for this \
                       repo](https://chatgpt.com/codex/cloud/settings/environments)."
                    .into(),
                at: Timestamp(at),
            });
        });
    }

    /// Codex says it found nothing in `head` of pull request `number` at `at`
    pub(crate) fn codex_clean(&self, number: u64, head: &str, at: u64) {
        self.post_as(number, LOGIN.rest, |seen| {
            seen.comments.push(Comment {
                body: format!(
                    "Codex Review: Didn't find any major issues. Keep them coming!\n\n\
                     **Reviewed commit:** `{}`",
                    head.get(..10).unwrap_or(head)
                ),
                at: Timestamp(at),
            });
        });
    }

    /// Codex leaves its thumbs up on pull request `number` at `at`, as it did
    /// beside its clean review on shep-pm/shep-kelpie#234
    pub(crate) fn codex_thumbs_up(&self, number: u64, at: u64) {
        self.post_as(number, LOGIN.rest, |seen| {
            seen.reactions.push(Reaction {
                content: "THUMBS_UP".into(),
                at: Timestamp(at),
            });
        });
    }

    /// Codex refuses a summon on pull request `number` at `at`
    pub(crate) fn codex_refuse(&self, number: u64, at: u64) {
        self.post_as(number, LOGIN.rest, |seen| {
            seen.comments.push(Comment {
                body: "You have reached your Codex usage limits for code reviews. You can see \
                       your limits in the [Codex usage dashboard](https://chatgpt.com/codex/settings/usage)."
                    .into(),
                at: Timestamp(at),
            });
        });
    }
}
