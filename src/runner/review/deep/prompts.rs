//! What the deep round's readers are asked
//!
//! The readers get the maintainer's defect prompt, word for word apart from
//! its two placeholders.

use crate::ports::Finding;
use crate::runner::review::calls::severity_tag;

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

// One finding as the readers write it.
fn line(f: &Finding) -> String {
    format!(
        "{}|{}:{}|{}|{}\n",
        severity_tag(f.severity),
        f.file,
        f.line,
        f.what,
        f.why
    )
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
}
