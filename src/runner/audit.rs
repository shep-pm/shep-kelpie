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
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

use serde::Deserialize;

use super::Runner;
use super::gate::short;
use super::report::{Begin, StepReport};
use super::review::calls::{build_call, diff_against};
use super::ruling::park;
use crate::pacer::Scope;
use crate::ports::{AgentCall, AgentError, AgentReply, PullRequestState, Role, Tools};
use crate::state::{RulingKind, StateError};
use crate::work_item::{Audit, CallKind, Passed, Phase, SENDS_BACK, Turn};
use crate::worktree;

#[cfg(test)]
mod tests;

/// The folder in the work item's build folder that the check's files go in
const FOLDER: &str = "audit";

/// Where the check's throwaway settings go
const SETTINGS_FILE: &str = "audit-settings.json";

/// The file of what the check reads but the diff, in its folder
const MATERIAL_FILE: &str = "issue.md";

/// The file of the final diff, in its folder
const DIFF_FILE: &str = "diff.patch";

impl Runner {
    // Whether `head`, which CI passed, goes on to the merge ruling: the
    // check's call when it has not passed this head on what the issue and the
    // pull request say now. An edit to either is as new to it as a push.
    pub(super) fn audit_before_merge(
        &mut self,
        number: u64,
        head: &str,
    ) -> Result<Option<Begin>, StateError> {
        let item = self.current().expect("the check is of a work item");
        let issue = item.issue;
        let inputs = match self.read_inputs(issue, number) {
            Ok(inputs) => inputs,
            Err(reason) => return Ok(Some(self.gate_failed(reason))),
        };
        let fingerprint = inputs.fingerprint();
        let passed = item.audit.as_ref().and_then(|a| a.passed.as_ref());
        if passed.is_some_and(|p| p.head == head && p.inputs == fingerprint) {
            return Ok(None);
        }
        let limit = self.agents.limits.auditor.clone();
        if let Some(held) = self.pace(Scope::Turn, &limit)?.holds() {
            return Ok(Some(held));
        }
        let call = match self.audit_call(issue, number, &inputs) {
            Ok(call) => call,
            Err(reason) => return Ok(Some(self.gate_failed(reason))),
        };
        let since = self.ports.clock.now();
        self.update(|item| item.call_started(CallKind::Audit, since))?;
        let audited = Audited {
            head: head.to_owned(),
            inputs: fingerprint,
        };
        Ok(Some(Begin::Audit(Box::new(call), audited)))
    }

    // What the check reads of the issue, what its body points to and the pull
    // request, which is everything it reads but the diff.
    fn read_inputs(&self, issue: u64, number: u64) -> Result<Inputs, String> {
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
        Ok(Inputs {
            title: found.title,
            body: found.body,
            pointed,
            pull_title: pull.title,
            pull_body: pull.body,
        })
    }

    fn audit_call(&self, issue: u64, number: u64, inputs: &Inputs) -> Result<AgentCall, String> {
        let item = self.current().expect("the check is of a work item");
        let base = format!("origin/{}", worktree::BASE);
        let diff = diff_against(&item.worktree, &base)?;
        // What it reads goes in files, not in the prompt, which a harness
        // passes as one argument, and a long issue and diff would not fit.
        let folder = item.build.join(FOLDER);
        let write = |name: &str, text: String| {
            std::fs::create_dir_all(&folder)
                .and_then(|()| std::fs::write(folder.join(name), text))
                .map_err(|e| format!("cannot write the whole-issue check's {name}: {}", e.kind()))
        };
        write(MATERIAL_FILE, inputs.material(issue, number))?;
        write(DIFF_FILE, diff)?;
        let prompt = prompt(&Reading {
            folder: &folder,
            base: &base,
        });
        let model = &self.agents.auditor;
        let limit = &self.agents.limits.auditor;
        let mut call = build_call(Role::Auditor, issue, &item.worktree, (model, limit), prompt)?;
        call.settings = self.paths.worker.join(SETTINGS_FILE);
        call.reach.read = vec![folder];
        // It reads the worktree to see what the code and its tests check,
        // and runs no command.
        call.tools = Tools::Review;
        self.prepared(call)
    }

    // What `n`, which the issue's body points to, says: a pull request's
    // title and body with the review comments still unresolved on it, or an
    // issue's title and body. A pull request is asked for first, since the
    // forge's issue view answers for a pull request's number too, with its
    // title and body alone, while a pull request view of an issue fails.
    // Said so when it cannot be read, for the check to count against.
    fn pulled_in(&self, n: u64) -> String {
        let forge = &self.ports.forge;
        let repo = &self.settings.forge;
        if let Ok(pull) = forge.reviewed(repo, n) {
            let mut said = format!("#{n} {}\n\n{}", pull.title.trim(), pull.body.trim());
            // The review's own body can hold the request, with no comment on a line.
            let review_body = pull.review.iter().map(|r| r.body.trim());
            for body in review_body.filter(|body| !body.is_empty()) {
                said.push_str(&format!("\n\nreview: {body}"));
            }
            let comments = pull.review.iter().flat_map(|r| &r.comments);
            for comment in comments {
                let line = comment.line.map(|l| format!(":{l}")).unwrap_or_default();
                said.push_str(&format!(
                    "\n\nreview comment on {}{line}: {}",
                    comment.file,
                    comment.body.trim()
                ));
            }
            return said;
        }
        match forge.issue(repo, n) {
            Ok(found) => {
                format!("#{n} {}\n\n{}", found.title.trim(), found.body.trim())
            }
            Err(e) => format!("#{n} could not be read: {e}"),
        }
    }

