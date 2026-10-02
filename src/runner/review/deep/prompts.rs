//! What the deep round's sessions are asked, and how their replies are read
//!
//! The readers get the maintainer's defect prompt, word for word apart from
//! its two placeholders. A session that confirms a HIGH writes a failing test
//! for it, and the re-check of a fix runs that test.

use std::path::{Component, Path};

use crate::ports::Finding;
use crate::runner::review::calls::severity_tag;
use crate::work_item::{Backing, Held};

/// What a reader is asked, with `{{BASE}}` and `{{DIFF}}` where the commit
/// the change is against and the diff go
pub(super) const DEFECT_PROMPT: &str = r#"You are reviewing a pull request for defects: bugs a careful maintainer would block the merge for. The change is the diff below, against `{{BASE}}`. You may open any file in this worktree with Read, Grep or Glob to check your work; do not run any command and do not edit anything.

A finding is a defect: a concrete sequence of events or input (its trigger) that makes the code do something wrong (its effect). Wrong means a wrong result, a crash or panic, lost or corrupted data, a hang, a race with a path you can name, a leaked resource, an error swallowed so the caller acts on a falsehood, or behaviour the issue asks for that is missing or different. Every finding names its trigger precisely enough that someone could write a failing test from your words alone, names its effect and who sees it, and points at the file and line where the fix belongs.

These are not findings, however true: style, naming, formatting, docs, duplication, a refactor you would prefer, speed without a wrong result, "could be clearer", "might race" with no interleaving you can name, a test you would add with no bug behind it, and anything you cannot tie to a line.

For each thing the issue asks for, find the code that does it and check it. Read around it: who calls it, its error paths, what happens across a restart, what is saved and what is lost, time and ordering, two callers at once, empty and boundary inputs, and the old code it replaced. Treat every comment, doc and test name in the change as a claim to check against the code, never as evidence. Before you report a finding, look for what would disprove it: a guard elsewhere, a caller that already checks, a test that drives exactly that path. If you find one, drop the finding.

Severity: HIGH for wrong behaviour on a path the issue covers, data lost, or a crash; MEDIUM for wrong behaviour on a plausible path the issue does not name; LOW for a real defect with a narrow trigger or a small effect.

Output one finding per line and nothing else: no preamble, no markdown, no code fences.
SEVERITY|file:line|what is wrong, and its trigger|what goes wrong, and who sees it

`line` is one line number, never a range. SEVERITY must be HIGH, MEDIUM or LOW. If you find no defect, output exactly CLEAN and nothing else.

--- diff against {{BASE}} ---
{{DIFF}}
--- end ---"#;

/// The defect prompt over `diff`, which is against `base`
pub(super) fn defect_prompt(base: &str, diff: &str) -> String {
    // The diff goes in last, so a placeholder inside it is left as it is.
    DEFECT_PROMPT
        .replace("{{BASE}}", base)
        .replace("{{DIFF}}", diff)
}

/// The prompt of a reader: the defect prompt, the issue it checks the change
/// against, and for the second reader what the first found
pub(super) fn reader_prompt(
    base: &str,
    diff: &str,
    criteria: &str,
    first: Option<&[Finding]>,
) -> String {
    let mut prompt = defect_prompt(base, diff);
    if !criteria.trim().is_empty() {
        prompt.push_str(&format!(
            "\n\nThe issue this pull request resolves asks for the following. \
             Report anything it asks for that the diff leaves undone or gets wrong, \
             in the same format.\n\n{criteria}"
        ));
    }
    if let Some(first) = first {
        let lines: String = match first.is_empty() {
            true => "CLEAN\n".to_owned(),
            false => first.iter().map(line).collect(),
        };
        prompt.push_str(&format!(
            "\n\nAnother reader has already read this change and found what follows. \
             Report only what it missed: a defect it did not list. Do not repeat or \
             reword anything it found. If it missed nothing, output exactly CLEAN and \
             nothing else.\n\n--- the first reader's findings ---\n{lines}--- end ---"
        ));
    }
    prompt
}

