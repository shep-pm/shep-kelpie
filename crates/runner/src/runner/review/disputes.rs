//! A worker's dispute of a finding it shows is wrong
//!
//! A fix turn that pushes nothing counts when every finding it was sent that
//! is not a nit was deferred or disputed with a reason. The disputes are shown
//! to the next reviewer, and posted on the pull request when none follows.

use std::path::Path;

use super::findings::{self, Sent};
use super::{Review, Unfixed};
use crate::ports::Finding;
use crate::runner::Runner;
use crate::runner::voice::finding_line;
use crate::state::StateError;
#[cfg(test)]
use crate::test::ticket;
use crate::work_item::{Dispute, ThreadAnswer, WorkItem};

/// What a fix turn that pushed nothing said of the findings it was sent that
/// are not nits, each by its index in the findings sent
#[derive(Debug, Default, PartialEq, Eq)]
pub(in crate::runner) struct Answers {
    /// Shown wrong, with the worker's reason
    disputed: Vec<(usize, String)>,
    /// Disputed with a reason, though a reviewer had raised it again after
    /// an earlier dispute
    repeated: Vec<usize>,
    /// Deferred as out of scope, and not disputed, with the worker's reason
    /// when it gave one
    deferred: Vec<(usize, Option<String>)>,
    /// Neither deferred nor disputed with a reason
    unanswered: Vec<usize>,
}

/// What a fix turn that pushed nothing did with the findings it was sent,
/// when it left none unanswered
#[derive(Debug, PartialEq, Eq)]
pub(in crate::runner) struct Settled {
    /// How many it deferred, and not disputed
    pub deferred: usize,
    /// What it disputed with a reason, by the index of the finding sent
    pub disputed: Vec<(usize, String)>,
}

/// How the worker answered `sent` in its files
///
/// `already` is what the pass disputed before `round`: a finding in it that
/// was sent again cannot be disputed again, so the two readers that
/// disagree end with the maintainer. A finding both deferred and disputed
/// counts as disputed. A deferral or dispute an earlier round left in its file
/// answers nothing in this one.
///
/// # Errors
///
/// As [`findings::deferred_with_reasons`] and [`findings::disputed`].
pub(in crate::runner) fn answers(
    build: &Path,
    sent: Sent<'_>,
    round: u32,
    already: &[Dispute],
) -> Result<Answers, String> {
    let mut deferred = findings::deferred_with_reasons(build)?;
    for before in sent.deferred_before {
        if let Some(at) = (deferred.iter()).position(|d| d.finding.is_same_as(before)) {
            deferred.remove(at);
        }
    }
    let disputed = findings::disputed(build)?;
    let mut answers = Answers::default();
    let to_answer =
        (sent.findings.iter().enumerate()).filter(|(_, f)| !f.is_nit() && !f.is_skipped_for_size());
    for (at, f) in to_answer {
        let reason = (disputed.iter())
            .filter(|d| d.finding.is_same_as(f))
            .find_map(|d| d.reason.clone());
        let again = already
            .iter()
            .any(|a| a.round < round && a.finding.is_same_as(f));
        match reason {
            Some(reason) if !again => answers.disputed.push((at, reason)),
            Some(_) => answers.repeated.push(at),
            None => match deferred.iter().find(|d| d.finding.is_same_as(f)) {
                Some(d) => answers.deferred.push((at, d.reason.clone())),
                None => answers.unanswered.push(at),
            },
        }
    }
    Ok(answers)
}

impl Answers {
    /// What the worker showed wrong, with its reason, by the finding's index
    pub(in crate::runner) fn disputed(&self) -> &[(usize, String)] {
        &self.disputed
    }

    /// Those disputed again after a reviewer raised them again
    pub(in crate::runner) fn repeated_at(&self) -> &[usize] {
        &self.repeated
    }

    /// Those deferred, and not disputed, with the worker's reason when it
    /// gave one
    pub(in crate::runner) fn deferred(&self) -> &[(usize, Option<String>)] {
        &self.deferred
    }

    /// Whether the worker disputed a finding again that a reviewer had raised
    /// again after an earlier dispute
    pub(in crate::runner) fn repeated(&self) -> bool {
        !self.repeated.is_empty()
    }

