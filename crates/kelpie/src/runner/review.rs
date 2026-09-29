//! The qwen-review loop: rounds alternating qwen and Claude, each judged
//! finding by finding before the worker sees anything
//!
//! A round with no raw findings is clean at once. One with findings waits
//! for the judge, in order; once every finding is judged, the held ones (if
//! any) go to the worker's next turn, and the round's cleanliness is decided
//! by the judge's severities. Two clean rounds in a row, one from each
//! reviewer since they strictly alternate, end the loop for [`super::gate`].
//! Past the round guard the worker parks for a ruling; a yes clears the
//! guard for the rest of this work item.

pub(super) mod calls;
pub(super) mod findings;

use std::path::Path;

use super::Runner;
use super::report::{Begin, ReviewCall, ReviewResult, Reviewed, Spent, StepReport};
use super::shots::RoundShots;
use crate::ports::{
    Claude, ClaudeCall, ClaudeError, ClaudeReply, Finding, Reviewer, ReviewerError, Severity,
    Timestamp, Verdict, parse_findings,
};
use crate::state::{Fix, RulingKind, StateError};
use crate::work_item::{Phase, Review, ReviewCallState, ReviewStage, ReviewerKind, Turn, WorkItem};
use crate::worktree;

impl Runner {
    pub(super) fn review_step(&mut self) -> Result<Begin, StateError> {
        let item = self
            .state
            .work_item
            .as_ref()
            .expect("review runs on a work item");
        let Phase::Review(review) = item.phase.clone() else {
            unreachable!("review_step only runs in the review phase")
        };
        let number = item
            .pull_request
            .expect("review starts once a pull request is known");
        let (issue, worktree) = (item.issue, item.worktree.clone());
        let base = item.review_base();
        let build = item.build.clone();
        let worker_folder = self.paths.worker.clone();

        match review.stage.clone() {
            ReviewStage::Round => {
                if !review.guard_cleared && review.round > self.settings.review.loop_guard.get() {
                    return self.raise(number, RulingKind::ReviewGuard { review });
                }
                match review.reviewer() {
                    ReviewerKind::Qwen => {
                        self.mark_review_call_running()?;
                        Ok(Begin::Review(ReviewCall::Qwen {
                            worktree,
                            base,
                            out: build.join("qwen-review"),
                            round: review.round,
                        }))
                    }
                    ReviewerKind::Claude => {
                        let shots = match self.round_shots()? {
                            RoundShots::Take(begin) => return Ok(begin),
                            RoundShots::Ready(shots) => shots,
                        };
                        let model = self.settings.models.reviewer.clone();
                        let dir = self.paths.shots(issue);
                        let shots = shots.as_ref().map(|run| calls::Screens { dir: &dir, run });
                        match calls::reviewer_call(&worktree, &base, &worker_folder, &model, shots)
                        {
                            Ok(call) => {
                                self.mark_review_call_running()?;
                                Ok(Begin::Review(ReviewCall::ClaudeRound(call)))
                            }
                            Err(reason) => Ok(self.gate_failed(reason)),
                        }
                    }
                }
            }
            ReviewStage::Judging { findings, verdicts } => {
                if verdicts.len() == findings.len() {
                    return self.finalize_round(review, findings, verdicts);
                }
                let finding = findings[verdicts.len()].clone();
                let model = self.settings.models.judge.clone();
                let shots = self.preview_on().then(|| self.paths.shots(issue));
                match calls::judge_call(
                    &worktree,
                    &base,
                    &worker_folder,
                    &model,
                    &finding,
                    shots.as_deref(),
                ) {
                    Ok(call) => {
                        self.mark_review_call_running()?;
                        Ok(Begin::Review(ReviewCall::Judge(call)))
                    }
                    Err(reason) => Ok(self.gate_failed(reason)),
                }
            }
            // begin_turn drives the fix turn itself, and comes here once it ends.
            ReviewStage::Fixing { clean, head } => {
                self.fix_ended(number, &build, review, clean, head)
            }
        }
    }

