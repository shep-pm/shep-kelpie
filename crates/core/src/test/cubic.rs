//! The rig's cubic, posting in the shapes it was recorded posting on
//! shep-pm/shep#637 and #1544

use super::coderabbit::FakeCodeRabbit;
use crate::cubic::{LIMIT, LOGIN};
use crate::ports::Timestamp;
use crate::review_bot::{CheckRun, Comment, Review, Thread};

/// When the rig's cubic says reviews resume: 29 November 2026 at 00:00 UTC,
/// the day shep-pm/shep#1544's check run named
pub const CUBIC_RESUMES: Timestamp = Timestamp(1_795_910_400);

impl FakeCodeRabbit {
    /// cubic says it started on pull request `number` at `at`
    pub fn cubic_start(&self, number: u64, at: u64) {
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
    pub fn cubic_review(&self, number: u64, head: &str, at: u64, findings: &[&str]) {
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
                    outdated: false,
                    path: "work.txt".into(),
                    line: Some(1),
                    body: format!(
                        "<!-- cubic:v=0 -->\n<!-- metadata:{{\"confidence\":8}} -->\n{text}\n\n\
                         <details>\n<summary>Prompt for AI agents</summary>\n</details>"
                    ),
                }));
        });
    }

    /// cubic refuses a summon on pull request `number` at `at`, posting no
    /// comment and completing its check run on the head instead, as it did on
    /// shep-pm/shep#1544
    pub fn cubic_refuse(&self, number: u64, at: u64) {
        self.post_as(number, LOGIN.rest, |seen| {
            seen.checks.push(CheckRun {
                title: LIMIT.into(),
                summary: "You've reached your plan's monthly review limit. Reviews \
                          resume on 29 November 2026 (in 25 days)."
                    .into(),
                conclusion: "neutral".into(),
                at: Timestamp(at),
            });
        });
    }
}
