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

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::Runner;
use super::report::{Begin, ReviewCall, ReviewResult, StepReport};
use crate::ports::{ClaudeCall, Finding, Role, Session, Severity, Verdict, parse_findings};
use crate::settings::RoleModel;
use crate::state::{RulingKind, StateError};
use crate::work_item::{Phase, Review, ReviewStage, ReviewerKind, Turn, new_session_id};

/// Where the round's held findings are written for the worker's next turn
const FINDINGS_FILE: &str = "review-findings.md";

/// The Claude round's throwaway settings: no sandbox fencing, no bypassed
/// permissions, but Read, Grep and Glob still work by default so it can
/// check its own work against the worktree
const REVIEW_SETTINGS_FILE: &str = "review-settings.json";

/// The judge's throwaway settings: every tool denied, so its one-shot answer
/// is schema-constrained text and nothing it read on the side
const JUDGE_SETTINGS_FILE: &str = "judge-settings.json";

/// Every tool name a Claude Code call can reach, denied outright for the judge
const NO_TOOLS: [&str; 11] = [
    "Bash",
    "Read",
    "Edit",
    "Write",
    "MultiEdit",
    "NotebookEdit",
    "Glob",
    "Grep",
    "WebFetch",
    "WebSearch",
    "Task",
];

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
        let worktree = item.worktree.clone();
        let worker_folder = self.paths.worker.clone();

        match review.stage.clone() {
            ReviewStage::Round => {
                if !review.guard_cleared && review.round > self.settings.review.loop_guard.get() {
                    return self.raise(number, RulingKind::ReviewGuard { review });
                }
                match review.reviewer() {
                    ReviewerKind::Qwen => Ok(Begin::Review(ReviewCall::Qwen {
                        worktree,
                        out: item.build.join("qwen-review"),
                        round: review.round,
                    })),
                    ReviewerKind::Claude => {
                        let model = self.settings.models.reviewer.clone();
                        match reviewer_call(&worktree, &worker_folder, &model) {
                            Ok(call) => Ok(Begin::Review(ReviewCall::ClaudeRound(call))),
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
                match judge_call(&worktree, &worker_folder, &model, &finding) {
                    Ok(call) => Ok(Begin::Review(ReviewCall::Judge(call))),
                    Err(reason) => Ok(self.gate_failed(reason)),
                }
            }
            ReviewStage::Fixing { .. } => {
                unreachable!("begin_turn drives a fix turn directly")
            }
        }
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
        let path = self.paths.worker.join(FINDINGS_FILE);
        if let Err(reason) = write_findings_file(&self.paths.worker, &path, round, &held) {
            return Ok(self.gate_failed(reason));
        }
        let held_count = held.len();
        let prompt = fix_prompt(number, round, held_count, &path);
        self.update(|item| {
            item.turn = Turn::Next { prompt };
            item.phase = Phase::Review(Review {
                stage: ReviewStage::Fixing { clean },
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
        result: ReviewResult,
    ) -> Result<Option<StepReport>, StateError> {
        let now = self.ports.clock.now();
        let mut next = self.state.clone();
        let item = next
            .work_item
            .as_mut()
            .expect("a review result is of a work item");
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
pub(super) fn run_review_call(
    claude: &dyn crate::ports::Claude,
    reviewer: &dyn crate::ports::Reviewer,
    action: ReviewCall,
) -> ReviewResult {
    match action {
        ReviewCall::Qwen {
            worktree,
            out,
            round,
        } => ReviewResult::Findings(
            reviewer
                .round(&worktree, &out, round)
                .map_err(|e| e.to_string()),
        ),
        ReviewCall::ClaudeRound(call) => ReviewResult::Findings(
            claude
                .run(&call)
                .map(|reply| parse_findings(&reply.text))
                .map_err(|e| e.to_string()),
        ),
        ReviewCall::Judge(call) => ReviewResult::Verdict(
            claude
                .run(&call)
                .map_err(|e| e.to_string())
                .and_then(|reply| {
                    parse_verdict(&reply.text)
                        .ok_or_else(|| format!("unreadable judge output: {}", reply.text.trim()))
                }),
        ),
    }
}

fn reviewer_call(
    worktree: &Path,
    worker_folder: &Path,
    model: &RoleModel,
) -> Result<ClaudeCall, String> {
    let diff = diff_against_base(worktree)?;
    let settings = review_settings(worker_folder)?;
    let session = new_session_id().map_err(|e| format!("cannot draw a session id: {e}"))?;
    Ok(ClaudeCall {
        role: Role::Reviewer,
        model: model.model.as_str().to_owned(),
        effort: model.effort,
        session: Session::New(session),
        cwd: worktree.to_owned(),
        settings,
        instructions: None,
        prompt: reviewer_prompt(&diff),
    })
}

fn judge_call(
    worktree: &Path,
    worker_folder: &Path,
    model: &RoleModel,
    finding: &Finding,
) -> Result<ClaudeCall, String> {
    let diff = diff_against_base(worktree)?;
    let settings = judge_settings(worker_folder)?;
    let session = new_session_id().map_err(|e| format!("cannot draw a session id: {e}"))?;
    Ok(ClaudeCall {
        role: Role::Judge,
        model: model.model.as_str().to_owned(),
        effort: model.effort,
        session: Session::New(session),
        cwd: worktree.to_owned(),
        settings,
        instructions: None,
        prompt: judge_prompt(&diff, finding),
    })
}

fn review_settings(worker_folder: &Path) -> Result<PathBuf, String> {
    let path = worker_folder.join(REVIEW_SETTINGS_FILE);
    super::turn::write(worker_folder, &path, "{}\n")?;
    Ok(path)
}

fn judge_settings(worker_folder: &Path) -> Result<PathBuf, String> {
    let path = worker_folder.join(JUDGE_SETTINGS_FILE);
    let deny = serde_json::json!({ "permissions": { "deny": NO_TOOLS } });
    let text = serde_json::to_string_pretty(&deny).expect("settings are JSON");
    super::turn::write(worker_folder, &path, &text)?;
    Ok(path)
}

fn diff_against_base(worktree: &Path) -> Result<String, String> {
    let base = format!("origin/{}", crate::worktree::BASE);
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["diff", &base])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run git diff: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn reviewer_prompt(diff: &str) -> String {
    format!(
        "You are a founding engineer reviewing a junior developer's pull request. Be \
         extremely critical and check every line of the diff below, against \
         `origin/main`. You may open any file in this worktree with Read, Grep or Glob \
         to check your work; do not run any command and do not edit anything.\n\n\
         Look for: code smells; duplicated code, types or logic; non-performant code; \
         non-idiomatic code for this language; hard-to-follow logic; poorly named \
         variables, functions and types; missing or inadequate doc comments; comments \
         that no longer match the code; missing error handling; unsafe assumptions \
         about input; and thin test coverage for new logic.\n\n\
         Report only real problems, never formatting or anything a linter already \
         enforces. Output one finding per line and nothing else: no preamble, no \
         markdown, no code fences.\n\
         SEVERITY|file:line|what is wrong|why it matters\n\n\
         SEVERITY must be HIGH, MEDIUM or LOW. If the diff is genuinely fine, output \
         exactly CLEAN and nothing else.\n\n\
         --- diff against origin/main ---\n{diff}\n--- end ---"
    )
}

fn judge_prompt(diff: &str, finding: &Finding) -> String {
    format!(
        "You are judging one code-review finding on a pull request. You did not write \
         the finding and will not fix it; you only decide whether it holds against the \
         diff below.\n\n\
         Finding:\n\
         severity: {}\n\
         location: {}:{}\n\
         what: {}\n\
         why: {}\n\n\
         Decide whether it holds. You may regrade its severity in either direction, \
         whether or not it holds. Output exactly one line of JSON and nothing else:\n\
         {{\"holds\": true|false, \"severity\": \"low\"|\"medium\"|\"high\", \"reason\": \"<one sentence>\"}}\n\n\
         --- diff against origin/main ---\n{diff}\n--- end ---",
        severity_tag(finding.severity),
        finding.file,
        finding.line,
        finding.what,
        finding.why,
    )
}

fn severity_tag(severity: Severity) -> &'static str {
    match severity {
        Severity::Low => "LOW",
        Severity::Medium => "MEDIUM",
        Severity::High => "HIGH",
    }
}

fn parse_verdict(text: &str) -> Option<Verdict> {
    #[derive(serde::Deserialize)]
    struct Raw {
        holds: bool,
        severity: String,
        reason: String,
    }
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end < start {
        return None;
    }
    let raw: Raw = serde_json::from_str(&text[start..=end]).ok()?;
    let severity = match raw.severity.to_lowercase().as_str() {
        "low" => Severity::Low,
        "medium" => Severity::Medium,
        "high" => Severity::High,
        _ => return None,
    };
    Some(Verdict {
        holds: raw.holds,
        severity,
        reason: raw.reason,
    })
}

fn write_findings_file(
    folder: &Path,
    path: &Path,
    round: u32,
    findings: &[Finding],
) -> Result<(), String> {
    let mut text = format!(
        "Round {round}'s held findings, at the judge's severity. Fix each one, then \
         commit and push.\n\n"
    );
    for f in findings {
        text.push_str(&format!(
            "{}|{}:{}|{}|{}\n",
            severity_tag(f.severity),
            f.file,
            f.line,
            f.what,
            f.why
        ));
    }
    super::turn::write(folder, path, &text)
}

fn fix_prompt(number: u64, round: u32, count: usize, path: &Path) -> String {
    format!(
        "Round {round} of the qwen-review loop on your pull request #{number} held \
         {count} finding(s), in {}. Fix each one, then commit and push with \
         `git push origin HEAD`.",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use serde_json::json;

    use super::*;
    use crate::runner::step;
    use crate::test::{Rig, Scripted, ScriptedRound};

    #[test]
    fn a_findings_file_that_cannot_be_written_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("readonly");
        std::fs::create_dir(&folder).unwrap();
        let mut perms = std::fs::metadata(&folder).unwrap().permissions();
        perms.set_mode(0o555);
        std::fs::set_permissions(&folder, perms).unwrap();

        let finding = Finding {
            severity: Severity::Low,
            file: "a.rs".into(),
            line: 1,
            what: "nit".into(),
            why: "style".into(),
        };
        let err = write_findings_file(&folder, &folder.join("review-findings.md"), 1, &[finding])
            .unwrap_err();
        assert!(err.contains("review-findings.md"), "{err}");

        // Restore write access so the tempdir can clean itself up.
        let mut perms = std::fs::metadata(&folder).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&folder, perms).unwrap();
    }

    #[test]
    fn a_judge_reply_wrapped_in_prose_or_fences_still_parses() {
        let plain = r#"{"holds": true, "severity": "medium", "reason": "it does hold"}"#;
        assert_eq!(
            parse_verdict(plain),
            Some(Verdict {
                holds: true,
                severity: Severity::Medium,
                reason: "it does hold".into(),
            })
        );
        let fenced = format!("```json\n{plain}\n```");
        assert_eq!(parse_verdict(&fenced), parse_verdict(plain));
        let cased = r#"{"holds": false, "severity": "HIGH", "reason": "no"}"#;
        assert_eq!(parse_verdict(cased).unwrap().severity, Severity::High);
    }

    #[test]
    fn an_unparseable_judge_reply_is_none() {
        assert_eq!(parse_verdict("I think it holds."), None);
        assert_eq!(parse_verdict(r#"{"holds": true}"#), None);
        assert_eq!(
            parse_verdict(r#"{"holds": true, "severity": "urgent", "reason": "x"}"#),
            None
        );
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
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "add", Some("7"));
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        rig.claude.script([Scripted::Push("work.txt", "work\n")]);
        step(&runner).unwrap(); // opens the pull request, enters round 1 (qwen)

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
        step(&runner).unwrap(); // round 2, claude: scripted clean above

        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
            "ci",
            "two clean rounds in a row end the loop"
        );
    }

    #[test]
    fn a_round_the_script_could_not_finish_is_reported_and_retried() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "add", Some("7"));
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        rig.claude.script([Scripted::Push("work.txt", "work\n")]);
        step(&runner).unwrap(); // opens the pull request, enters round 1 (qwen)

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
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "add", Some("7"));
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        rig.claude.script([Scripted::Push("work.txt", "work\n")]);
        step(&runner).unwrap(); // opens the pull request, enters round 1 (qwen)

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

    #[test]
    fn the_judge_gets_every_tool_denied() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "add", Some("7"));
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        rig.claude.script([Scripted::Push("work.txt", "work\n")]);
        step(&runner).unwrap(); // opens the pull request, enters round 1 (qwen)

        rig.reviewer.script([ScriptedRound::Findings(vec![Finding {
            severity: Severity::Low,
            file: "src/lib.rs".into(),
            line: 3,
            what: "unused variable".into(),
            why: "dead code".into(),
        }])]);
        step(&runner).unwrap(); // round 1's qwen call

        rig.claude.script([Scripted::Text(
            r#"{"holds": true, "severity": "low", "reason": "real, but minor"}"#,
        )]);
        step(&runner).unwrap(); // the judge's one-shot

        let all = rig.claude.all_seen();
        let judge = all
            .iter()
            .find(|s| s.call.role == Role::Judge)
            .expect("the judge ran");
        let denied: Vec<&str> = judge.settings["permissions"]["deny"]
            .as_array()
            .expect("a deny list")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        for tool in ["Bash", "Read", "Edit", "Write", "WebFetch"] {
            assert!(
                denied.contains(&tool),
                "{tool} should be denied to the judge"
            );
        }
    }

    #[test]
    fn the_claude_round_keeps_its_read_tools_to_check_its_own_work() {
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

        let all = rig.claude.all_seen();
        let reviewer = all
            .iter()
            .find(|s| s.call.role == Role::Reviewer)
            .expect("a reviewer round ran");
        assert_eq!(
            reviewer.settings,
            serde_json::json!({}),
            "no tool is denied, unlike the judge's"
        );
    }
}