    // A fix turn that pushed nothing fixed nothing, whatever it says: the
    // held findings still stand, so the round cannot count.
    fn fix_ended(
        &mut self,
        number: u64,
        build: &Path,
        review: Review,
        clean: bool,
        head: Option<String>,
    ) -> Result<Begin, StateError> {
        let issue = self
            .state
            .work_item
            .as_ref()
            .expect("a fix is a work item's")
            .issue;
        let round = review.round;
        let pushed = match head {
            Some(before) => match self.origin_head() {
                Ok(now) if now == before => {
                    let path = findings::findings_path(build);
                    let prompt = findings::again_prompt(number, round, &path);
                    let fix = Fix::Review(review);
                    return self.raise(number, RulingKind::FixNotPushed { fix, prompt });
                }
                Ok(now) => Some(now),
                Err(reason) => return Ok(self.gate_failed(reason)),
            },
            None => None,
        };
        let now = self.ports.clock.now();
        self.update(|item| item.phase = advance(review, clean, now))?;
        Ok(Begin::Report(StepReport::FixPushed {
            issue,
            pull_request: number,
            round,
            head: pushed,
        }))
    }

    // Asks git rather than the forge: the forge's head lags a push by a moment.
    pub(super) fn origin_head(&self) -> Result<String, String> {
        let item = self
            .state
            .work_item
            .as_ref()
            .expect("a head is a work item's");
        worktree::origin_head(&self.settings.repo, &item.branch).map_err(|e| e.to_string())
    }

    // Recorded in state before the runner's lock is released for the call
    // itself, the same as a worker's turn marks `Turn::Running`: `drop`
    // refuses while this is set, so a call in flight always has a work
    // item to land its result on.
    pub(super) fn mark_review_call_running(&mut self) -> Result<(), StateError> {
        let since = self.ports.clock.now();
        self.update(|item| item.review_call = ReviewCallState::Running { since })
    }

