//! What a reviewer's session is asked
//!
//! Its agent file's body, with the commit the change is against at
//! `{{BASE}}` and the diff at `{{DIFF}}`, then what the issue asks for, and on
//! a second look what the first found. A body with no `{{DIFF}}` gets the
//! diff after it.

use std::path::Path;

use super::calls::severity_tag;
use crate::ports::{CLEAN, Finding};
use crate::work_item::{Dispute, ReviewPlan};

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
/// against `base`, the issue's `criteria`, on a project with a test command
/// the `tests` it may write, as that command, the folder and the name of the
/// tool it writes with, and on a second look what the first found
pub(in crate::runner) fn reviewer_prompt(
    body: &str,
    base: &str,
    diff: &str,
    criteria: &str,
    first: Option<&[Finding]>,
    tests: Option<(&str, &Path, &str)>,
) -> String {
    let mut prompt = filled(body, base, diff);
    if !criteria.trim().is_empty() {
        prompt.push_str(&format!(
            "\n\nThe issue this pull request resolves asks for the following. \
             Report anything it asks for that the diff leaves undone or gets wrong, \
             in the same format.\n\n{criteria}"
        ));
    }
    if let Some((command, folder, tool)) = tests {
        let folder = folder.display();
        prompt.push_str(&format!(
            "\n\nWhere a finding's trigger can be written as a test, also write one failing \
             test per finding with the {tool} tool. Write it into {folder}, which is the only \
             place you may write, at the path the test should have in the repo: a test for \
             tests/flag.rs is written to {folder}/tests/flag.rs. Name that path as a fifth \
             field on the finding's line, SEVERITY|file:line|what|why|tests/flag.rs, and leave \
             the field off a finding with no test. The test must fail on the code as it stands, \
             for the reason the finding gives, and pass once the finding is fixed. Name no path \
             the repo already holds. The project runs a test with `{command}`, where {{test}} \
             is the file's path in the repo and {{name}} its name without the extension."
        ));
    }
    if let Some(first) = first {
        let lines: String = match first.is_empty() {
            true => format!("{CLEAN}\n"),
            false => first.iter().map(line).collect(),
        };
        prompt.push_str(&format!(
            "\n\nAnother reader has already read this change and found what follows. \
             Report only what it missed: a defect it did not list. Do not repeat or \
             reword anything it found. If it missed nothing, output exactly {CLEAN} and \
             nothing else.\n\n--- the first reader's findings ---\n{lines}--- end ---"
        ));
    }
    prompt
}

/// What a reader is told of the findings the worker disputed, to add after
/// its prompt: each finding with the worker's reason, indented under it, and
/// nothing when there are none
pub(in crate::runner) fn disputes_note(disputes: &[Dispute]) -> String {
    if disputes.is_empty() {
        return String::new();
    }
    let lines: String = (disputes.iter())
        .map(|d| format!("{}  {}\n", line(&d.finding), d.reason))
        .collect();
    format!(
        "\n\nThe worker disputed these findings of an earlier reader on this pull \
         request, saying each is wrong and why: each finding is followed by the \
         worker's reason, indented. Check the reason against the code. Raise a finding \
         again, in the same format, only if the reason is wrong; leave out one whose \
         reason holds.\n\n--- the disputed findings ---\n{lines}--- end ---"
    )
}

/// What a reader is told of the findings earlier rounds sent the worker, to
/// add after its prompt: each finding, and that a change answering one is
/// asked for, and nothing when there are none
pub(in crate::runner) fn asked_note(held: &[Finding]) -> String {
    if held.is_empty() {
        return String::new();
    }
    let lines: String = held.iter().map(line).collect();
    format!(
        "\n\nEarlier reviewers on this pull request asked the worker to make these \
         changes. A change in the diff that answers one of them counts as asked for: \
         do not report it as behaviour nobody asked for.\n\n\
         --- the findings earlier rounds sent ---\n{lines}--- end ---"
    )
}

/// What a reader is told of why the project manager wants these reads, to
/// add after its prompt, and nothing when there is no plan or it says nothing
/// of what the reads are for
pub(in crate::runner) fn plan_note(plan: Option<&ReviewPlan>) -> String {
    let Some(about) = plan.and_then(|plan| plan.about.as_deref()) else {
        return String::new();
    };
    format!(
        "\n\nThe project manager planned the reads of this pull request, yours among \
         them, for this: {about}\nGive it your attention beside the rest of the change."
    )
}

