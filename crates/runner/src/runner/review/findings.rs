//! Writing a round's findings to the file its fix turn reads

use std::path::{Path, PathBuf};

use super::attached::tests_folder;
use super::calls::severity_tag;
use crate::ports::{Finding, parse_findings};
use crate::runner::turn;
use crate::state::FailingTest;
use crate::worker_folder;

/// Where a round's findings are written for the worker's next turn
const FINDINGS_FILE: &str = "review-findings.md";

/// Where the worker copies the findings it leaves unfixed
const DEFERRED_FILE: &str = "deferred-findings.md";

/// Where the worker copies the findings it shows are wrong, each with its reason
const DISPUTED_FILE: &str = "disputed-findings.md";

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

/// Where the worker copies a finding it shows is wrong, in the same folder
/// as the findings file
pub(in crate::runner) fn disputed_path(build: &Path) -> PathBuf {
    build.join(DISPUTED_FILE)
}

/// A finding the worker copied into the disputed findings file, with the
/// reason on the line after it
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runner) struct Disputed {
    /// The finding, as the worker copied it
    pub finding: Finding,
    /// The reason, when the next line is indented by two spaces and says
    /// something
    pub reason: Option<String>,
}

/// The findings the worker copied into the disputed findings file in `build`,
/// each with its reason: none when there is no file
///
/// # Errors
///
/// A message naming the file when it is there but cannot be read.
pub(in crate::runner) fn disputed(build: &Path) -> Result<Vec<Disputed>, String> {
    let text = match worker_folder::read(build, Path::new(DISPUTED_FILE)) {
        Ok(text) => text.unwrap_or_default(),
        Err(e) => {
            return Err(format!(
                "cannot read {}: {e}",
                disputed_path(build).display()
            ));
        }
    };
    Ok((with_reasons(&text).into_iter())
        .map(|(finding, reason)| Disputed { finding, reason })
        .collect())
}

// The findings in `text`, each with the reason the worker gave on the line
// after it: indented by two spaces, and saying something.
fn with_reasons(text: &str) -> Vec<(Finding, Option<String>)> {
    let mut read: Vec<(Finding, Option<String>)> = Vec::new();
    for line in text.lines() {
        if let Some(finding) = parse_findings(line).pop() {
            read.push((finding, None));
        } else if let Some(reason) = line.strip_prefix("  ")
            && let Some(last) = read.last_mut().filter(|(_, reason)| reason.is_none())
        {
            last.1 = Some(reason.trim().to_owned()).filter(|r| !r.is_empty());
        }
    }
    read
}

/// The findings the worker copied into the deferred findings file in
/// `build`, as it wrote them: none when there is no file
///
/// # Errors
///
/// As [`deferred_with_reasons`].
pub(in crate::runner) fn deferred(build: &Path) -> Result<Vec<Finding>, String> {
    let read = deferred_with_reasons(build)?;
    Ok(read.into_iter().map(|d| d.finding).collect())
}

/// The findings the worker copied into the deferred findings file in
/// `build`, each with its reason: none when there is no file
///
/// # Errors
///
/// A message naming the file when it is there but cannot be read.
pub(in crate::runner) fn deferred_with_reasons(build: &Path) -> Result<Vec<Deferred>, String> {
    let text = match worker_folder::read(build, Path::new(DEFERRED_FILE)) {
        Ok(text) => text.unwrap_or_default(),
        Err(e) => {
            return Err(format!(
                "cannot read {}: {e}",
                deferred_path(build).display()
            ));
        }
    };
    Ok((with_reasons(&text).into_iter())
        .map(|(finding, reason)| Deferred { finding, reason })
        .collect())
}

