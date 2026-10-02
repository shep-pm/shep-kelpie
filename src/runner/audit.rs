//! The whole-issue check before the merge ruling, or before an `auto` merge
//!
//! The review rounds read the diff line by line. This is one fresh session
//! that reads the work as a whole: the issue with whatever its body points
//! to, the pull request's body and the final diff. It answers whether each
//! acceptance criterion is met and where, and what the change assumes about
//! the world outside the repo, and whether the code or a test checks each
//! assumption against the real thing rather than a fake that accepts
//! anything. A criterion not met or an assumption not checked sends the
//! worker back with the gap named, the way a held finding does. After
//! [`SENDS_BACK`] such trips the next gap goes to the maintainer.

use std::collections::BTreeSet;

use serde::Deserialize;

use super::Runner;
use super::gate::short;
use super::report::{Begin, StepReport};
use super::review::calls::{build_call, diff_against};
use crate::pacer::Scope;
use crate::ports::{AgentCall, AgentError, AgentReply, Role, Tools};
use crate::state::{RulingKind, StateError};
use crate::work_item::{Audit, CallKind, Phase, SENDS_BACK, Turn};
use crate::worktree;

#[cfg(test)]
mod tests;

/// Where the check's throwaway settings go
const SETTINGS_FILE: &str = "audit-settings.json";

/// The most of the issue, or of one item it points to, the check's prompt
/// carries, in bytes
const MOST: usize = 20_000;

/// The most items an issue's body may point to that are pulled in
const POINTED_AT: usize = 8;

impl Runner {
    // Whether `head`, which CI passed, goes on to the merge ruling: the
    // check's call when it has not looked at this head yet.
    pub(super) fn audit_before_merge(
        &mut self,
        number: u64,
        head: &str,
    ) -> Result<Option<Begin>, StateError> {
        let item = self.current().expect("the check is of a work item");
        if item.audit.as_ref().and_then(|a| a.passed.as_deref()) == Some(head) {
            return Ok(None);
        }
        let issue = item.issue;
        let limit = self.agents.limits.auditor.clone();
        if let Some(held) = self.pace(Scope::Turn, &limit)?.holds() {
            return Ok(Some(held));
        }
        let call = match self.audit_call(issue, number) {
            Ok(call) => call,
            Err(reason) => return Ok(Some(self.gate_failed(reason))),
        };
        let since = self.ports.clock.now();
        self.update(|item| item.call_started(CallKind::Audit, since))?;
        Ok(Some(Begin::Audit(Box::new(call), head.to_owned())))
    }

    fn audit_call(&self, issue: u64, number: u64) -> Result<AgentCall, String> {
        let item = self.current().expect("the check is of a work item");
        let forge = &self.ports.forge;
        let repo = &self.settings.forge;
        let found = forge
            .issue(repo, issue)
            .map_err(|e| format!("cannot read issue #{issue} for the whole-issue check: {e}"))?;
        let pull = forge
            .reviewed(repo, number)
            .map_err(|e| format!("cannot read #{number} for the whole-issue check: {e}"))?;
        let pointed = pointed_at(&found.body, &[issue, number])
            .into_iter()
            .map(|n| self.pulled_in(n))
            .collect::<Vec<_>>()
            .join("\n\n");
        let base = format!("origin/{}", worktree::BASE);
        let diff = diff_against(&item.worktree, &base)?;
        let prompt = prompt(&Reading {
            issue,
            title: &found.title,
            body: &found.body,
            pointed: &pointed,
            number,
            pull_title: &pull.title,
            pull_body: &pull.body,
            base: &base,
            diff: &diff,
        });
        let model = &self.agents.auditor;
        let limit = &self.agents.limits.auditor;
        let mut call = build_call(Role::Auditor, issue, &item.worktree, (model, limit), prompt)?;
        call.settings = self.paths.worker.join(SETTINGS_FILE);
        // It reads the worktree to see what the code and its tests check,
        // and runs no command.
        call.tools = Tools::Review;
        self.prepared(call)
    }

    // What `n`, which the issue's body points to, says: an issue's title and
    // body, or a pull request's with the review comments still unresolved on
    // it. Said so when it cannot be read, for the check to count against.
    fn pulled_in(&self, n: u64) -> String {
        let forge = &self.ports.forge;
        let repo = &self.settings.forge;
        if let Ok(found) = forge.issue(repo, n) {
            let said = format!("#{n} {}\n\n{}", found.title.trim(), found.body.trim());
            return cut(&said, MOST);
        }
        match forge.reviewed(repo, n) {
            Ok(pull) => {
                let mut said = format!("#{n} {}\n\n{}", pull.title.trim(), pull.body.trim());
                let comments = pull.review.iter().flat_map(|r| &r.comments);
                for comment in comments {
                    let line = comment.line.map(|l| format!(":{l}")).unwrap_or_default();
                    said.push_str(&format!(
                        "\n\nreview comment on {}{line}: {}",
                        comment.file,
                        comment.body.trim()
                    ));
                }
                cut(&said, MOST)
            }
            Err(e) => format!("#{n} could not be read: {e}"),
        }
    }

