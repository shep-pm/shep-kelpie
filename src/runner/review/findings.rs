//! Writing a round's findings to the file its fix turn reads

use std::path::{Path, PathBuf};

use super::calls::severity_tag;
use crate::ports::{Finding, parse_findings};
use crate::runner::turn;

/// Where a round's findings are written for the worker's next turn
const FINDINGS_FILE: &str = "review-findings.md";

/// Where the worker copies the findings it leaves unfixed
const DEFERRED_FILE: &str = "deferred-findings.md";

// The worker can read its build folder, and a commit never carries it. The
// project's folder holds its settings, so the worker is denied it.
/// The findings file's path, in the work item's build folder
pub(in crate::runner) fn findings_path(build: &Path) -> PathBuf {
    build.join(FINDINGS_FILE)
}

/// Where the worker copies a finding it leaves unfixed, in the same
/// folder as the findings file
pub(in crate::runner) fn deferred_path(build: &Path) -> PathBuf {
    build.join(DEFERRED_FILE)
}

/// The findings the worker copied into the deferred findings file in
/// `build`, as it wrote them: none when there is no file
///
/// # Errors
///
/// A message naming the file when it is there but cannot be read.
pub(in crate::runner) fn deferred(build: &Path) -> Result<Vec<Finding>, String> {
    let path = deferred_path(build);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(parse_findings(&text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("cannot read {}: {}", path.display(), e.kind())),
    }
}

/// The findings a round sent the worker, and the deferred findings file's
/// lines when it sent them
#[derive(Debug, Clone, Copy)]
pub(in crate::runner) struct Sent<'a> {
    /// The findings sent
    pub findings: &'a [Finding],
    /// The deferred findings file's lines then
    pub deferred_before: &'a [Finding],
}

/// Whether the worker deferred every finding `sent` holds since they were
/// sent, which is never so when none were
///
/// A line the file held before counts once against the lines now, so a
/// deferral left by an earlier round defers nothing in this one.
///
/// # Errors
///
/// As [`deferred`].
pub(in crate::runner) fn all_deferred(build: &Path, sent: Sent<'_>) -> Result<bool, String> {
    if sent.findings.is_empty() {
        return Ok(false);
    }
    let mut since = deferred(build)?;
    for before in sent.deferred_before {
        if let Some(at) = since.iter().position(|d| d.is_same_as(before)) {
            since.remove(at);
        }
    }
    Ok(sent
        .findings
        .iter()
        .all(|f| since.iter().any(|d| d.is_same_as(f))))
}

