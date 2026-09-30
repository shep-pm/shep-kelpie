//! The rig's cubic, posting in the shapes it was recorded posting on
//! shep-pm/shep#637

use super::coderabbit::FakeCodeRabbit;
use crate::cubic::LOGIN;
use crate::ports::Timestamp;
use crate::review_bot::{Comment, Review, Thread};

impl FakeCodeRabbit {
    /// cubic says it started on pull request `number` at `at`
    pub(crate) fn cubic_start(&self, number: u64, at: u64) {
        self.post_as(number, LOGIN.rest, |seen| {
            seen.comments.push(Comment {
                body: "> @cubic-dev-ai review\n\n@maintainer I have started the AI code \
                       review. It will take a few minutes to complete."
                    .into(),
                at: Timestamp(at),
            });
        });
    }

    /// cubic reviews `head` of pull request `number` at `at`, opening a
    /// thread for each finding, written as `P2: text`
    pub(crate) fn cubic_review(&self, number: u64, head: &str, at: u64, findings: &[&str]) {
        self.post_as(number, LOGIN.rest, |seen| {
            let found = match findings.len() {
                0 => "**No issues found**".to_owned(),
                n => format!("**{n} issues found**"),
            };
            seen.reviews.push(Review {
                commit: head.to_owned(),
                body: format!(
                    "<!-- cubic:review-summary:start -->\n{found} across 1 file\n\
                     <!-- cubic:review-summary:end -->"
                ),
                at: Timestamp(at),
            });
            let first = seen.threads.len();
            seen.threads
                .extend(findings.iter().enumerate().map(|(i, text)| Thread {
                    id: format!("PRRT_cubic_{number}_{}", first + i),
                    resolved: false,
                    path: "work.txt".into(),
                    line: Some(1),
                    body: format!(
                        "<!-- cubic:v=0 -->\n<!-- metadata:{{\"confidence\":8}} -->\n{text}\n\n\
                         <details>\n<summary>Prompt for AI agents</summary>\n</details>"
                    ),
                }));
        });
    }

    /// cubic refuses a summon on pull request `number` at `at`. Its real
    /// wording was never recorded.
    pub(crate) fn cubic_refuse(&self, number: u64, at: u64) {
        self.post_as(number, LOGIN.rest, |seen| {
            seen.comments.push(Comment {
                body: "You have reached the monthly reviewed-line limit.".into(),
                at: Timestamp(at),
            });
        });
    }
}