    /// Records what the check found of `head`, once its call ends
    pub(super) fn end_audit(
        &mut self,
        head: &str,
        result: Result<AgentReply, AgentError>,
    ) -> Result<Option<StepReport>, StateError> {
        let spent = result
            .as_ref()
            .ok()
            .map(|reply| super::report::Spent::Claude {
                role: Role::Auditor,
                session: reply.session_id.clone(),
                usage: reply.usage,
                session_cost: reply.session_cost,
            });
        if matches!(result, Err(AgentError::Stopped)) {
            return Ok(None);
        }
        let now = self.ports.clock.now();
        let Some(item) = self.current() else {
            return Ok(None);
        };
        let (issue, number) = (item.issue, item.pull_request);
        let Some(number) = number else {
            return Ok(None);
        };
        self.update(|item| super::review::record_spent(item, spent, now))?;
        let read = result
            .map_err(|e| e.to_string())
            .and_then(|reply| read_findings(&reply.text));
        let findings = match read {
            Ok(findings) => findings,
            Err(reason) => {
                let reason = format!("the whole-issue check on #{number} failed: {reason}");
                return Ok(Some(StepReport::GateFailed { issue, reason }));
            }
        };
        let gaps = findings.gaps();
        let head_now = head.to_owned();
        if gaps.is_empty() {
            self.update(|item| {
                let audit = item.audit.get_or_insert_with(Audit::default);
                audit.passed = Some(head_now);
                // A later head's gaps get the worker's trips afresh.
                audit.sent_back = 0;
            })?;
            return Ok(Some(StepReport::AuditPassed {
                issue,
                pull_request: number,
                head: head.to_owned(),
                criteria: findings.criteria.len(),
                assumptions: findings.assumptions.len(),
            }));
        }
        let prompt = gap_prompt(number, head, &gaps);
        let sent = item_sent_back(self);
        if sent >= SENDS_BACK {
            self.update(|item| {
                item.audit.get_or_insert_with(Audit::default).sent_back = 0;
            })?;
            let kind = RulingKind::Audit {
                head: head.to_owned(),
                gaps,
                prompt,
            };
            return match self.raise(number, kind)? {
                Begin::Report(report) => Ok(Some(report)),
                _ => unreachable!("a ruling is raised as a report"),
            };
        }
        self.update(|item| {
            item.audit.get_or_insert_with(Audit::default).sent_back = sent + 1;
            item.turn = Turn::Next { prompt };
            item.phase = Phase::Implement;
        })?;
        Ok(Some(StepReport::AuditSentBack {
            issue,
            pull_request: number,
            head: head.to_owned(),
            gaps,
        }))
    }
}

fn item_sent_back(runner: &Runner) -> u32 {
    let item = runner.current().expect("the check is of a work item");
    item.audit.as_ref().map_or(0, |a| a.sent_back)
}

/// What the check read, for its prompt
struct Reading<'a> {
    issue: u64,
    title: &'a str,
    body: &'a str,
    pointed: &'a str,
    number: u64,
    pull_title: &'a str,
    pull_body: &'a str,
    base: &'a str,
    diff: &'a str,
}