    /// Records what the check found of `audited`, once its call ends
    pub(super) fn end_audit(
        &mut self,
        audited: &Audited,
        result: Result<AgentReply, AgentError>,
    ) -> Result<Option<StepReport>, StateError> {
        let head = audited.head.as_str();
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
        // The call ran outside the lock, so the pull request may have been
        // merged, closed or pushed to meanwhile, and the issue or the pull
        // request's description edited. What it found is then about a head or
        // a text that no longer stands, and the next step takes it from there.
        let still = match self.ports.forge.pull_request(&self.settings.forge, number) {
            Ok(pr) => pr.state == PullRequestState::Open && pr.head == head,
            Err(e) => {
                let reason = format!("cannot read #{number} after the whole-issue check: {e}");
                return Ok(Some(StepReport::GateFailed { issue, reason }));
            }
        };
        if !still {
            return Ok(None);
        }
        match self.read_inputs(issue, number) {
            Ok(inputs) if inputs.fingerprint() == audited.inputs => {}
            Ok(_) => return Ok(None),
            Err(reason) => return Ok(Some(StepReport::GateFailed { issue, reason })),
        }
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
        let passed = Passed {
            head: head.to_owned(),
            inputs: audited.inputs,
        };
        if gaps.is_empty() {
            self.update(|item| {
                let audit = item.audit.get_or_insert_with(Audit::default);
                audit.passed = Some(passed);
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
            // The counter's reset and the ruling are one save, so a restart
            // between them cannot send the same gap to the worker twice more.
            let kind = RulingKind::Audit {
                head: head.to_owned(),
                gaps,
                prompt,
            };
            let mut next = self.state.clone();
            let item = self
                .current_in(&mut next)
                .expect("the check is of a work item");
            item.audit.get_or_insert_with(Audit::default).sent_back = 0;
            let (id, question) = park(self.names(), &mut next, issue, Some(number), kind);
            self.save(next)?;
            let comment_failed = self.post_ruling(Some(number), id);
            return Ok(Some(StepReport::Ruling {
                issue,
                pull_request: number,
                id,
                question,
                comment_failed,
            }));
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

/// What a check call was made on: the head, and what the issue and the pull
/// request said then
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Audited {
    pub(super) head: String,
    pub(super) inputs: u64,
}

/// What the check reads but the diff
struct Inputs {
    title: String,
    body: String,
    pointed: String,
    pull_title: String,
    pull_body: String,
}

impl Inputs {
    // Stands for the text read. A different one only makes the check run
    // again, so it need not outlive the build that wrote it.
    fn fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        for text in [
            &self.title,
            &self.body,
            &self.pointed,
            &self.pull_title,
            &self.pull_body,
        ] {
            text.hash(&mut hasher);
        }
        hasher.finish()
    }
}

/// Where the check's files are
struct Reading<'a> {
    folder: &'a Path,
    base: &'a str,
}

impl Inputs {
    // The issue, what it points to and the pull request, as the check reads them.
    fn material(&self, issue: u64, number: u64) -> String {
        let pointed = match self.pointed.is_empty() {
            true => String::new(),
            false => format!(
                "\n\n--- what the issue points to ---\n{}\n--- end ---",
                self.pointed
            ),
        };
        format!(
            "--- issue #{issue}: {} ---\n{}\n--- end ---{pointed}\n\n\
             --- pull request #{number}: {} ---\n{}\n--- end ---\n",
            self.title,
            self.body.trim(),
            self.pull_title,
            self.pull_body.trim(),
        )
    }
}

fn prompt(r: &Reading<'_>) -> String {
    let Reading { folder, base } = r;
    let (material, diff) = (folder.join(MATERIAL_FILE), folder.join(DIFF_FILE));
    format!(
        "You are the last check before a pull request merges. The review rounds before you \
         read the diff line by line; you read the work as a whole. You may open any file \
         in this worktree with Read, Grep or Glob to check your answer; do not run any \
         command and do not edit anything.\n\n\
         What you check is in two files. Read each to its end, in parts if it is long, \
         before you answer.\n\
         - {}: the issue, whatever its body points to, and the pull request's own text\n\
         - {}: the final diff against {base}\n\n\
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
         \"assumptions\": [{{\"assumption\": \"<text>\", \"checked\": true|false, \"where\": \"<text>\"}}]}}",
        material.display(),
        diff.display(),
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
        // An entry that says it holds but names no file, line or test shows
        // nothing, so it is as much a gap as one that says it does not.
        let unmet = self
            .criteria
            .iter()
            .filter_map(|c| match (c.met, c.place.trim()) {
                (true, "") => Some(format!(
                    "Criterion with no evidence of where it is met: {}",
                    c.criterion.trim()
                )),
                (true, _) => None,
                (false, place) => Some(format!(
                    "Criterion not met: {} ({place})",
                    c.criterion.trim()
                )),
            });
        let unchecked = self
            .assumptions
            .iter()
            .filter_map(|a| match (a.checked, a.place.trim()) {
                (true, "") => Some(format!(
                    "Assumption with no evidence of what checks it: {}",
                    a.assumption.trim()
                )),
                (true, _) => None,
                (false, place) => Some(format!(
                    "Assumption not checked against the real thing: {} ({place})",
                    a.assumption.trim()
                )),
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
// leaving out `own`. Every one is pulled in: a request in the ninth would
// otherwise go unread while an empty answer still passed.
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
    found
}