    /// What the answers come to when every finding that is not a nit is
    /// deferred or disputed, and one at least is disputed
    pub(in crate::runner) fn settled(self) -> Option<Settled> {
        let whole = self.unanswered.is_empty() && self.repeated.is_empty();
        (whole && !self.disputed.is_empty()).then_some(Settled {
            deferred: self.deferred.len(),
            disputed: self.disputed,
        })
    }

    /// What the worker disputed and deferred of `sent`, and what it left,
    /// for the question of the ruling a fix turn that fell short raises; none
    /// when it did neither
    pub(in crate::runner) fn said(&self, sent: &[Finding]) -> Option<String> {
        let places = |at: &[usize]| {
            let places: Vec<String> = (at.iter())
                .map(|at| format!("`{}:{}`", sent[*at].file, sent[*at].line))
                .collect();
            places.join(", ")
        };
        let disputed: Vec<usize> = self.disputed.iter().map(|(at, _)| *at).collect();
        let mut said = Vec::new();
        if !disputed.is_empty() {
            said.push(format!("disputed {}", places(&disputed)));
        }
        if !self.repeated.is_empty() {
            said.push(format!(
                "disputed {} again after a reviewer raised it again, which parks it",
                places(&self.repeated)
            ));
        }
        if !self.deferred.is_empty() {
            let deferred: Vec<usize> = self.deferred.iter().map(|(at, _)| *at).collect();
            said.push(format!("deferred {}", places(&deferred)));
        }
        if said.is_empty() {
            return None;
        }
        let mut said = format!("The worker {}", said.join(" and "));
        if !self.unanswered.is_empty() {
            said.push_str(&format!(
                ", and left {} neither fixed, deferred nor disputed with a reason",
                places(&self.unanswered)
            ));
        }
        said.push('.');
        Some(said)
    }
}

/// A dispute, with the bot thread it answers where the finding was one
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runner) struct Raised {
    /// The forge's id of the thread, for a bot's finding
    pub thread: Option<String>,
    /// The dispute
    pub dispute: Dispute,
}

/// Keeps `raised` on `item`, and the worker's reason as the answer to the bot
/// thread of each, which stays open
///
/// A dispute kept already for the same round is replaced, so a step that
/// runs again records nothing twice.
pub(in crate::runner) fn record(item: &mut WorkItem, raised: &[Raised]) {
    for Raised { thread, dispute } in raised {
        item.disputes
            .retain(|d| !(d.round == dispute.round && d.finding.is_same_as(&dispute.finding)));
        item.disputes.push(dispute.clone());
        if let Some(thread) = thread {
            item.thread_answers.retain(|a| a.thread() != thread);
            item.thread_answers.push(ThreadAnswer::Declined {
                thread: thread.clone(),
                reason: dispute.reason.clone(),
            });
        }
    }
}

/// Marks every dispute as shown to a reader
pub(in crate::runner) fn mark_shown(item: &mut WorkItem) {
    for dispute in &mut item.disputes {
        dispute.shown = true;
    }
}

impl Runner {
    // What a fix turn that moved the head disputed, kept for the next reader
    // as one that pushed nothing keeps it. A bot's thread is answered by the
    // worker's lines then, so none is recorded here.
    pub(super) fn record_pushed_disputes(
        &mut self,
        review: &Review,
        sent: Sent<'_>,
    ) -> Result<Option<String>, StateError> {
        let item = self.current().expect("a fix is a work item's");
        let (build, round) = (item.build.clone(), review.round);
        let already = item.disputes.clone();
        let answers = match answers(&build, sent, round, &already) {
            Ok(answers) => answers,
            Err(reason) => return Ok(Some(reason)),
        };
        let settled = Settled {
            deferred: 0,
            disputed: answers.disputed,
        };
        if settled.disputed.is_empty() {
            return Ok(None);
        }
        let Unfixed::Disputed { mut disputes, .. } = self.disputed_by(review, sent, settled) else {
            unreachable!("disputes are what `disputed_by` makes");
        };
        for raised in &mut disputes {
            raised.thread = None;
        }
        self.update(|item| record(item, &disputes))?;
        Ok(None)
    }