    // Every held finding holds the judge's own severity, since the nit rule
    // and the worker's fix both go by what the judge decided, not the
    // reviewer's original claim.
    fn finalize_round(
        &mut self,
        review: Review,
        findings: Vec<Finding>,
        verdicts: Vec<Verdict>,
    ) -> Result<Begin, StateError> {
        let item = self
            .state
            .work_item
            .as_ref()
            .expect("finalize runs on a work item");
        let issue = item.issue;
        let number = item
            .pull_request
            .expect("review runs once a pull request is known");
        let round = review.round;
        let now = self.ports.clock.now();
        let held: Vec<Finding> = findings
            .into_iter()
            .zip(verdicts)
            .filter(|(_, v)| v.holds)
            .map(|(f, v)| Finding {
                severity: v.severity,
                ..f
            })
            .collect();
        if held.is_empty() {
            self.update(|item| item.phase = advance(review, true, now))?;
            return Ok(Begin::Report(StepReport::ReviewFindingsSent {
                issue,
                pull_request: number,
                round,
                held: 0,
                clean: true,
            }));
        }
        let clean = held.iter().all(|f| f.severity <= Severity::Low);
        let head = match self.origin_head() {
            Ok(head) => head,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        let build = &item.build;
        let path = findings::findings_path(build);
        if let Err(reason) = findings::write_findings_file(build, &path, round, &held) {
            return Ok(self.gate_failed(reason));
        }
        let held_count = held.len();
        let prompt = findings::fix_prompt(number, round, held_count, &path);
        self.update(|item| {
            item.turn = Turn::Next { prompt };
            item.phase = Phase::Review(Review {
                stage: ReviewStage::Fixing {
                    clean,
                    head: Some(head),
                },
                ..review
            });
        })?;
        Ok(Begin::Report(StepReport::ReviewFindingsSent {
            issue,
            pull_request: number,
            round,
            held: held_count,
            clean,
        }))
    }

    pub(super) fn end_review(
        &mut self,
        reviewed: Reviewed,
    ) -> Result<Option<StepReport>, StateError> {
        let Reviewed { result, spent } = reviewed;
        // A call stopped with the runner saves nothing, the same as a turn
        // in `end_turn`: its round or judge call is still due, and runs
        // again once a restart clears `review_call`.
        if matches!(result, ReviewResult::Stopped) {
            return Ok(None);
        }
        let in_coderabbit_round = self
            .state
            .work_item
            .as_ref()
            .is_some_and(|item| matches!(item.phase, Phase::CodeRabbit(_)));
        if in_coderabbit_round {
            return self.coderabbit_verdict(result, spent);
        }
        let now = self.ports.clock.now();
        let mut next = self.state.clone();
        // Tolerated the same way `end_turn` tolerates a turn's result
        // arriving with nothing (or something else) to apply it to: the
        // guard on `drop` keeps this from happening today, but a result
        // for a work item that is no longer this one, or gone outright,
        // is silently discarded rather than panicking the runner.
        let Some(item) = next.work_item.as_mut() else {
            return Ok(None);
        };
        record_spent(item, spent, now);
        let Phase::Review(review) = item.phase.clone() else {
            return Ok(None);
        };
        let issue = item.issue;
        let number = item
            .pull_request
            .expect("review runs once a pull request is known");
        let round = review.round;

        let report = match result {
            ReviewResult::Findings(Err(reason)) => StepReport::GateFailed { issue, reason },
            // Nothing to judge: the round is clean at once.
            ReviewResult::Findings(Ok(findings)) if findings.is_empty() => {
                if !matches!(review.stage, ReviewStage::Round) {
                    unreachable!("a round's findings only arrive while awaiting that round");
                }
                item.phase = advance(review, true, now);
                StepReport::ReviewFindingsSent {
                    issue,
                    pull_request: number,
                    round,
                    held: 0,
                    clean: true,
                }
            }
            ReviewResult::Findings(Ok(findings)) => {
                if !matches!(review.stage, ReviewStage::Round) {
                    unreachable!("a round's findings only arrive while awaiting that round");
                }
                let reviewer = review.reviewer();
                let count = findings.len();
                item.phase = Phase::Review(Review {
                    stage: ReviewStage::Judging {
                        findings,
                        verdicts: Vec::new(),
                    },
                    ..review
                });
                StepReport::ReviewRound {
                    issue,
                    pull_request: number,
                    round,
                    reviewer,
                    findings: count,
                }
            }
            ReviewResult::Verdict(Err(reason)) => StepReport::GateFailed { issue, reason },
            ReviewResult::Stopped => unreachable!("a stopped call returns above"),
            ReviewResult::Verdict(Ok(verdict)) => {
                let ReviewStage::Judging {
                    findings,
                    mut verdicts,
                } = review.stage.clone()
                else {
                    unreachable!("a verdict only arrives while judging");
                };
                let (holds, severity) = (verdict.holds, verdict.severity);
                verdicts.push(verdict);
                item.phase = Phase::Review(Review {
                    stage: ReviewStage::Judging { findings, verdicts },
                    ..review
                });
                StepReport::FindingJudged {
                    issue,
                    round,
                    holds,
                    severity,
                }
            }
        };
        self.save(next)?;
        Ok(Some(report))
    }
}

/// Where the review phase goes after one round finishes, clean or not
///
/// Two clean rounds in a row end the loop, and a strict qwen/Claude
/// alternation means the pair is always one of each.
pub(super) fn advance(review: Review, clean: bool, now: crate::ports::Timestamp) -> Phase {
    let consecutive_clean = if clean {
        review.consecutive_clean + 1
    } else {
        0
    };
    if consecutive_clean >= 2 {
        Phase::Ci {
            head: None,
            since: now,
        }
    } else {
        Phase::Review(Review {
            round: review.round + 1,
            consecutive_clean,
            guard_cleared: review.guard_cleared,
            stage: ReviewStage::Round,
        })
    }
}

/// Runs `action` outside the runner's lock: the qwen script, or a fresh
/// Claude call for a review round or the judge
///
/// A call stopped with the runner comes back as [`ReviewResult::Stopped`]
/// rather than an error, so `end_review` can tell it from a failed gate.
pub(super) fn run_review_call(
    claude: &dyn Claude,
    reviewer: &dyn Reviewer,
    action: ReviewCall,
) -> Reviewed {
    match action {
        ReviewCall::Qwen {
            worktree,
            base,
            out,
            round,
        } => match reviewer.round(&worktree, &base, &out, round) {
            Err(ReviewerError::Stopped) => stopped(),
            result => Reviewed {
                result: ReviewResult::Findings(result.map_err(|e| e.to_string())),
                spent: Some(Spent::Qwen),
            },
        },
        ReviewCall::ClaudeRound(call) => {
            let (reply, spent) = run_claude(claude, &call);
            match reply {
                Err(ClaudeError::Stopped) => stopped(),
                reply => Reviewed {
                    result: ReviewResult::Findings(
                        reply
                            .map(|reply| parse_findings(&reply.text))
                            .map_err(|e| e.to_string()),
                    ),
                    spent,
                },
            }
        }
        ReviewCall::Judge(call) => {
            let (reply, spent) = run_claude(claude, &call);
            match reply {
                Err(ClaudeError::Stopped) => stopped(),
                reply => Reviewed {
                    result: ReviewResult::Verdict(reply.map_err(|e| e.to_string()).and_then(
                        |reply| {
                            calls::parse_verdict(&reply.text).ok_or_else(|| {
                                format!("unreadable judge output: {}", reply.text.trim())
                            })
                        },
                    )),
                    spent,
                },
            }
        }
    }
}

/// Adds what a review call spent to the work item's record, and marks no
/// call in flight
///
/// A qwen round's time runs from when the call was marked running.
pub(super) fn record_spent(item: &mut WorkItem, spent: Option<Spent>, now: Timestamp) {
    match spent {
        Some(Spent::Claude {
            role,
            session,
            usage,
            session_cost,
        }) => {
            item.record_call(role, now, session, usage, session_cost);
        }
        Some(Spent::Qwen) => {
            item.qwen.rounds += 1;
            if let ReviewCallState::Running { since } = item.review_call {
                item.qwen.seconds += now.0.saturating_sub(since.0);
            }
        }
        None => {}
    }
    item.review_call = ReviewCallState::Idle;
}

fn stopped() -> Reviewed {
    Reviewed {
        result: ReviewResult::Stopped,
        spent: None,
    }
}

// A reply that came back cost something even if what it said is unusable.
fn run_claude(
    claude: &dyn Claude,
    call: &ClaudeCall,
) -> (Result<ClaudeReply, ClaudeError>, Option<Spent>) {
    let reply = claude.run(call);
    let spent = reply.as_ref().ok().map(|reply| Spent::Claude {
        role: call.role,
        session: call.session.id().clone(),
        usage: reply.usage,
        session_cost: reply.session_cost,
    });
    (reply, spent)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ports::{Cost, Role, Session};
    use crate::runner::step;
    use crate::test::{Rig, Scripted, ScriptedRound};

    // `drop` refuses while a call is running (see merge.rs's own test for
    // that), but `end_review` tolerates a work item that is gone anyway,
    // the same defensive shape as `end_turn`'s equivalent guard, rather
    // than panicking the runner on the `.expect` this replaced.
    #[test]
    fn end_review_tolerates_a_work_item_that_is_gone() {
        let (_rig, runner, _) = Rig::with_pull_request("shep");
        runner.lock().unwrap().state.work_item = None;
        let report = runner
            .lock()
            .unwrap()
            .end_review(Reviewed {
                result: ReviewResult::Findings(Ok(vec![])),
                spent: None,
            })
            .unwrap();
        assert_eq!(report, None);
    }

    #[test]
    fn advancing_a_clean_round_extends_the_streak_and_settles_at_two() {
        let now = crate::ports::Timestamp(100);
        let after_first = advance(Review::first(), true, now);
        let Phase::Review(review) = after_first else {
            panic!("stays reviewing after one clean round");
        };
        assert_eq!((review.round, review.consecutive_clean), (2, 1));
        assert_eq!(review.stage, ReviewStage::Round);

        let phase = advance(review, true, now);
        assert_eq!(
            phase,
            Phase::Ci {
                head: None,
                since: now
            }
        );
    }

    #[test]
    fn a_dirty_round_resets_the_streak_but_keeps_the_guard_cleared_flag() {
        let now = crate::ports::Timestamp(1);
        let review = Review {
            round: 4,
            consecutive_clean: 1,
            guard_cleared: true,
            stage: ReviewStage::Round,
        };
        let Phase::Review(next) = advance(review, false, now) else {
            panic!("stays reviewing");
        };
        assert_eq!(
            next,
            Review {
                round: 5,
                consecutive_clean: 0,
                guard_cleared: true,
                stage: ReviewStage::Round,
            }
        );
    }

    #[test]
    fn a_round_with_only_nits_is_clean_once_the_worker_fixes_them() {
        let (rig, runner) = at_round_1("shep");

        rig.reviewer.script([ScriptedRound::Findings(vec![Finding {
            severity: Severity::Medium,
            file: "src/lib.rs".into(),
            line: 3,
            what: "unused variable".into(),
            why: "dead code".into(),
        }])]);
        let report = step(&runner).unwrap(); // round 1's qwen call
        assert!(
            matches!(
                report,
                Some(StepReport::ReviewRound {
                    round: 1,
                    findings: 1,
                    ..
                })
            ),
            "{report:?}"
        );

        rig.claude.script([Scripted::Text(
            r#"{"holds": true, "severity": "low", "reason": "it is a nit, not a bug"}"#,
        )]);
        step(&runner).unwrap(); // the judge regrades it to LOW and holds it

        let Some(StepReport::ReviewFindingsSent {
            round, held, clean, ..
        }) = step(&runner).unwrap()
        // the round finalizes: one nit held
        else {
            panic!("the held nit was not sent to the worker");
        };
        assert_eq!((round, held, clean), (1, 1, true));
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["turn"]["state"],
            "next"
        );

        rig.claude.script([
            Scripted::Push("fixed.txt", "fixed\n"),
            Scripted::Text("CLEAN"),
        ]);
        step(&runner).unwrap(); // the worker's fix turn
        let fixed = rig.forge.head_of("kelpie/7");
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::FixPushed {
                issue: 7,
                pull_request: 71,
                round: 1,
                head: fixed,
            })
        );
        step(&runner).unwrap(); // round 2, claude: scripted clean above

        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
            "ci",
            "two clean rounds in a row end the loop"
        );
    }

    #[test]
    fn a_round_the_script_could_not_finish_is_reported_and_retried() {
        let (rig, runner) = at_round_1("shep");

        rig.reviewer
            .script([ScriptedRound::Fail(crate::ports::ReviewerError::Incomplete)]);
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::GateFailed {
                issue: 7,
                reason: "qwen-review.sh left no completion marker".into(),
            })
        );
        assert_eq!(rig.reviewer.seen().len(), 1);
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["phase"],
            json!({
                "state": "review",
                "round": 1,
                "consecutive_clean": 0,
                "guard_cleared": false,
                "stage": { "stage": "round" },
            }),
            "the round stays due, and the next step tries it again"
        );
        assert_eq!(
            step(&runner).unwrap(), // clean by default this time
            Some(StepReport::ReviewFindingsSent {
                issue: 7,
                pull_request: 71,
                round: 1,
                held: 0,
                clean: true,
            })
        );
    }

    #[test]
    fn a_rejected_finding_never_reaches_the_worker() {
        let (rig, runner) = at_round_1("shep");

        rig.reviewer.script([ScriptedRound::Findings(vec![Finding {
            severity: Severity::High,
            file: "src/lib.rs".into(),
            line: 9,
            what: "looks racy".into(),
            why: "two threads write the same field".into(),
        }])]);
        step(&runner).unwrap(); // round 1's qwen call

        rig.claude.script([Scripted::Text(
            r#"{"holds": false, "severity": "high", "reason": "the field is behind a mutex"}"#,
        )]);
        step(&runner).unwrap(); // the judge rejects it

        assert_eq!(
            step(&runner).unwrap(), // the round finalizes: nothing held
            Some(StepReport::ReviewFindingsSent {
                issue: 7,
                pull_request: 71,
                round: 1,
                held: 0,
                clean: true,
            })
        );
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["turn"]["state"],
            "ended",
            "no fix turn was queued for a finding the judge rejected"
        );
        assert_eq!(
            rig.claude.calls().len(),
            1,
            "only the worker's own turn is a `Role::Worker` call"
        );
        assert_eq!(
            rig.claude.all_calls().len(),
            2,
            "the worker's turn, and the judge's one-shot"
        );
    }

    #[test]
    fn reviewer_and_judge_sessions_never_match_the_workers() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "add", Some("7"));
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        rig.claude.script([
            Scripted::Push("work.txt", "work\n"),
            Scripted::Text("CLEAN"),
        ]);
        step(&runner).unwrap(); // the worker's first turn
        step(&runner).unwrap(); // round 1, qwen: clean by default
        step(&runner).unwrap(); // round 2, claude: scripted clean above

        let worker_session = rig.ask(&runner, "status", None)["work_item"]["session"].clone();
        let all = rig.claude.all_calls();
        let reviewer_call = all
            .iter()
            .find(|c| c.role == Role::Reviewer)
            .expect("a reviewer round ran");
        assert_ne!(json!(reviewer_call.session.id()), worker_session);
        assert!(
            matches!(reviewer_call.session, Session::New(_)),
            "a fresh session every round, never the worker's resumed"
        );
    }

    #[test]
    fn the_round_guard_parks_for_a_ruling_and_a_yes_clears_it_for_the_rest_of_the_item() {
        let (rig, runner, _) = Rig::with_pull_request("shep");
        drop(runner);
        let state = rig.paths().state;
        let text = std::fs::read_to_string(&state).unwrap();
        let mut saved: serde_json::Value = serde_json::from_str(&text).unwrap();
        saved["work_item"]["phase"] = json!({
            "state": "review",
            "round": 9,
            "consecutive_clean": 0,
            "guard_cleared": false,
            "stage": { "stage": "round" },
        });
        std::fs::write(&state, saved.to_string()).unwrap();
        let runner = rig.open().unwrap();

        let Some(StepReport::Ruling { id, question, .. }) = step(&runner).unwrap() else {
            panic!("the round guard did not raise a ruling");
        };
        assert!(question.contains("8 rounds"), "{question}");

        rig.ask(&runner, "rule", Some(&format!("{id} yes")));
        assert_eq!(
            step(&runner).unwrap(), // round 9, qwen: clean by default, past the guard
            Some(StepReport::ReviewFindingsSent {
                issue: 7,
                pull_request: 71,
                round: 9,
                held: 0,
                clean: true,
            })
        );
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["phase"],
            json!({
                "state": "review",
                "round": 10,
                "consecutive_clean": 1,
                "guard_cleared": true,
                "stage": { "stage": "round" },
            }),
        );
    }

    // Where round 1 stands before its qwen call runs: the worker's first
    // turn opened the pull request.
    fn at_round_1(project: &str) -> (Rig, std::sync::Mutex<crate::runner::Runner>) {
        let rig = Rig::new(project);
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "add", Some("7"));
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        rig.claude.script([Scripted::Push("work.txt", "work\n")]);
        step(&runner).unwrap();
        (rig, runner)
    }

    #[test]
    fn a_claude_round_with_a_finding_records_a_reviewer_call_and_a_judge_call() {
        let (rig, runner) = at_round_1("shep");
        step(&runner).unwrap(); // round 1, qwen: clean by default
        rig.claude.script([
            Scripted::Billed(
                "MEDIUM|src/lib.rs:3|unused variable|dead code",
                Cost(50_000_000),
            ),
            Scripted::Billed(
                r#"{"holds": false, "severity": "low", "reason": "it is used"}"#,
                Cost(7_000_000),
            ),
        ]);
        step(&runner).unwrap(); // round 2, claude: one finding
        step(&runner).unwrap(); // the judge rejects it

        let status = rig.ask(&runner, "status", None);
        let item = &status["work_item"];
        assert_eq!(
            item["calls"], 3,
            "the worker's turn, the reviewer's and the judge's"
        );
        assert_eq!(
            item["by_role"]["reviewer"],
            json!({ "calls": 1, "cost_usd": 0.05 })
        );
        assert_eq!(
            item["by_role"]["judge"],
            json!({ "calls": 1, "cost_usd": 0.007 })
        );
        assert_eq!(
            item["by_role"]["worker"],
            json!({ "calls": 1, "cost_usd": 0.0 }),
            "the worker's turn cost nothing here, and is not the reviewer's"
        );
        assert_eq!(item["qwen"]["rounds"], 1);
    }

    #[test]
    fn a_judge_reply_that_cannot_be_read_is_still_recorded() {
        let (rig, runner) = at_round_1("shep");
        step(&runner).unwrap(); // round 1, qwen: clean by default
        rig.claude.script([
            Scripted::Billed("HIGH|src/lib.rs:9|racy|two writers", Cost(1)),
            Scripted::Billed("not json", Cost(9)),
        ]);
        step(&runner).unwrap(); // round 2, claude: one finding
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::GateFailed { .. })
        ));

        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["work_item"]["by_role"]["judge"]["calls"], 1);
    }

    #[test]
    fn a_qwen_rounds_time_runs_from_when_it_was_marked_running() {
        let mut item = crate::test::a_work_item();
        item.review_call = ReviewCallState::Running {
            since: Timestamp(100),
        };
        record_spent(&mut item, Some(Spent::Qwen), Timestamp(190));
        record_spent(&mut item, Some(Spent::Qwen), Timestamp(200));
        assert_eq!(item.qwen.rounds, 2);
        assert_eq!(
            item.qwen.seconds, 90,
            "the second had no start to measure from"
        );
        assert_eq!(item.review_call, ReviewCallState::Idle);
    }

    #[test]
    fn a_finished_work_items_report_carries_its_totals() {
        let (rig, runner) = at_round_1("shep");
        step(&runner).unwrap(); // round 1, qwen: clean by default
        rig.claude
            .script([Scripted::Billed("CLEAN", Cost(20_000_000))]);
        step(&runner).unwrap(); // round 2, claude: clean, so CI is next
        rig.forge
            .set_state(71, crate::ports::PullRequestState::Merged);
        let Some(StepReport::Finished { spend, qwen, .. }) = step(&runner).unwrap() else {
            panic!("the merged work item did not finish");
        };
        assert_eq!(spend.reviewer.calls, 1);
        assert_eq!(qwen.rounds, 1);
    }

    #[test]
    fn a_qwen_round_stopped_with_the_runner_runs_again_on_restart() {
        let (rig, runner) = at_round_1("shep");
        rig.reviewer
            .script([ScriptedRound::Fail(crate::ports::ReviewerError::Stopped)]);
        assert_eq!(step(&runner).unwrap(), None, "no failed gate is reported");
        drop(runner);

        let runner = rig.open().unwrap();
        assert_eq!(
            step(&runner).unwrap(), // clean by default this time
            Some(StepReport::ReviewFindingsSent {
                issue: 7,
                pull_request: 71,
                round: 1,
                held: 0,
                clean: true,
            })
        );
        assert_eq!(rig.reviewer.seen().len(), 2, "round 1 ran again");
    }

    #[test]
    fn a_claude_round_stopped_with_the_runner_runs_again_on_restart() {
        let (rig, runner) = at_round_1("shep");
        step(&runner).unwrap(); // round 1, qwen: clean by default
        rig.claude.script([Scripted::Fail(ClaudeError::Stopped)]);
        assert_eq!(step(&runner).unwrap(), None, "no failed gate is reported");
        drop(runner);

        let runner = rig.open().unwrap();
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["phase"],
            json!({
                "state": "review",
                "round": 2,
                "consecutive_clean": 1,
                "guard_cleared": false,
                "stage": { "stage": "round" },
            }),
            "round 2 is still due, and round 1's clean still counts"
        );
        rig.claude.script([Scripted::Text("CLEAN")]);
        step(&runner).unwrap();
        let rounds = rig.claude.all_calls();
        let rounds = rounds.iter().filter(|c| c.role == Role::Reviewer).count();
        assert_eq!(rounds, 2, "round 2 ran again");
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
            "ci"
        );
    }

    #[test]
    fn a_judge_call_stopped_with_the_runner_runs_again_on_restart() {
        let (rig, runner) = at_round_1("shep");
        rig.reviewer.script([ScriptedRound::Findings(vec![Finding {
            severity: Severity::High,
            file: "src/lib.rs".into(),
            line: 9,
            what: "looks racy".into(),
            why: "two threads write the same field".into(),
        }])]);
        step(&runner).unwrap(); // round 1's qwen call
        rig.claude.script([Scripted::Fail(ClaudeError::Stopped)]);
        assert_eq!(step(&runner).unwrap(), None, "no failed gate is reported");
        drop(runner);

        let runner = rig.open().unwrap();
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["phase"]["stage"]["verdicts"],
            json!([]),
        );
        rig.claude.script([Scripted::Text(
            r#"{"holds": false, "severity": "high", "reason": "the field is behind a mutex"}"#,
        )]);
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::FindingJudged {
                issue: 7,
                round: 1,
                holds: false,
                severity: Severity::High,
            })
        );
    }
}