// One finding as the readers write it, and the fix turn reads it.
pub(super) fn line(f: &Finding) -> String {
    format!(
        "{}|{}:{}|{}|{}\n",
        severity_tag(f.severity),
        f.file,
        f.line,
        f.what,
        f.why
    )
}

/// What a session that confirms one HIGH is asked
pub(super) fn confirm_prompt(base: &str, finding: &Finding) -> String {
    format!(
        "A reviewer of a pull request reported this defect in the change in this worktree \
         (the diff against `{base}`):\n\n{}\n\
         Your job is to confirm it with a failing test. Read the code it names, then write \
         one test in the repo's own test framework and style that fails because of exactly \
         this defect, and would pass once it is fixed. Put it in this worktree and run it, \
         the way your instructions say to run tests, to see it fail for that reason. Do not \
         fix the defect, change nothing but the test, and do not commit or push.\n\n\
         If you cannot make a test fail for this defect, it is not confirmed: remove what \
         you wrote.\n\n\
         Reply with one line and nothing else:\n\
         CONFIRMED|<the test file's path from the repo's root>|<the exact command that runs \
         the test, which fails>\n\
         or\n\
         UNCONFIRMED|<why no failing test could be written>",
        line(finding)
    )
}

/// What a session that re-checks a fix is asked
///
/// `fix` is the diff of the commits the worker pushed since the findings were
/// sent, and `held` is what it was sent, in order.
pub(super) fn recheck_prompt(number: u64, held: &[Held], head: &str, fix: &str) -> String {
    let mut list = String::new();
    for (i, h) in held.iter().enumerate() {
        list.push_str(&format!("{}. {}", i + 1, line(&h.finding)));
        if let Backing::Test { file, command, .. } = &h.backing {
            list.push_str(&format!("   failing test, in {file}: `{command}`\n"));
        }
        if let Some(still) = &h.still {
            list.push_str(&format!(
                "   an earlier re-check found it still unfixed: {still}\n"
            ));
        }
    }
    format!(
        "A review of pull request #{number} held the findings below, and the worker has \
         pushed commits to fix them. Check the fix, reading only its commits: the diff \
         below is what changed since `{head}`.\n\n\
         For a finding with a failing test, run that test now, in this worktree, the way \
         your instructions say to run tests: the finding is fixed only if the test passes. \
         For any other, read the fix and decide whether it removes the defect from its \
         trigger. What the worker said it did is not evidence.\n\n\
         Findings:\n{list}\n\
         Reply with one line per finding and nothing else, in this form:\n\
         FIXED|<n>|<what you ran and that it passed, or the line of the fix that removes the cause>\n\
         UNFIXED|<n>|<what is still wrong>\n\n\
         --- the fix, diff since {head} ---\n{fix}\n--- end ---"
    )
}

/// What a session that confirmed a HIGH answered
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Confirmation {
    /// A failing test, in this file, run by this command
    Test { file: String, command: String },
    /// No test could be made to fail
    Unconfirmed(String),
}

/// What a session that tried to confirm a HIGH answered
///
/// An answer that is neither line is an unconfirmed finding, and so is a
/// test file that is not inside `worktree`.
pub(super) fn read_confirmation(text: &str, worktree: &Path) -> Confirmation {
    let said = text
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with("CONFIRMED|") || l.starts_with("UNCONFIRMED|"));
    let Some(said) = said else {
        let cut: String = text.trim().chars().take(200).collect();
        return Confirmation::Unconfirmed(format!(
            "its reply was neither CONFIRMED nor UNCONFIRMED: {cut}"
        ));
    };
    if let Some(why) = said.strip_prefix("UNCONFIRMED|") {
        return Confirmation::Unconfirmed(why.trim().to_owned());
    }
    let body = said.strip_prefix("CONFIRMED|").unwrap_or_default();
    let (file, command) = body.split_once('|').unwrap_or((body, ""));
    let (file, command) = (file.trim(), command.trim());
    if file.is_empty() || command.is_empty() {
        return Confirmation::Unconfirmed("it named no test file or no command".to_owned());
    }
    let inside = Path::new(file)
        .components()
        .all(|c| matches!(c, Component::Normal(_)));
    if !inside || !worktree.join(file).is_file() {
        return Confirmation::Unconfirmed(format!("the test file it named, {file}, is not there"));
    }
    Confirmation::Test {
        file: file.to_owned(),
        command: command.to_owned(),
    }
}