    // What `settled` found, as the fix turn's outcome: each dispute with who
    // raised it and, for a bot's finding, its thread, which `threads_sent`
    // lists in the order the findings were sent.
    pub(super) fn disputed_by(&self, review: &Review, sent: Sent<'_>, settled: Settled) -> Unfixed {
        let item = self.current().expect("a fix is a work item's");
        let by_thread = item.threads_sent.len() == sent.findings.len();
        let raised_by = (review.reviewer.as_ref()).map_or_else(String::new, ToString::to_string);
        let disputes = (settled.disputed.into_iter())
            .map(|(at, reason)| Raised {
                thread: by_thread.then(|| item.threads_sent[at].clone()),
                dispute: Dispute {
                    finding: sent.findings[at].clone(),
                    reason,
                    raised_by: raised_by.clone(),
                    round: review.round,
                    shown: false,
                },
            })
            .collect();
        Unfixed::Disputed {
            disputes,
            deferred: settled.deferred,
        }
    }
}

impl Runner {
    /// Posts the disputes no reviewer was shown on pull request `number` as
    /// one comment, as the App where one covers the repo, and says where they
    /// are for the merge ruling; none when the worker disputed nothing, and
    /// why not when the comment could not be posted, for the step to try again
    ///
    /// # Errors
    ///
    /// [`StateError`] when the state file cannot be written.
    pub(in crate::runner) fn post_disputes(
        &mut self,
        number: u64,
    ) -> Result<Result<Option<String>, String>, StateError> {
        let item = self.current().expect("a dispute is a work item's");
        let unshown: Vec<&Dispute> = item.disputes.iter().filter(|d| !d.shown).collect();
        if unshown.is_empty() {
            return Ok(Ok((item.disputes_comment).map(|id| told(None, number, id))));
        }
        let body = comment(&unshown);
        let count = unshown.len();
        let posted = match self.app_covers() {
            true => (self.ports.voice.post(&self.remote, number, &body)).map(|posted| posted.id),
            false => self.comment_as_gh_id(number, &body),
        };
        match posted {
            Ok(id) => {
                self.update(|item| {
                    mark_shown(item);
                    item.disputes_comment = Some(id);
                })?;
                Ok(Ok(Some(told(Some(count), number, id))))
            }
            Err(e) => Ok(Err(format!(
                "cannot post the worker's {} disputes on #{number}: {e}",
                findings_of(count)
            ))),
        }
    }
}

fn findings_of(count: usize) -> String {
    match count {
        1 => "1 finding".to_owned(),
        n => format!("{n} findings"),
    }
}

// `count` is how many the comment lists, unknown where it was posted by an earlier step
fn told(count: Option<usize>, number: u64, comment: u64) -> String {
    let which = count.map_or_else(|| "findings".to_owned(), findings_of);
    format!(
        "The worker disputed {which} that no reviewer read again: they are in comment {comment} on #{number}."
    )
}

fn comment(disputes: &[&Dispute]) -> String {
    let mut text = String::from(
        "The worker disputed these findings, saying each is wrong, and no reviewer follows to read the reasons:\n",
    );
    for Dispute {
        finding,
        reason,
        raised_by,
        ..
    } in disputes
    {
        let who = if raised_by.is_empty() {
            "a reader"
        } else {
            raised_by
        };
        text.push_str(&format!(
            "\n{}\n  Raised by {who}. The worker's reason: {reason}\n",
            finding_line(finding)
        ));
    }
    text
}

#[cfg(test)]
use std::sync::Mutex;

#[cfg(test)]
use serde_json::json;

#[cfg(test)]
use crate::github::ApiError;
#[cfg(test)]
use crate::ports::{AgentError, Checks, Severity};
#[cfg(test)]
use crate::runner::{StepReport, step};
#[cfg(test)]
use crate::settings::Harness;
#[cfg(test)]
use crate::test::{Rig, Scripted, ScriptedRound};

// Round 1 on pull request 71 found one MEDIUM finding, and its fix turn is next.
#[cfg(test)]
fn found() -> Finding {
    Finding::new(
        Severity::Medium,
        "src/lib.rs",
        3,
        "the closure destructures a three-tuple",
        "it reads the zip as three values",
    )
}

#[cfg(test)]
const FOUND_LINE: &str =
    "MEDIUM|src/lib.rs:3|the closure destructures a three-tuple|it reads the zip as three values\n";