pub(in crate::runner) fn write_findings_file(
    folder: &Path,
    path: &Path,
    round: u32,
    findings: &[Finding],
) -> Result<(), String> {
    let deferred = deferred_path(folder);
    let mut text = format!(
        "Round {round}'s findings, at the reviewer's severity. Fix each one, then \
         commit and push. A finding that is out of scope for this pull request may be \
         left: copy its line, as it stands here, onto a line of its own in {}. Kelpie \
         files what is there as an issue once the pull request merges.\n\n",
        deferred.display()
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
    turn::write(folder, path, &text)
}

pub(super) fn fix_prompt(number: u64, round: u32, count: usize, path: &Path) -> String {
    format!(
        "Round {round} of the review on your pull request #{number} found \
         {count} finding(s), in {}. Fix each one, then commit and push with \
         `git push origin HEAD`.",
        path.display()
    )
}

pub(in crate::runner) fn again_prompt(number: u64, round: u32, path: &Path) -> String {
    format!(
        "Your last turn on pull request #{number} pushed nothing, so round {round}'s \
         findings in {} still hold. Fix each one, then commit and push with \
         `git push origin HEAD`.",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::ports::{AgentError, Severity};
    use crate::runner::{Runner, StepReport, step};
    use crate::test::{Rig, Scripted, ScriptedRound};

    #[test]
    fn the_file_lists_one_finding_per_line_in_qwens_own_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("review-findings.md");
        let findings = [
            Finding {
                severity: Severity::High,
                file: "src/lib.rs".into(),
                line: 9,
                what: "looks racy".into(),
                why: "two threads write the same field".into(),
            },
            Finding {
                severity: Severity::Low,
                file: "src/main.rs".into(),
                line: 1,
                what: "unused import".into(),
                why: "dead code".into(),
            },
        ];
        write_findings_file(dir.path(), &path, 3, &findings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("Round 3's findings, at the reviewer's severity."),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "in {}. Kelpie files what is there as an issue",
                dir.path().join("deferred-findings.md").display()
            )),
            "the header names where a finding left unfixed goes: {text}"
        );
        assert!(
            text.contains("HIGH|src/lib.rs:9|looks racy|two threads write the same field\n"),
            "{text}"
        );
        assert!(
            text.contains("LOW|src/main.rs:1|unused import|dead code\n"),
            "{text}"
        );
    }

    #[test]
    fn the_fix_prompt_names_the_round_the_pull_request_and_the_file() {
        let prompt = fix_prompt(71, 3, 2, Path::new("/k/targets/shep/7/review-findings.md"));
        assert_eq!(
            prompt,
            "Round 3 of the review on your pull request #71 found 2 \
             finding(s), in /k/targets/shep/7/review-findings.md. Fix each one, then \
             commit and push with `git push origin HEAD`."
        );
    }

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

    // Round 1 on pull request 71 found one MEDIUM finding, and its fix turn is next.
    fn findings_sent() -> (Rig, Mutex<Runner>) {
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
            what: "the flag is misnamed".into(),
            why: "it reads as its opposite".into(),
        }])]);
        step(&runner).unwrap(); // round 1's qwen call
        step(&runner).unwrap(); // the findings go out: the fix turn is next
        (rig, runner)
    }

    #[test]
    fn the_worker_can_read_its_findings_under_the_settings_it_runs_with() {
        let (rig, runner) = findings_sent();
        rig.claude.script([Scripted::Push("fixed.txt", "fixed\n")]);
        step(&runner).unwrap(); // the fix turn
        let fix = rig.claude.seen().pop().unwrap();
        let named = fix.call.prompt.split(" in ").nth(1).unwrap();
        let path = Path::new(named.split(". Fix").next().unwrap());

        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("the flag is misnamed"), "{text}");
        rig.assert_worker_reads(&fix, path);
    }

    #[test]
    fn a_fix_the_forge_has_not_shown_yet_still_counts() {
        let (rig, runner) = findings_sent();
        let before = rig.forge.head_of("kelpie/7").unwrap();
        rig.forge.set_lagging(71, Some(&before));
        rig.claude.script([Scripted::Push("fixed.txt", "fixed\n")]);
        step(&runner).unwrap(); // the fix turn
        let fixed = rig.forge.head_of("kelpie/7");
        assert_ne!(fixed.as_deref(), Some(before.as_str()));
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::FixPushed {
                issue: 7,
                pull_request: 71,
                round: 1,
                head: fixed,
            })
        );
    }

    #[test]
    fn a_fix_turn_that_pushes_nothing_parks_and_a_yes_sends_the_findings_again() {
        let (rig, runner) = findings_sent();
        rig.claude
            .script([Scripted::Say("I can't read the findings file.")]);
        step(&runner).unwrap(); // the fix turn ends with nothing pushed

        let Some(StepReport::Ruling { id, question, .. }) = step(&runner).unwrap() else {
            panic!("a fix with nothing pushed raised no ruling");
        };
        assert!(
            question.starts_with(
                "The worker on pull request #71 ended its fix for round 1 of the \
                 review without pushing, so those findings still hold."
            ),
            "{question}"
        );
        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["work_item"]["phase"]["state"], "ruling");
        let path = rig.build_7().join("review-findings.md");
        assert_eq!(
            status["rulings"][0]["kind"],
            json!({
                "kind": "stuck",
                "reason": "fix-not-pushed",
                "review": {
                    "round": 1,
                    "reviewer": "qwen",
                    "stage": {
                        "stage": "fixing",
                        "head": rig.forge.head_of("kelpie/7"),
                        "sent": [{
                            "severity": "medium",
                            "file": "src/lib.rs",
                            "line": 3,
                            "what": "the flag is misnamed",
                            "why": "it reads as its opposite",
                        }],
                    },
                },
                "prompt": again_prompt(71, 1, &path),
            })
        );
        assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id }));
        assert_eq!(
            step(&runner).unwrap(),
            None,
            "parked until the maintainer rules"
        );

        rig.ask(&runner, "rule", Some(&format!("{id} yes")));
        rig.claude.script([Scripted::Push("fixed.txt", "fixed\n")]);
        step(&runner).unwrap(); // the fix turn again
        let again = rig.claude.calls().pop().unwrap();
        assert_eq!(
            again.prompt,
            format!(
                "Your last turn on pull request #71 pushed nothing, so round 1's \
                 findings in {} still hold. Fix each one, then commit and push with \
                 `git push origin HEAD`.",
                rig.build_7().join("review-findings.md").display()
            )
        );
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::FixPushed {
                issue: 7,
                pull_request: 71,
                round: 1,
                head: rig.forge.head_of("kelpie/7"),
            })
        );
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["phase"],
            json!({
                "state": "review",
                "round": 2,
                "stage": { "stage": "round" },
                "ran": ["qwen"],
            }),
            "the next reviewer in the list reads the fix"
        );
    }

    #[test]
    fn a_fix_turn_that_defers_every_finding_goes_on_to_the_next_reviewer() {
        let (rig, runner) = findings_sent();
        // The worker copies the finding's line as the findings file holds it.
        let line = "MEDIUM|src/lib.rs:3|the flag is misnamed|it reads as its opposite\n";
        std::fs::write(rig.build_7().join("deferred-findings.md"), line).unwrap();
        rig.claude
            .script([Scripted::Say("That is out of scope, so I deferred it.")]);
        step(&runner).unwrap(); // the fix turn ends with nothing pushed
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::FindingsDeferred {
                issue: 7,
                pull_request: 71,
                round: 1,
                deferred: 1,
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
            }),
            "the next reviewer in the list reads the pull request as it stands"
        );
    }

    #[test]
    fn a_deferral_an_earlier_round_left_does_not_defer_this_rounds_finding() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "add", Some("7"));
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        rig.claude.script([Scripted::Push("work.txt", "work\n")]);
        step(&runner).unwrap(); // opens the pull request, enters round 1 (qwen)
        // The same line, deferred before this round's findings are sent.
        let line = "MEDIUM|src/lib.rs:3|the flag is misnamed|it reads as its opposite\n";
        std::fs::write(rig.build_7().join("deferred-findings.md"), line).unwrap();
        rig.reviewer.script([ScriptedRound::Findings(vec![Finding {
            severity: Severity::Medium,
            file: "src/lib.rs".into(),
            line: 3,
            what: "the flag is misnamed".into(),
            why: "it reads as its opposite".into(),
        }])]);
        step(&runner).unwrap(); // round 1's qwen call
        step(&runner).unwrap(); // the findings go out: the fix turn is next
        rig.claude.script([Scripted::Say("Already deferred.")]);
        step(&runner).unwrap(); // the fix turn ends with nothing pushed
        let report = step(&runner).unwrap();
        assert!(
            matches!(report, Some(StepReport::Ruling { .. })),
            "{report:?}"
        );
        let deferred = std::fs::read_to_string(rig.build_7().join("deferred-findings.md"));
        assert_eq!(
            deferred.unwrap(),
            line,
            "the file is kept for the follow-ups"
        );
    }

    #[test]
    fn a_fix_turn_that_defers_only_some_findings_and_pushes_nothing_still_parks() {
        let (rig, runner) = findings_sent();
        let other = "MEDIUM|src/lib.rs:9|another thing|it is wrong\n";
        std::fs::write(rig.build_7().join("deferred-findings.md"), other).unwrap();
        rig.claude.script([Scripted::Say("Done.")]);
        step(&runner).unwrap(); // the fix turn ends with nothing pushed
        let report = step(&runner).unwrap();
        assert!(
            matches!(report, Some(StepReport::Ruling { .. })),
            "{report:?}"
        );
    }

    #[test]
    fn a_timed_out_fix_turn_resumes_that_round() {
        let (rig, runner) = findings_sent();
        rig.claude.script([Scripted::Fail(AgentError::TimedOut(
            crate::settings::Harness::ClaudeCode,
        ))]);
        let Some(StepReport::TimedOut { id, .. }) = step(&runner).unwrap() else {
            panic!("the fix turn did not time out");
        };
        step(&runner).unwrap(); // the alert

        rig.ask(&runner, "rule", Some(&format!("{id} yes")));
        rig.claude.script([Scripted::Say("Still thinking.")]);
        step(&runner).unwrap(); // the resumed fix turn ends with nothing pushed
        let Some(StepReport::Ruling { question, .. }) = step(&runner).unwrap() else {
            panic!("the resumed fix was not checked for a push");
        };
        assert!(
            question.contains("round 1 of the review without pushing"),
            "{question}"
        );
    }
}