/// Takes the first finding `is` names, and the reason under it, out of the deferred
/// findings file in `build`. A file that does not name one is left as it is.
///
/// # Errors
///
/// A message naming the file when it cannot be read or written.
pub(in crate::runner) fn drop_deferred(
    build: &Path,
    is: impl Fn(&Finding) -> bool,
) -> Result<(), String> {
    let path = deferred_path(build);
    let text = match worker_folder::read(build, Path::new(DEFERRED_FILE)) {
        Ok(text) => text.unwrap_or_default(),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let (mut kept, mut dropped) = (String::new(), false);
    let mut reason_of_dropped = false;
    for line in text.split_inclusive('\n') {
        let named = parse_findings(line).pop();
        if !dropped && named.as_ref().is_some_and(&is) {
            (dropped, reason_of_dropped) = (true, true);
            continue;
        }
        // The reason is the line under the finding, if indented by two spaces.
        if reason_of_dropped && line.starts_with("  ") {
            reason_of_dropped = false;
            continue;
        }
        reason_of_dropped = false;
        kept.push_str(line);
    }
    if !dropped {
        return Ok(());
    }
    turn::write(build, &path, &kept)
}

/// A finding the worker copied into the deferred findings file, with the
/// reason on the line after it
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runner) struct Deferred {
    /// The finding, as the worker copied it
    pub finding: Finding,
    /// The reason, when the next line is indented by two spaces and says
    /// something
    pub reason: Option<String>,
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
    // A version 13 state file can hold a size-skipped file's line among
    // the findings sent, which no worker defers.
    let fixable: Vec<&Finding> = (sent.findings.iter())
        .filter(|f| !f.is_skipped_for_size())
        .collect();
    if fixable.is_empty() {
        return Ok(false);
    }
    let mut since = deferred(build)?;
    for before in sent.deferred_before {
        if let Some(at) = since.iter().position(|d| d.is_same_as(before)) {
            since.remove(at);
        }
    }
    Ok(fixable
        .iter()
        .all(|f| since.iter().any(|d| d.is_same_as(f))))
}

pub(in crate::runner) fn write_findings_file(
    folder: &Path,
    path: &Path,
    round: u32,
    findings: &[Finding],
    numbered: bool,
) -> Result<(), String> {
    let (deferred, disputed) = (deferred_path(folder), disputed_path(folder));
    let mut text = format!(
        "Round {round}'s findings, at the reviewer's severity. Fix each one, then \
         commit and push. A finding that is out of scope for this pull request may be \
         left: copy its line, as it stands here, onto a line of its own in {}, and on the \
         next line, indented by two spaces, give your reason for leaving it. Kelpie \
         files what is there as an issue once the pull request merges, except a LOW \
         one, which is dropped. A finding you show is wrong is not fixed or deferred: \
         copy its line, as it stands here, into {}, and on the next line, indented by \
         two spaces, give your reason, naming its evidence: a file and line, a command \
         you ran and what it printed, or the version that settles it. The next reviewer \
         is shown your reason, and a finding it raises again is sent to you once more. A \
         finding of any severity, a low one included, that needs a design choice the issue \
         and the docs do not settle is a question for the maintainer, asked in a \
         `<kelpie-question>` block as your instructions say, rather than a guess or a \
         silent skip.",
        deferred.display(),
        disputed.display()
    );
    if numbered {
        text.push_str(
            " Each line starts with its thread number, `thread <n>|`, which is how you \
             report on it.",
        );
    }
    if findings.iter().any(|f| f.test.is_some()) {
        text.push_str(&format!(
            " A line with a fifth field names a failing test the reviewer wrote for it, \
             which is not in your worktree: read it at {}/<that path>.",
            tests_folder(folder).display()
        ));
    }
    text.push_str("\n\n");
    for (at, f) in findings.iter().enumerate() {
        if numbered {
            text.push_str(&format!("thread {}|", at + 1));
        }
        let test = (f.test.as_ref()).map_or_else(String::new, |t| format!("|{t}"));
        text.push_str(&format!(
            "{}|{}:{}|{}|{}{test}\n",
            severity_tag(f.severity),
            f.file,
            f.line,
            f.what,
            f.why
        ));
    }
    turn::write(folder, path, &text)?;
    // Lines an earlier round's worker left there answer nothing in this one.
    turn::write(folder, &disputed, "")
}

// What the worker is asked to end its reply with when the findings are a
// review bot's threads. Kelpie reads the lines (`thread_lines`) and replies
// on each thread for the worker, resolving the ones it fixed.
const THREAD_LINES: &str = " Each finding is a review bot's thread, and its line in the file starts \
     with its number, `thread <n>|`. Kelpie replies on each thread for you and \
     resolves the ones you fixed, so end your final reply with one line per \
     finding, using the number its line starts with: `thread <n>: fixed` or \
     `thread <n>: declined: <the reason>`. A finding with no line is replied to as \
     not addressed, and stays open.";

pub(super) fn fix_prompt(
    number: u64,
    round: u32,
    count: usize,
    path: &Path,
    threads: bool,
) -> String {
    let lines = if threads { THREAD_LINES } else { "" };
    format!(
        "Round {round} of the review on your pull request #{number} found \
         {count} finding(s), in {}. Fix each one, then commit and push with \
         `git push origin HEAD`. {}{lines}",
        path.display(),
        turn::KEEP_BODY_TRUE
    )
}