#[cfg(test)]
fn findings_sent() -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // opens the pull request, enters round 1 (qwen)
    rig.reviewer
        .script([ScriptedRound::Findings(vec![found()])]);
    step(&runner).unwrap(); // round 1's qwen call
    step(&runner).unwrap(); // the findings go out: the fix turn is next
    (rig, runner)
}

#[cfg(test)]
fn dispute(rig: &Rig, text: &str) {
    std::fs::write(rig.build_7().join("disputed-findings.md"), text).unwrap();
}

#[test]
fn a_fix_turn_that_disputes_every_finding_with_a_reason_goes_on_to_the_next_reviewer() {
    let (rig, runner) = findings_sent();
    dispute(
        &rig,
        &format!(
            "{FOUND_LINE}  src/runner/review.rs:312 destructures the two-tuple and builds the three-tuple for the next map\n"
        ),
    );
    rig.claude
        .script([Scripted::Say("The finding reads the closure wrong.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed

    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::FindingsDisputed {
            issue: ticket(7),
            pull_request: 71,
            round: 1,
            disputed: 1,
            deferred: 0,
        })
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"], json!([]));
    assert_eq!(
        status["work_item"]["phase"],
        json!({
            "state": "review",
            "round": 2,
            "stage": { "stage": "round" },
            "ran": ["qwen"],
        })
    );
}

#[cfg(test)]
const REASON: &str = "  src/runner/review.rs:312 destructures the two-tuple and builds the three-tuple for the next map\n";

#[test]
fn a_dispute_with_no_reason_line_parks_on_fix_not_pushed() {
    let (rig, runner) = findings_sent();
    dispute(&rig, FOUND_LINE);
    rig.claude.script([Scripted::Say("The finding is wrong.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed

    let report = step(&runner).unwrap();

    assert!(
        matches!(report, Some(StepReport::Ruling { .. })),
        "{report:?}"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["kind"]["reason"], "fix-not-pushed");
}

#[test]
fn a_turn_that_defers_some_findings_and_disputes_the_rest_goes_on() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // opens the pull request, enters round 1 (qwen)
    let other = Finding::new(
        Severity::High,
        "src/lib.rs",
        9,
        "another thing",
        "it is wrong",
    );
    rig.reviewer
        .script([ScriptedRound::Findings(vec![found(), other])]);
    step(&runner).unwrap(); // round 1's qwen call
    step(&runner).unwrap(); // the findings go out: the fix turn is next
    let deferred = "HIGH|src/lib.rs:9|another thing|it is wrong\n";
    std::fs::write(rig.build_7().join("deferred-findings.md"), deferred).unwrap();
    dispute(&rig, &format!("{FOUND_LINE}{REASON}"));
    rig.claude
        .script([Scripted::Say("One is wrong, one is out of scope.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed

    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::FindingsDisputed {
            issue: ticket(7),
            pull_request: 71,
            round: 1,
            disputed: 1,
            deferred: 1,
        })
    );
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
}

#[test]
fn a_turn_that_disputes_one_finding_and_leaves_another_parks_and_the_question_names_it() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // opens the pull request, enters round 1 (qwen)
    let other = Finding::new(
        Severity::High,
        "src/lib.rs",
        9,
        "another thing",
        "it is wrong",
    );
    rig.reviewer
        .script([ScriptedRound::Findings(vec![found(), other])]);
    step(&runner).unwrap(); // round 1's qwen call
    step(&runner).unwrap(); // the findings go out: the fix turn is next
    dispute(&rig, &format!("{FOUND_LINE}{REASON}"));
    rig.claude.script([Scripted::Say("One is wrong.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed

    let Some(StepReport::Ruling { question, .. }) = step(&runner).unwrap() else {
        panic!("a finding left unanswered parks the item");
    };

    assert!(
        question.contains("disputed `src/lib.rs:3`"),
        "the dispute: {question}"
    );
    assert!(
        question.contains("left `src/lib.rs:9`"),
        "the finding left: {question}"
    );
}

#[test]
fn a_disputed_finding_whose_attached_test_still_fails_parks() {
    let rig = Rig::new("shep");
    rig.test_command("cargo test --test {name}");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the worker's first turn
    step(&runner).unwrap(); // round 1, qwen: clean by default
    let failed = |output: &str| crate::ports::TestOutcome::Failed {
        output: output.to_owned(),
    };
    rig.tests.script([failed("before"), failed("after")]);
    let line = "MEDIUM|src/lib.rs:3|the flag is read first|a caller sees nothing|tests/flag.rs";
    rig.claude.script([Scripted::Attach(
        "tests/flag.rs",
        "#[test]\nfn flag_is_read() { assert!(false); }\n",
        line,
    )]);
    step(&runner).unwrap(); // round 2, claude
    step(&runner).unwrap(); // the findings sent
    rig.claude.script([Scripted::Push("fixed.txt", "fixed\n")]);
    step(&runner).unwrap(); // the worker's fix turn
    step(&runner).unwrap(); // the test fails still: the worker is sent back to it

    // The second fix turn pushes nothing and says the finding is wrong.
    dispute(&rig, &format!("{line}\n{REASON}"));
    rig.claude.script([Scripted::Say("The test is wrong.")]);
    step(&runner).unwrap();
    let report = step(&runner).unwrap();

    assert!(
        matches!(report, Some(StepReport::Ruling { .. })),
        "{report:?}"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["kind"]["reason"], "fix-not-pushed");
}

#[test]
fn the_next_reviewer_is_shown_each_disputed_finding_and_its_reason() {
    let (rig, runner) = findings_sent();
    dispute(&rig, &format!("{FOUND_LINE}{REASON}"));
    rig.claude.script([Scripted::Say("The finding is wrong.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed
    step(&runner).unwrap(); // the dispute is taken

    rig.claude.script([Scripted::Text(crate::ports::CLEAN)]);
    step(&runner).unwrap(); // round 2, claude
    let prompt = rig.claude.all_calls().pop().unwrap().prompt;

    assert!(
        prompt.contains(FOUND_LINE.trim_end()),
        "the finding as it was raised: {prompt}"
    );
    assert!(
        prompt.contains(REASON.trim()),
        "the worker's reason: {prompt}"
    );
    assert!(
        prompt.contains("only if the reason is wrong"),
        "the ask: {prompt}"
    );
}

#[test]
fn a_finding_disputed_a_second_time_parks_on_fix_not_pushed() {
    let (rig, runner) = findings_sent();
    dispute(&rig, &format!("{FOUND_LINE}{REASON}"));
    rig.claude.script([Scripted::Say("The finding is wrong.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed
    step(&runner).unwrap(); // the dispute is taken
    // The next reviewer reads the reason and raises the finding again.
    rig.claude.script([Scripted::Text(FOUND_LINE)]);
    step(&runner).unwrap(); // round 2, claude
    step(&runner).unwrap(); // the finding goes to the worker again
    dispute(
        &rig,
        &format!("{FOUND_LINE}{REASON}{FOUND_LINE}  I ran it again and it still holds.\n"),
    );
    rig.claude.script([Scripted::Say("Still wrong.")]);
    step(&runner).unwrap(); // the second fix turn ends with nothing pushed

    let report = step(&runner).unwrap();

    assert!(
        matches!(report, Some(StepReport::Ruling { .. })),
        "{report:?}"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["kind"]["reason"], "fix-not-pushed");
}

#[cfg(unix)]
#[test]
fn a_dispute_file_that_links_to_another_file_is_not_read() {
    let dir = tempfile::tempdir().unwrap();
    let (build, theirs) = (dir.path().join("build"), dir.path().join("notes.md"));
    std::fs::create_dir(&build).unwrap();
    std::fs::write(&theirs, format!("{FOUND_LINE}{REASON}")).unwrap();
    std::os::unix::fs::symlink(&theirs, super::findings::disputed_path(&build)).unwrap();

    let err = super::findings::disputed(&build).unwrap_err();

    assert!(err.contains("disputed-findings.md"), "{err}");
    assert!(err.contains("not a regular file"), "{err}");
    assert!(!err.contains("closure"), "{err}");
}

#[test]
fn with_no_reviewer_after_it_the_disputes_go_on_the_pull_request_as_one_comment() {
    let rig = Rig::new("shep");
    rig.with_app();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // opens the pull request, enters round 1 (qwen)
    step(&runner).unwrap(); // round 1, qwen: clean by default
    rig.claude.script([Scripted::Text(FOUND_LINE)]);
    step(&runner).unwrap(); // round 2, claude, the last reviewer, finds it
    step(&runner).unwrap(); // the finding goes to the worker
    dispute(&rig, &format!("{FOUND_LINE}{REASON}"));
    rig.claude.script([Scripted::Say("The finding is wrong.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed
    step(&runner).unwrap(); // the dispute is taken: no reviewer is left

    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "ci");
    rig.ci_on_now(&runner);
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    let Some(StepReport::Ruling { question, .. }) = rig.verdict(&runner) else {
        panic!("a merge ruling is raised on green CI");
    };

    let comments = rig.github.comments();
    let disputes: Vec<_> = comments
        .iter()
        .filter(|(_, text)| text.contains("disputed"))
        .collect();
    let [(thread, text)] = disputes.as_slice() else {
        panic!("one comment for the disputes: {comments:?}");
    };
    assert_eq!(*thread, 71);
    assert!(text.contains("claude"), "who raised it: {text}");
    assert!(
        text.contains("`src/lib.rs:3`: the closure destructures a three-tuple"),
        "the finding: {text}"
    );
    assert!(text.contains(REASON.trim()), "the worker's reason: {text}");
    assert!(
        question.contains("disputed"),
        "the ruling names it: {question}"
    );
    assert!(question.contains("comment"), "{question}");
}

// Pull request 71 with the App covering the repo, round 1 (qwen) about to read it.
#[cfg(test)]
fn at_round_1_with_app() -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("shep");
    rig.with_app();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // opens the pull request, enters round 1 (qwen)
    (rig, runner)
}

// Green CI on the head, and the merge ruling it raises
#[cfg(test)]
fn merge_question(rig: &Rig, runner: &Mutex<Runner>) -> String {
    rig.ci_on_now(runner);
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    let Some(StepReport::Ruling { question, .. }) = rig.verdict(runner) else {
        panic!("a merge ruling is raised on green CI");
    };
    question
}

#[test]
fn a_reviewer_that_fails_after_being_shown_the_disputes_leaves_them_for_the_pull_request() {
    let (rig, runner) = at_round_1_with_app();
    rig.reviewer
        .script([ScriptedRound::Findings(vec![found()])]);
    step(&runner).unwrap(); // round 1's qwen call
    step(&runner).unwrap(); // the findings go out: the fix turn is next
    dispute(&rig, &format!("{FOUND_LINE}{REASON}"));
    rig.claude.script([Scripted::Say("The finding is wrong.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed
    step(&runner).unwrap(); // the dispute is taken: claude reads next
    let down = AgentError::Failed(Harness::ClaudeCode, "overloaded".into());
    rig.claude
        .script((0..3).map(|_| Scripted::Fail(down.clone())));
    for _ in 0..3 {
        step(&runner).unwrap(); // claude's round fails, and is passed over on the third
    }
    // The bots are not listed, so claude was the pass's last reviewer.
    rig.reviewers(&["qwen", "claude"]);
    rig.allow(&["sonnet-high", "qwen", "claude"]);
    let (settings, kelpie) = (rig.settings(), rig.kelpie_settings());
    runner.lock().unwrap().reread(settings, kelpie).unwrap();
    step(&runner).unwrap(); // one of two read, so the pass ends marked unreviewed

    let question = merge_question(&rig, &runner);

    let comments = rig.github.comments();
    assert!(
        comments.iter().any(|(_, text)| text.contains("disputed")),
        "the disputes no reviewer read are posted: {comments:?}"
    );
    assert!(question.contains("disputed"), "{question}");
}

#[test]
fn the_merge_ruling_counts_only_the_disputes_no_reviewer_read() {
    let (rig, runner) = at_round_1_with_app();
    rig.reviewer
        .script([ScriptedRound::Findings(vec![found()])]);
    step(&runner).unwrap(); // round 1's qwen call
    step(&runner).unwrap(); // the findings go out: the fix turn is next
    dispute(&rig, &format!("{FOUND_LINE}{REASON}"));
    rig.claude.script([Scripted::Say("The finding is wrong.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed
    step(&runner).unwrap(); // the dispute is taken: claude reads next
    let other = "HIGH|src/lib.rs:9|another thing|it is wrong\n";
    rig.claude.script([Scripted::Text(other)]);
    step(&runner).unwrap(); // round 2, claude, the last reviewer, finds another
    step(&runner).unwrap(); // the finding goes to the worker
    dispute(&rig, &format!("{other}{REASON}"));
    rig.claude.script([Scripted::Say("Wrong too.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed
    step(&runner).unwrap(); // the dispute is taken: no reviewer is left

    let question = merge_question(&rig, &runner);

    assert!(
        question.contains("disputed 1 finding that"),
        "only the dispute no reviewer read: {question}"
    );
}

#[test]
fn a_finding_disputed_a_second_time_and_deferred_still_parks_on_fix_not_pushed() {
    let (rig, runner) = findings_sent();
    dispute(&rig, &format!("{FOUND_LINE}{REASON}"));
    rig.claude.script([Scripted::Say("The finding is wrong.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed
    step(&runner).unwrap(); // the dispute is taken
    rig.claude.script([Scripted::Text(FOUND_LINE)]);
    step(&runner).unwrap(); // round 2, claude raises it again
    step(&runner).unwrap(); // the finding goes to the worker again
    dispute(
        &rig,
        &format!("{FOUND_LINE}{REASON}{FOUND_LINE}  I ran it again and it still holds.\n"),
    );
    std::fs::write(rig.build_7().join("deferred-findings.md"), FOUND_LINE).unwrap();
    rig.claude.script([Scripted::Say("Still wrong.")]);
    step(&runner).unwrap(); // the second fix turn ends with nothing pushed

    let report = step(&runner).unwrap();

    assert!(
        matches!(report, Some(StepReport::Ruling { .. })),
        "{report:?}"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["kind"]["reason"], "fix-not-pushed");
}

// Round 1 found `found()`; the worker's fix turn pushes and disputes it too, as the
// instructions let it; round 2's reader is about to read the head it pushed.
#[cfg(test)]
fn disputed_in_a_fix_that_pushed() -> (Rig, Mutex<Runner>) {
    let (rig, runner) = findings_sent();
    dispute(&rig, &format!("{FOUND_LINE}{REASON}"));
    rig.claude.script([Scripted::Push("fixed.txt", "fixed\n")]);
    step(&runner).unwrap(); // the fix turn pushes
    step(&runner).unwrap(); // the fix turn ends: round 2 is next
    (rig, runner)
}

#[test]
fn a_dispute_made_in_a_fix_that_pushed_is_shown_to_the_next_reviewer() {
    let (rig, runner) = disputed_in_a_fix_that_pushed();

    rig.claude.script([Scripted::Text(crate::ports::CLEAN)]);
    step(&runner).unwrap(); // round 2, claude
    let prompt = rig.claude.all_calls().pop().unwrap().prompt;

    assert!(
        prompt.contains(REASON.trim()),
        "the worker's reason: {prompt}"
    );
}

#[test]
fn a_dispute_a_pushing_fix_made_is_not_an_answer_to_the_next_round() {
    let (rig, runner) = disputed_in_a_fix_that_pushed();
    rig.claude.script([Scripted::Text(FOUND_LINE)]);
    step(&runner).unwrap(); // round 2, claude raises it again
    step(&runner).unwrap(); // the finding goes to the worker again
    rig.claude.script([Scripted::Say("Nothing to add.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed, the files untouched

    let report = step(&runner).unwrap();

    assert!(
        matches!(report, Some(StepReport::Ruling { .. })),
        "{report:?}"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["kind"]["reason"], "fix-not-pushed");
}

#[test]
fn disputes_that_cannot_be_posted_hold_an_auto_merge() {
    let rig = Rig::new("shep");
    rig.with_app();
    rig.merge_auto();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // opens the pull request, enters round 1 (qwen)
    step(&runner).unwrap(); // round 1, qwen: clean by default
    rig.claude.script([Scripted::Text(FOUND_LINE)]);
    step(&runner).unwrap(); // round 2, claude, the last reviewer, finds it
    step(&runner).unwrap(); // the finding goes to the worker
    dispute(&rig, &format!("{FOUND_LINE}{REASON}"));
    rig.claude.script([Scripted::Say("The finding is wrong.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed
    step(&runner).unwrap(); // the dispute is taken: no reviewer is left
    rig.github.refuse_comments(ApiError::Refused(502));

    rig.ci_on_now(&runner);
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    let report = rig.verdict(&runner);

    assert!(
        matches!(report, Some(StepReport::GateFailed { .. })),
        "{report:?}"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "ci");
}