/// The most of a pull request's body a reader is given, in bytes, the end of it
const MOST_BODY: usize = 4000;

/// The pull request's `body` for a reader to check the change against, to add
/// after its prompt, and nothing for a blank body
///
/// A body over 4000 bytes keeps its end, where a project's CI appends.
pub(in crate::runner) fn body_note(body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        return String::new();
    }
    let (body, left_out) = match body.len().checked_sub(MOST_BODY) {
        Some(over) if over > 0 => {
            let start = (over..)
                .find(|i| body.is_char_boundary(*i))
                .unwrap_or(body.len());
            (&body[start..], "The start of the body is left out.\n")
        }
        _ => (body, ""),
    };
    format!(
        "\n\nThe author and the project's CI wrote this pull request's body. It \
         describes the change and proves nothing about it: check the diff for what it \
         says.\n{left_out}\n--- the pull request's body ---\n{body}\n--- end ---"
    )
}

// One finding as the readers write it, its test as a fifth field.
fn line(f: &Finding) -> String {
    let test = (f.test.as_ref()).map_or_else(String::new, |t| format!("|{t}"));
    format!(
        "{}|{}:{}|{}|{}{test}\n",
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
        Finding::new(
            severity,
            "src/lib.rs",
            line,
            "the flag is read first",
            "a caller sees nothing",
        )
    }

    fn defect_hunter() -> String {
        let agents = Agents::embedded().bind(
            DEFECT_HUNTER,
            "harness: claude-code\nmodel: claude-opus-5-5\neffort: high\n",
        );
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
    fn the_prompt_asks_for_a_test_only_when_the_project_has_a_test_command() {
        let body = defect_hunter();
        let plain = reviewer_prompt(&body, "origin/main", "D", "#7 the issue", None, None);
        let folder = Path::new("/k/targets/shep/7/review-tests");
        let asked = reviewer_prompt(
            &body,
            "origin/main",
            "D",
            "#7 the issue",
            None,
            Some(("cargo test --test {name}", folder, "Write")),
        );

        assert!(!plain.contains("review-tests"), "{plain}");
        assert!(
            asked.starts_with(&plain),
            "the paragraph is appended: {asked}"
        );
        let paragraph = &asked[plain.len()..];
        assert!(
            paragraph.contains("/k/targets/shep/7/review-tests"),
            "{paragraph}"
        );
        assert!(
            paragraph.contains("one failing test per finding"),
            "{paragraph}"
        );
        assert!(paragraph.contains("with the Write tool"), "{paragraph}");
        assert!(paragraph.contains("fifth field"), "{paragraph}");
        assert!(
            paragraph.contains("`cargo test --test {name}`"),
            "{paragraph}"
        );

        // A goose reviewer writes with `write`, the tool of its `developer` extension.
        let tool = super::super::attached::writing_tool(crate::settings::Harness::Goose);
        assert_eq!(tool, Some("write"));
        let goose = reviewer_prompt(
            &body,
            "origin/main",
            "D",
            "#7 the issue",
            None,
            Some(("cargo test --test {name}", folder, tool.unwrap())),
        );
        assert!(
            goose.contains("per finding with the write tool."),
            "{goose}"
        );
        assert!(goose.contains("/k/targets/shep/7/review-tests"), "{goose}");
    }

    #[test]
    fn the_prompt_names_the_tool_the_reviewers_harness_writes_with() {
        let folder = Path::new("/k/targets/shep/7/review-tests");
        let ask = |tool| {
            let tests = Some(("cargo test --test {name}", folder, tool));
            reviewer_prompt(&defect_hunter(), "origin/main", "D", "", None, tests)
        };

        let pi = ask("write");
        assert!(pi.contains("per finding with the write tool."), "{pi}");
        assert!(!pi.contains("Write tool"), "{pi}");
        assert!(ask("Write").contains("per finding with the Write tool."));
    }

    #[test]
    fn the_prompt_names_apply_patch_for_codex() {
        let folder = Path::new("/k/targets/shep/7/review-tests");
        let tests = Some(("cargo test --test {name}", folder, "apply_patch"));

        let prompt = reviewer_prompt(&defect_hunter(), "origin/main", "D", "", None, tests);

        assert!(
            prompt.contains("per finding with the apply_patch tool."),
            "{prompt}"
        );
        assert!(
            prompt.contains("/k/targets/shep/7/review-tests"),
            "{prompt}"
        );
    }

    #[test]
    fn the_asked_note_is_empty_with_no_earlier_finding() {
        assert_eq!(asked_note(&[]), "");
    }

    #[test]
    fn the_asked_note_lists_each_earlier_finding_and_counts_a_change_answering_one_as_asked_for() {
        let held = [finding(Severity::High, 3), finding(Severity::Medium, 9)];

        let note = asked_note(&held);

        assert!(
            note.contains("asked the worker to make these changes"),
            "{note}"
        );
        assert!(note.contains("counts as asked for"), "{note}");
        assert!(note.contains("behaviour nobody asked for"), "{note}");
        assert!(
            note.ends_with(
                "--- the findings earlier rounds sent ---\n\
                 HIGH|src/lib.rs:3|the flag is read first|a caller sees nothing\n\
                 MEDIUM|src/lib.rs:9|the flag is read first|a caller sees nothing\n\
                 --- end ---"
            ),
            "{note}"
        );
    }

    fn planned(about: Option<&str>) -> ReviewPlan {
        ReviewPlan {
            reads: 2.try_into().unwrap(),
            about: about.map(str::to_owned),
        }
    }

    #[test]
    fn the_plan_note_says_what_the_reads_are_for() {
        let note = plan_note(Some(&planned(Some("the migration's rollback path"))));

        assert!(
            note.starts_with("\n\nThe project manager planned"),
            "{note}"
        );
        assert!(
            note.contains("for this: the migration's rollback path\n"),
            "{note}"
        );
    }

    #[test]
    fn the_plan_note_is_empty_with_no_about() {
        assert_eq!(plan_note(Some(&planned(None))), "");
        assert_eq!(plan_note(None), "");
    }

    #[test]
    fn a_pull_requests_body_is_given_between_its_markers() {
        let note = body_note("Fixes the flag.\n\n## Screenshots\n\n![a](a.png)");

        assert!(note.starts_with("\n\n"), "{note}");
        assert!(
            note.contains("The author and the project's CI wrote"),
            "{note}"
        );
        assert!(note.contains("proves nothing about it"), "{note}");
        assert!(
            note.ends_with(
                "--- the pull request's body ---\n\
                 Fixes the flag.\n\n## Screenshots\n\n![a](a.png)\n--- end ---"
            ),
            "{note}"
        );
    }

    #[test]
    fn a_blank_body_adds_nothing() {
        assert_eq!(body_note(""), "");
        assert_eq!(body_note(" \n\t\r\n "), "");
    }

    #[test]
    fn a_long_body_keeps_its_end_at_a_character_boundary() {
        // 6001 bytes: the last 4000 begin at byte 2001, inside the 1001st "é"
        let body = format!("{}z", "é".repeat(3000));

        let note = body_note(&body);

        let kept = note
            .split("--- the pull request's body ---\n")
            .nth(1)
            .and_then(|rest| rest.strip_suffix("\n--- end ---"))
            .expect("the body sits between its markers");
        assert_eq!(kept, format!("{}z", "é".repeat(1999)), "{note}");
        assert!(note.contains("start of the body is left out"), "{note}");
    }

    #[test]
    fn a_body_of_exactly_the_most_is_given_whole() {
        let body = "b".repeat(4000);

        let note = body_note(&body);

        assert!(note.contains(&format!("\n{body}\n--- end ---")), "{note}");
        assert!(!note.contains("left out"), "{note}");
    }

    #[test]
    fn a_finding_with_a_test_is_listed_with_its_fifth_field() {
        let mut found = finding(Severity::High, 3);
        found.test = Some("tests/flag.rs".into());
        let prompt = reviewer_prompt(&defect_hunter(), "b", "D", "", Some(&[found]), None);
        assert!(
            prompt.ends_with(
                "HIGH|src/lib.rs:3|the flag is read first|a caller sees nothing|tests/flag.rs\n--- end ---"
            ),
            "{prompt}"
        );
    }

    #[test]
    fn the_prompt_adds_the_issue_and_on_a_second_look_the_first_s_findings() {
        let body = defect_hunter();
        let first = reviewer_prompt(&body, "origin/main", "D", "#7 the issue", None, None);
        assert!(first.starts_with(&filled(&body, "origin/main", "D")));
        assert!(first.contains("asks for the following"));
        assert!(first.ends_with("#7 the issue"));
        assert!(!first.contains("the first reader's findings"));

        let found = [finding(Severity::High, 3)];
        let second = reviewer_prompt(
            &body,
            "origin/main",
            "D",
            "#7 the issue",
            Some(&found),
            None,
        );
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