pub(in crate::runner) fn again_prompt(
    number: u64,
    round: u32,
    path: &Path,
    threads: bool,
) -> String {
    let lines = if threads { THREAD_LINES } else { "" };
    format!(
        "Your last turn on pull request #{number} pushed nothing, so round {round}'s \
         findings in {} still hold. Fix each one, then commit and push with \
         `git push origin HEAD`. {}{lines}",
        path.display(),
        turn::KEEP_BODY_TRUE
    )
}

// The second fix turn for a reviewer's test the first fix left failing. The
// tests are the reviewer's, so the worker reads them and changes the code.
pub(in crate::runner) fn still_failing_prompt(
    number: u64,
    round: u32,
    build: &Path,
    failing: &[FailingTest],
) -> String {
    let mut prompt = format!(
        "Your fix for round {round} of the review on pull request #{number} left \
         {} of the reviewer's failing tests still failing. Change the code so each \
         passes, and leave the test alone: it is the reviewer's, and an edit to it \
         is not run.",
        failing.len()
    );
    for failed in failing {
        prompt.push_str(&format!(
            "\n\nThe test {} is kept at {}/{}. Its output ended:\n{}",
            failed.test,
            tests_folder(build).display(),
            failed.test,
            failed.output.trim_end()
        ));
    }
    prompt.push_str(&format!(
        "\n\nCommit and push with `git push origin HEAD`. {}",
        turn::KEEP_BODY_TRUE
    ));
    prompt
}

