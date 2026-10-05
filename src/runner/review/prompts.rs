//! What a reviewer's session is asked
//!
//! Its agent file's body, with the commit the change is against at
//! `{{BASE}}` and the diff at `{{DIFF}}`, then what the issue asks for, and on
//! a second look what the first found. A body with no `{{DIFF}}` gets the
//! diff after it.

use super::calls::severity_tag;
use crate::ports::Finding;

/// `body` with `base` and `diff` in their places
fn filled(body: &str, base: &str, diff: &str) -> String {
    let body = body.replace("{{BASE}}", base);
    // The diff goes in last, so a placeholder inside it is left as it is.
    match body.contains("{{DIFF}}") {
        true => body.replace("{{DIFF}}", diff),
        false => format!("{body}\n\n--- diff against {base} ---\n{diff}\n--- end ---"),
    }
}

/// What a reviewer's session is asked: its `body` over `diff`, which is
/// against `base`, the issue's `criteria`, and on a second look what the
/// first found
pub(super) fn reviewer_prompt(
    body: &str,
    base: &str,
    diff: &str,
    criteria: &str,
    first: Option<&[Finding]>,
) -> String {
    let mut prompt = filled(body, base, diff);
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
    use crate::agents::{Agents, DEFECT_HUNTER};
    use crate::ports::Severity;
    use crate::settings::AgentName;

    fn finding(severity: Severity, line: u32) -> Finding {
        Finding {
            severity,
            file: "src/lib.rs".into(),
            line,
            what: "the flag is read first".into(),
            why: "a caller sees nothing".into(),
        }
    }

    fn defect_hunter() -> String {
        let agents = Agents::embedded();
        let name = AgentName::kelpies(DEFECT_HUNTER);
        let agent = agents.get(&name).expect("kelpie ships defect-hunter");
        agent.prompt.clone().expect("its body is its prompt")
    }

    // The maintainer's defect prompt, typed out here with its placeholders
    // filled, so a change to `agents/defect-hunter.md` shows as a change here.
    #[test]
    fn defect_hunter_is_asked_the_maintainers_defect_prompt_word_for_word() {
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
        assert_eq!(
            filled(&defect_hunter(), "origin/main", "+let x = 1;"),
            expected
        );
    }

    #[test]
    fn a_placeholder_inside_the_diff_is_left_as_it_is() {
        let prompt = filled(&defect_hunter(), "origin/main", "+\"{{BASE}}\"");
        assert!(prompt.contains("\n+\"{{BASE}}\"\n--- end ---"), "{prompt}");
    }

    #[test]
    fn a_body_with_no_diff_placeholder_gets_the_diff_after_it() {
        let prompt = filled("Read it against {{BASE}}.", "origin/main", "+x");
        assert_eq!(
            prompt,
            "Read it against origin/main.\n\n--- diff against origin/main ---\n+x\n--- end ---"
        );
    }

    #[test]
    fn the_prompt_adds_the_issue_and_on_a_second_look_the_first_s_findings() {
        let body = defect_hunter();
        let first = reviewer_prompt(&body, "origin/main", "D", "#7 the issue", None);
        assert!(first.starts_with(&filled(&body, "origin/main", "D")));
        assert!(first.contains("asks for the following"));
        assert!(first.ends_with("#7 the issue"));
        assert!(!first.contains("the first reader's findings"));

        let found = [finding(Severity::High, 3)];
        let second = reviewer_prompt(&body, "origin/main", "D", "#7 the issue", Some(&found));
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