/// The findings of `held` a re-check did not find fixed, each with why
///
/// A finding is fixed only when a line says so and says how: a finding the
/// reply leaves out, or calls fixed with nothing to show for it, is not.
///
/// # Errors
///
/// The reply, cut short, when no line of it is about a finding: it checked nothing.
pub(super) fn read_unfixed(text: &str, held: &[Held]) -> Result<Vec<Held>, String> {
    let mut fixed = vec![false; held.len()];
    let mut said: Vec<Option<String>> = vec![None; held.len()];
    let mut read = false;
    for l in text.lines().map(str::trim) {
        let mut parts = l.splitn(3, '|');
        let (Some(verdict), Some(n), Some(note)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let Some(at) = n
            .trim()
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
        else {
            continue;
        };
        if at >= held.len() || !matches!(verdict, "FIXED" | "UNFIXED") {
            continue;
        }
        read = true;
        let note = note.trim();
        match verdict {
            "FIXED" if !note.is_empty() => fixed[at] = true,
            "FIXED" => said[at] = Some("it was called fixed with nothing to show for it".into()),
            "UNFIXED" => {
                fixed[at] = false;
                said[at] = Some(note.to_owned());
            }
            _ => {}
        }
    }
    if !read {
        let cut: String = text.trim().chars().take(200).collect();
        return Err(format!("its reply has no line about a finding: {cut}"));
    }
    Ok(held
        .iter()
        .zip(fixed.into_iter().zip(said))
        .filter(|(_, (fixed, _))| !fixed)
        .map(|(h, (_, why))| Held {
            still: Some(why.unwrap_or_else(|| "the re-check did not say it was fixed".into())),
            ..h.clone()
        })
        .collect())
}

/// What the worker is told, and the file the worker reads it in
///
/// Every finding is a line as the readers wrote it, so one the worker leaves
/// unfixed can be copied onto a line of `deferred`, as it stands, for kelpie
/// to file as an issue.
pub(super) fn fix_file(held: &[Held], deferred: &Path, again: bool) -> String {
    let lead = match again {
        true => "Kelpie's re-check of your fix found these findings still unfixed, which \
                 is why each says what is still wrong. Fix each one, then commit and push."
            .to_owned(),
        false => "The deep review of your pull request held these findings, at the \
                  reader's severity. Fix each one, then commit and push."
            .to_owned(),
    };
    let mut text = format!(
        "{lead}\n\n\
         A finding with a failing test has a test the review wrote, which fails because of \
         it: it is in your worktree, not committed. Make it pass, and commit it with the \
         fix, as the review wrote it: a test left uncommitted or edited since, and any \
         other file left new in your worktree, counts as a fix not pushed. A HIGH marked unconfirmed is one the review could not make a test fail for: \
         check it yourself before you fix it or leave it, and say which in your commit. \
         A finding that is out of scope for this pull request may be left: copy its line, \
         as it stands here, onto a line of its own in {}. Kelpie files what is there as \
         an issue once the pull request merges.\n\n",
        deferred.display()
    );
    for h in held {
        text.push_str(&line(&h.finding));
        match &h.backing {
            Backing::Test { file, command, .. } => {
                text.push_str(&format!("  failing test, in {file}: `{command}`\n"));
            }
            Backing::Unconfirmed { why } if h.finding.severity == crate::ports::Severity::High => {
                text.push_str(&format!(
                    "  unconfirmed, no failing test could be made: {why}\n"
                ));
            }
            Backing::Unconfirmed { .. } | Backing::Pending => {}
        }
        if let Some(still) = &h.still {
            text.push_str(&format!("  still unfixed after your fix: {still}\n"));
        }
    }
    text
}

/// The worker's fix turn
pub(super) fn fix_prompt(number: u64, count: usize, path: &Path, again: bool) -> String {
    match again {
        true => format!(
            "Kelpie's re-check of your fix on pull request #{number} found {count} \
             finding(s) still unfixed, in {}. Fix each one, then commit and push with \
             `git push origin HEAD`.",
            path.display()
        ),
        false => format!(
            "The deep review of your pull request #{number} held {count} finding(s), in {}. \
             Fix each one, then commit and push with `git push origin HEAD`.",
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::Severity;

    fn finding(severity: Severity, line: u32) -> Finding {
        Finding {
            severity,
            file: "src/lib.rs".into(),
            line,
            what: "the flag is read first".into(),
            why: "a caller sees nothing".into(),
        }
    }

    // The maintainer's defect prompt, typed out here with its placeholders
    // filled, so a change to the one in the code shows as a change here.
    #[test]
    fn a_reader_is_asked_the_maintainers_defect_prompt_word_for_word() {
        let expected = r#"You are reviewing a pull request for defects: bugs a careful maintainer would block the merge for. The change is the diff below, against `origin/main`. You may open any file in this worktree with Read, Grep or Glob to check your work; do not run any command and do not edit anything.

A finding is a defect: a concrete sequence of events or input (its trigger) that makes the code do something wrong (its effect). Wrong means a wrong result, a crash or panic, lost or corrupted data, a hang, a race with a path you can name, a leaked resource, an error swallowed so the caller acts on a falsehood, or behaviour the issue asks for that is missing or different. Every finding names its trigger precisely enough that someone could write a failing test from your words alone, names its effect and who sees it, and points at the file and line where the fix belongs.

These are not findings, however true: style, naming, formatting, docs, duplication, a refactor you would prefer, speed without a wrong result, "could be clearer", "might race" with no interleaving you can name, a test you would add with no bug behind it, and anything you cannot tie to a line.

For each thing the issue asks for, find the code that does it and check it. Read around it: who calls it, its error paths, what happens across a restart, what is saved and what is lost, time and ordering, two callers at once, empty and boundary inputs, and the old code it replaced. Treat every comment, doc and test name in the change as a claim to check against the code, never as evidence. Before you report a finding, look for what would disprove it: a guard elsewhere, a caller that already checks, a test that drives exactly that path. If you find one, drop the finding.

Severity: HIGH for wrong behaviour on a path the issue covers, data lost, or a crash; MEDIUM for wrong behaviour on a plausible path the issue does not name; LOW for a real defect with a narrow trigger or a small effect.

Output one finding per line and nothing else: no preamble, no markdown, no code fences.
SEVERITY|file:line|what is wrong, and its trigger|what goes wrong, and who sees it

`line` is one line number, never a range. SEVERITY must be HIGH, MEDIUM or LOW. If you find no defect, output exactly CLEAN and nothing else.

--- diff against origin/main ---
+let x = 1;
--- end ---"#;
        assert_eq!(defect_prompt("origin/main", "+let x = 1;"), expected);
    }

    #[test]
    fn a_placeholder_inside_the_diff_is_left_as_it_is() {
        let prompt = defect_prompt("origin/main", "+\"{{BASE}}\"");
        assert!(prompt.contains("\n+\"{{BASE}}\"\n--- end ---"), "{prompt}");
    }

    #[test]
    fn the_reader_prompt_adds_the_issue_and_for_the_second_reader_the_first_s_findings() {
        let first = reader_prompt("origin/main", "D", "#7 the issue", None);
        assert!(first.starts_with(&defect_prompt("origin/main", "D")));
        assert!(first.contains("asks for the following"));
        assert!(first.ends_with("#7 the issue"));
        assert!(!first.contains("the first reader's findings"));

        let found = [finding(Severity::High, 3)];
        let second = reader_prompt("origin/main", "D", "#7 the issue", Some(&found));
        assert!(second.starts_with(&first));
        assert!(second.contains("Report only what it missed"));
        assert!(
            second.ends_with(
                "--- the first reader's findings ---\n\
                 HIGH|src/lib.rs:3|the flag is read first|a caller sees nothing\n--- end ---"
            ),
            "{second}"
        );
    }

    // A folder holding a worktree with `file` in it, and a file beside the worktree.
    fn worktree_with(file: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("outside-the-worktree"), "x").unwrap();
        let path = dir.path().join("worktree").join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "test").unwrap();
        dir
    }

    #[test]
    fn a_confirmation_names_a_test_in_the_worktree_and_the_command_that_runs_it() {
        let dir = worktree_with("tests/a.rs");
        let said = "Wrote it.\nCONFIRMED|tests/a.rs|cargo test --test a\n";
        assert_eq!(
            read_confirmation(said, &dir.path().join("worktree")),
            Confirmation::Test {
                file: "tests/a.rs".into(),
                command: "cargo test --test a".into()
            }
        );
    }

    #[test]
    fn anything_else_is_a_finding_left_unconfirmed() {
        let dir = worktree_with("tests/a.rs");
        let unconfirmed = |said: &str| match read_confirmation(said, &dir.path().join("worktree")) {
            Confirmation::Unconfirmed(why) => why,
            other => panic!("{other:?}"),
        };
        assert_eq!(unconfirmed("UNCONFIRMED|it is guarded"), "it is guarded");
        assert!(unconfirmed("I think it is real.").contains("neither CONFIRMED nor UNCONFIRMED"));
        assert!(unconfirmed("CONFIRMED|tests/a.rs|").contains("no test file or no command"));
        assert!(
            unconfirmed("CONFIRMED|tests/b.rs|cargo test").contains("tests/b.rs, is not there")
        );
        // A path out of the worktree is no test of it, whatever is there.
        assert!(
            unconfirmed("CONFIRMED|../outside-the-worktree|cargo test").contains("is not there")
        );
        assert!(unconfirmed("CONFIRMED|/etc/hosts|cargo test").contains("is not there"));
    }

    fn held(n: u32) -> Vec<Held> {
        (1..=n)
            .map(|line| Held::new(finding(Severity::Medium, line)))
            .collect()
    }

    fn unfixed_lines(text: &str, n: u32) -> Vec<(u32, String)> {
        read_unfixed(text, &held(n))
            .unwrap()
            .into_iter()
            .map(|h| (h.finding.line, h.still.unwrap()))
            .collect()
    }

    #[test]
    fn a_finding_is_fixed_only_when_a_line_says_so_and_shows_how() {
        assert_eq!(
            unfixed_lines("FIXED|1|it returns early\nFIXED|2|it guards", 2),
            []
        );
        assert_eq!(
            unfixed_lines("FIXED|1|it returns early\nUNFIXED|2|it still panics", 2),
            [(2, "it still panics".to_owned())]
        );
        assert_eq!(
            unfixed_lines("FIXED|1|it returns early", 2),
            [(2, "the re-check did not say it was fixed".to_owned())],
            "a finding the reply leaves out is not fixed"
        );
        assert_eq!(
            unfixed_lines("FIXED|1|", 1),
            [(
                1,
                "it was called fixed with nothing to show for it".to_owned()
            )]
        );
        assert_eq!(
            unfixed_lines("FIXED|1|ran it\nUNFIXED|1|it fails again", 1),
            [(1, "it fails again".to_owned())],
            "the later word stands"
        );
        assert_eq!(
            unfixed_lines("FIXED|9|nothing of the kind\nFIXED|1|ok", 1),
            []
        );
    }

    #[test]
    fn a_reply_with_no_line_about_a_finding_checked_nothing() {
        let err = read_unfixed("Looks good to me.", &held(1)).unwrap_err();
        assert!(err.contains("no line about a finding"), "{err}");
    }

    #[test]
    fn the_worker_is_sent_each_finding_as_a_line_it_can_copy_with_what_backs_it() {
        let mut high = Held::new(finding(Severity::High, 1));
        high.backing = Backing::Test {
            file: "tests/a.rs".into(),
            command: "cargo test --test a".into(),
            written: Vec::new(),
        };
        let mut unconfirmed = Held::new(finding(Severity::High, 2));
        unconfirmed.backing = Backing::Unconfirmed {
            why: "guarded".into(),
        };
        let mut again = Held::new(finding(Severity::Low, 3));
        again.still = Some("it panics".into());
        let text = fix_file(
            &[
                high,
                unconfirmed,
                Held::new(finding(Severity::Medium, 4)),
                again,
            ],
            Path::new("/b/deferred-findings.md"),
            false,
        );
        let line = |n: u32, sev: &str| {
            format!("{sev}|src/lib.rs:{n}|the flag is read first|a caller sees nothing\n")
        };
        assert!(text.starts_with("The deep review of your pull request held these findings"));
        assert!(text.contains("onto a line of its own in /b/deferred-findings.md"));
        let body = text.split_once("\n\n").unwrap().1;
        let body = body.split_once("\n\n").unwrap().1;
        assert_eq!(
            body,
            format!(
                "{}  failing test, in tests/a.rs: `cargo test --test a`\n\
                 {}  unconfirmed, no failing test could be made: guarded\n\
                 {}{}  still unfixed after your fix: it panics\n",
                line(1, "HIGH"),
                line(2, "HIGH"),
                line(4, "MEDIUM"),
                line(3, "LOW")
            )
        );
        assert!(
            !text.contains("MEDIUM|src/lib.rs:4|the flag is read first|a caller sees nothing\n  ")
        );
    }

    #[test]
    fn the_second_trip_says_the_recheck_found_them_unfixed() {
        let text = fix_file(&held(1), Path::new("/b/d.md"), true);
        assert!(
            text.starts_with("Kelpie's re-check of your fix found these findings still unfixed")
        );
        let prompt = fix_prompt(71, 2, Path::new("/b/f.md"), true);
        assert_eq!(
            prompt,
            "Kelpie's re-check of your fix on pull request #71 found 2 finding(s) still \
             unfixed, in /b/f.md. Fix each one, then commit and push with `git push origin HEAD`."
        );
    }

    #[test]
    fn the_recheck_is_told_to_run_each_failing_test_and_shown_only_the_fix() {
        let mut backed = Held::new(finding(Severity::High, 1));
        backed.backing = Backing::Test {
            file: "tests/a.rs".into(),
            command: "cargo test --test a".into(),
            written: Vec::new(),
        };
        let prompt = recheck_prompt(
            71,
            &[backed, Held::new(finding(Severity::Low, 2))],
            "abc123",
            "+fix",
        );
        assert!(prompt.contains("1. HIGH|src/lib.rs:1|"), "{prompt}");
        assert!(prompt.contains("   failing test, in tests/a.rs: `cargo test --test a`\n2. LOW|"));
        assert!(prompt.contains("run that test now"));
        assert!(prompt.contains("What the worker said it did is not evidence."));
        assert!(prompt.ends_with("--- the fix, diff since abc123 ---\n+fix\n--- end ---"));
    }
}