#[cfg(test)]
mod tests {
    use crate::test::ticket;
    #[cfg(unix)]
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
            Finding::new(
                Severity::High,
                "src/lib.rs",
                9,
                "looks racy",
                "two threads write the same field",
            ),
            Finding::new(
                Severity::Low,
                "src/main.rs",
                1,
                "unused import",
                "dead code",
            ),
        ];
        write_findings_file(dir.path(), &path, 3, &findings, false).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("Round 3's findings, at the reviewer's severity."),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "in {}, and on the next line, indented by two spaces, give your reason for \
                 leaving it. Kelpie files what is there as an issue",
                dir.path().join("deferred-findings.md").display()
            )),
            "the header names where a finding left unfixed goes: {text}"
        );
        assert!(
            text.contains(&format!(
                "into {}, and on the next line",
                dir.path().join("disputed-findings.md").display()
            )),
            "the header names where a finding shown to be wrong goes: {text}"
        );
        assert!(
            text.contains(
                "A finding of any severity, a low one included, that needs a design choice \
                 the issue and the docs do not settle is a question for the maintainer, asked \
                 in a `<kelpie-question>` block as your instructions say"
            ),
            "the header tells a fix turn what to do with a finding that needs a design choice: \
             {text}"
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
    fn a_findings_test_is_its_fifth_field_and_the_header_says_where_to_read_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("review-findings.md");
        let with = bot_finding("first").with_test("tests/flag.rs");
        let without = bot_finding("second");

        write_findings_file(
            dir.path(),
            &path,
            1,
            &[with.clone(), without.clone()],
            false,
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("\nMEDIUM|src/lib.rs:3|first|it matters|tests/flag.rs\n"),
            "{text}"
        );
        assert!(
            text.contains("\nMEDIUM|src/lib.rs:3|second|it matters\n"),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "read it at {}/<that path>",
                dir.path().join("review-tests").display()
            )),
            "the header says where the reviewer's file is: {text}"
        );
        assert_eq!(parse_findings(&text), [with, without]);

        write_findings_file(dir.path(), &path, 1, &[bot_finding("third")], false).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("review-tests"), "{text}");
    }

    fn bot_finding(what: &str) -> Finding {
        Finding::new(Severity::Medium, "src/lib.rs", 3, what, "it matters")
    }

    // A bot's findings are its threads, which the worker names by number: each
    // line starts with its own, and the header says so.
    #[test]
    fn a_bots_threads_are_numbered_from_one_at_the_start_of_each_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("review-findings.md");
        let findings = [bot_finding("first"), bot_finding("second")];
        write_findings_file(dir.path(), &path, 3, &findings, true).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("Each line starts with its thread number"),
            "{text}"
        );
        assert!(
            text.contains("\nthread 1|MEDIUM|src/lib.rs:3|first|it matters\n"),
            "{text}"
        );
        assert!(
            text.contains("\nthread 2|MEDIUM|src/lib.rs:3|second|it matters\n"),
            "{text}"
        );

        // The numbers are the worker's, so a line copied into the deferred
        // file with its number still reads as the finding it was.
        assert_eq!(parse_findings(&text), findings);
    }

    #[test]
    fn the_fix_prompt_names_the_round_the_pull_request_and_the_file() {
        let prompt = fix_prompt(
            71,
            3,
            2,
            Path::new("/k/targets/shep/7/review-findings.md"),
            false,
        );
        assert_eq!(
            prompt,
            "Round 3 of the review on your pull request #71 found 2 \
             finding(s), in /k/targets/shep/7/review-findings.md. Fix each one, then \
             commit and push with `git push origin HEAD`. Update the pull request's \
             body so it matches the head you push: what changed, the files touched, the \
             test evidence, and the decisions you made."
        );
    }

    #[test]
    fn a_review_fix_prompt_asks_for_the_body_to_be_kept_true() {
        let path = Path::new("/k/targets/shep/7/review-findings.md");
        let ask = "Update the pull request's body so it matches the head you push";
        for threads in [false, true] {
            let fix = fix_prompt(71, 3, 2, path, threads);
            let again = again_prompt(71, 3, path, threads);
            assert!(fix.contains(ask), "{fix}");
            assert!(again.contains(ask), "{again}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_findings_file_that_cannot_be_written_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("readonly");
        std::fs::create_dir(&folder).unwrap();
        let mut perms = std::fs::metadata(&folder).unwrap().permissions();
        perms.set_mode(0o555);
        std::fs::set_permissions(&folder, perms).unwrap();

        let finding = Finding::new(Severity::Low, "a.rs", 1, "nit", "style");
        let err = write_findings_file(
            &folder,
            &folder.join("review-findings.md"),
            1,
            &[finding],
            false,
        )
        .unwrap_err();
        assert!(err.contains("review-findings.md"), "{err}");

        // Restore write access so the tempdir can clean itself up.
        let mut perms = std::fs::metadata(&folder).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&folder, perms).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_findings_file_that_links_to_another_file_is_replaced_and_that_file_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (build, hook) = (dir.path().join("build"), dir.path().join("pre-commit"));
        std::fs::create_dir(&build).unwrap();
        std::fs::write(&hook, "#!/bin/sh\n").unwrap();
        std::os::unix::fs::symlink(&hook, findings_path(&build)).unwrap();
        let finding = Finding::new(Severity::Low, "a.rs", 1, "nit", "style");

        write_findings_file(&build, &findings_path(&build), 1, &[finding], false).unwrap();

        assert_eq!(std::fs::read_to_string(&hook).unwrap(), "#!/bin/sh\n");
        let text = std::fs::read_to_string(findings_path(&build)).unwrap();
        assert!(text.contains("a.rs:1"), "{text}");
    }

    #[cfg(unix)]
    #[test]
    fn a_deferred_file_that_links_to_another_file_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let (build, theirs) = (dir.path().join("build"), dir.path().join("notes.md"));
        std::fs::create_dir(&build).unwrap();
        let line = "HIGH|src/lib.rs:3|a thing|it is wrong\n";
        std::fs::write(&theirs, line).unwrap();
        std::os::unix::fs::symlink(&theirs, deferred_path(&build)).unwrap();

        let err = deferred(&build).unwrap_err();

        assert!(err.contains("deferred-findings.md"), "{err}");
        assert!(err.contains("not a regular file"), "{err}");
        assert!(!err.contains("a thing"), "{err}");
    }

    #[test]
    fn a_deferred_line_reads_back_with_the_reason_on_the_line_after_it() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (
            "HIGH|src/a.rs:1|first|why a\n",
            "MEDIUM|src/b.rs:2|second|why b\n",
            "MEDIUM|src/c.rs:3|third|why c\n",
        );
        let text = format!("{a}  the fix is a new module\n{b}{c}  \n");
        std::fs::write(deferred_path(dir.path()), text).unwrap();

        let read = deferred_with_reasons(dir.path()).unwrap();

        let reasons: Vec<_> = read.iter().map(|d| d.reason.as_deref()).collect();
        assert_eq!(reasons, [Some("the fix is a new module"), None, None]);
        let found: Vec<_> = read.into_iter().map(|d| d.finding).collect();
        assert_eq!(found, parse_findings(&format!("{a}{b}{c}")));
    }

    #[test]
    fn the_header_asks_for_the_reason_of_a_deferral_on_the_next_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("review-findings.md");
        let finding = Finding::new(Severity::High, "src/lib.rs", 9, "looks racy", "it is");

        write_findings_file(dir.path(), &path, 1, &[finding], false).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains(&format!(
                "onto a line of its own in {}, and on the next line, indented by two \
                 spaces, give your reason for leaving it.",
                dir.path().join("deferred-findings.md").display()
            )),
            "{text}"
        );
    }

    // The path a findings header names for `name`, which it holds between a
    // space and the punctuation after it
    fn header_path(text: &str, name: &str) -> PathBuf {
        let at = text.find(name).expect(name);
        let start = text[..at].rfind(' ').expect("a path before the name");
        PathBuf::from(&text[start + 1..at + name.len()])
    }

    // A fix turn learns where a deferred finding goes from the header of the
    // findings file it was sent to, and that is the path kelpie reads at merge.
    #[test]
    fn the_deferred_path_the_findings_header_names_is_the_one_read_at_merge() {
        let (rig, _runner) = findings_sent();
        let text = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
        let named = header_path(&text, "deferred-findings.md");
        assert_eq!(named, rig.build_7().join("deferred-findings.md"));
        let line = "MEDIUM|src/lib.rs:3|the flag is misnamed|it reads as its opposite\n";
        std::fs::write(&named, line).unwrap();

        let read = deferred(&rig.build_7()).unwrap();

        assert_eq!(read, parse_findings(line));
    }

    // Round 1 on pull request 71 found one MEDIUM finding, and its fix turn is next.
    fn findings_sent() -> (Rig, Mutex<Runner>) {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        rig.ask(&runner, "add", Some("7"));
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        rig.claude.script([Scripted::Push("work.txt", "work\n")]);
        step(&runner).unwrap(); // opens the pull request, enters round 1 (qwen)
        rig.reviewer
            .script([ScriptedRound::Findings(vec![Finding::new(
                Severity::Medium,
                "src/lib.rs",
                3,
                "the flag is misnamed",
                "it reads as its opposite",
            )])]);
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
                issue: ticket(7),
                pull_request: 71,
                round: 1,
                head: fixed,
                checks: Vec::new(),
                unchecked: 0,
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
                "prompt": again_prompt(71, 1, &path, false),
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
                 `git push origin HEAD`. Update the pull request's body so it matches the \
                 head you push: what changed, the files touched, the test evidence, and \
                 the decisions you made.",
                rig.build_7().join("review-findings.md").display()
            )
        );
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::FixPushed {
                issue: ticket(7),
                pull_request: 71,
                round: 1,
                head: rig.forge.head_of("kelpie/7"),
                checks: Vec::new(),
                unchecked: 0,
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
                issue: ticket(7),
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

    // A version 13 state file's fix turn may have been sent a size-skipped
    // file's notice beside its findings: no worker defers that, so deferring
    // the rest defers them all.
    #[test]
    fn a_size_skipped_files_notice_sent_by_an_older_kelpie_need_not_be_deferred() {
        let dir = tempfile::tempdir().unwrap();
        let medium = "MEDIUM|src/lib.rs:3|the flag is misnamed|it reads as its opposite\n";
        let notice = "LOW|src/big.rs:0|not reviewed: 900 lines exceeds the chunk limit|split it\n";
        let sent = parse_findings(&format!("{medium}{notice}"));
        std::fs::write(dir.path().join("deferred-findings.md"), medium).unwrap();
        let sent = Sent {
            findings: &sent,
            deferred_before: &[],
        };
        assert_eq!(all_deferred(dir.path(), sent), Ok(true));
        let only_notice = parse_findings(notice);
        let sent = Sent {
            findings: &only_notice,
            deferred_before: &[],
        };
        assert_eq!(all_deferred(dir.path(), sent), Ok(false));
    }

    #[test]
    fn a_deferral_an_earlier_round_left_does_not_defer_this_rounds_finding() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        rig.ask(&runner, "add", Some("7"));
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        rig.claude.script([Scripted::Push("work.txt", "work\n")]);
        step(&runner).unwrap(); // opens the pull request, enters round 1 (qwen)
        // The same line, deferred before this round's findings are sent.
        let line = "MEDIUM|src/lib.rs:3|the flag is misnamed|it reads as its opposite\n";
        std::fs::write(rig.build_7().join("deferred-findings.md"), line).unwrap();
        rig.reviewer
            .script([ScriptedRound::Findings(vec![Finding::new(
                Severity::Medium,
                "src/lib.rs",
                3,
                "the flag is misnamed",
                "it reads as its opposite",
            )])]);
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