fn prompt(r: &Reading<'_>) -> String {
    let Reading {
        issue,
        title,
        body,
        pointed,
        number,
        pull_title,
        pull_body,
        base,
        diff,
    } = r;
    let pointed = match pointed.is_empty() {
        true => String::new(),
        false => format!("\n\n--- what the issue points to ---\n{pointed}\n--- end ---"),
    };
    format!(
        "You are the last check before a pull request merges. The review rounds before you \
         read the diff line by line; you read the work as a whole. You may open any file \
         in this worktree with Read, Grep or Glob to check your answer; do not run any \
         command and do not edit anything.\n\n\
         Answer two questions.\n\
         1. For each acceptance criterion of the issue, and each item the issue points to \
         that it asks for, is it met by the final diff, and where? An issue with no \
         criteria heading has its bullets as its criteria.\n\
         2. What does the change assume about the world outside the repo: labels, files, \
         settings, other services, what a command or a forge answers? Does the code or a \
         test check each assumption against the real thing, rather than a fake or a \
         stand-in that accepts anything?\n\n\
         Be strict. A criterion is met only when the diff does all of it, and `where` names \
         the file and line, or the test, that shows it; otherwise `where` says what is \
         missing. An assumption is checked only when something runs against the real \
         thing; one that only a fake touches, or that nothing touches, is not.\n\n\
         Output exactly one JSON object and nothing else:\n\
         {{\"criteria\": [{{\"criterion\": \"<text>\", \"met\": true|false, \"where\": \"<text>\"}}], \
         \"assumptions\": [{{\"assumption\": \"<text>\", \"checked\": true|false, \"where\": \"<text>\"}}]}}\n\n\
         --- issue #{issue}: {title} ---\n{}\n--- end ---{pointed}\n\n\
         --- pull request #{number}: {pull_title} ---\n{}\n--- end ---\n\n\
         --- final diff against {base} ---\n{diff}\n--- end ---",
        cut(body.trim(), MOST),
        cut(pull_body.trim(), MOST),
    )
}

/// What the check answered
#[derive(Debug, Deserialize)]
struct Findings {
    criteria: Vec<Criterion>,
    assumptions: Vec<Assumption>,
}

#[derive(Debug, Deserialize)]
struct Criterion {
    criterion: String,
    met: bool,
    #[serde(rename = "where", default)]
    place: String,
}

#[derive(Debug, Deserialize)]
struct Assumption {
    assumption: String,
    checked: bool,
    #[serde(rename = "where", default)]
    place: String,
}

impl Findings {
    // Each criterion not met and each assumption not checked, as the worker
    // is told of it.
    fn gaps(&self) -> Vec<String> {
        let unmet = self.criteria.iter().filter(|c| !c.met).map(|c| {
            format!(
                "Criterion not met: {} ({})",
                c.criterion.trim(),
                c.place.trim()
            )
        });
        let unchecked = self.assumptions.iter().filter(|a| !a.checked).map(|a| {
            format!(
                "Assumption not checked against the real thing: {} ({})",
                a.assumption.trim(),
                a.place.trim()
            )
        });
        unmet.chain(unchecked).collect()
    }
}

// The check's JSON object, which may be wrapped in prose or a code fence:
// the first `{` that starts the object through the last `}`.
fn read_findings(text: &str) -> Result<Findings, String> {
    let end = text.rfind('}');
    let found = end.and_then(|end| {
        let starts = text.match_indices('{').map(|(start, _)| start);
        starts
            .filter(|start| *start < end)
            .find_map(|start| serde_json::from_str(&text[start..=end]).ok())
    });
    found.ok_or_else(|| format!("unreadable whole-issue check output: {}", text.trim()))
}

fn gap_prompt(number: u64, head: &str, gaps: &[String]) -> String {
    let list = gaps
        .iter()
        .map(|gap| format!("- {gap}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Kelpie's whole-issue check of your pull request #{number} at {} found gaps \
         between it and the issue:\n\n{list}\n\n\
         Close each on this branch. For a criterion, do the rest of what it asks and say \
         where. For an assumption about the world outside the repo, make the code or a \
         test check it against the real thing, not a fake that accepts anything, or change \
         the code if the assumption is wrong. Commit, and push with `git push origin HEAD`.",
        short(head)
    )
}

// The issue numbers `body` points to with `#<n>`, in order of first mention,
// leaving out `own`, and at most [`POINTED_AT`] of them.
fn pointed_at(body: &str, own: &[u64]) -> Vec<u64> {
    let bytes = body.as_bytes();
    let mut seen = BTreeSet::new();
    let mut found = Vec::new();
    for (at, byte) in bytes.iter().enumerate() {
        let alone = at == 0 || !bytes[at - 1].is_ascii_alphanumeric();
        if *byte != b'#' || !alone {
            continue;
        }
        let digits = bytes[at + 1..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        // `#123abc` is no reference to issue 123.
        let after = bytes.get(at + 1 + digits);
        if after.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_') {
            continue;
        }
        let Ok(n) = body[at + 1..at + 1 + digits].parse::<u64>() else {
            continue;
        };
        if !own.contains(&n) && seen.insert(n) {
            found.push(n);
        }
    }
    found.truncate(POINTED_AT);
    found
}

// Cut at a character boundary, saying so.
fn cut(text: &str, most: usize) -> String {
    if text.len() <= most {
        return text.to_owned();
    }
    let end = (0..=most)
        .rev()
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(0);
    format!("{}\n[the rest is left out]", &text[..end])
}
