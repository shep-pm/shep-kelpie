//! The issue check, on real issues of this repo and on one part at a time
use super::check::*;

// An issue as `gh issue view <n> --json title,body,labels` printed it.
fn recorded(json: &str) -> Vec<Failure> {
    let issue: serde_json::Value = serde_json::from_str(json).unwrap();
    let labels: Vec<String> = (issue["labels"].as_array().unwrap().iter())
        .map(|l| l["name"].as_str().unwrap().to_owned())
        .collect();
    check(
        issue["title"].as_str().unwrap(),
        issue["body"].as_str().unwrap(),
        &labels,
    )
}

const BODY: &str = "Touches: `src/board.rs`\n\n## What to build\n\n- A thing\n\n\
    ## Acceptance criteria\n\n- [ ] `board::tests::a_thing` passes\n";

fn labels(names: &[&str]) -> Vec<String> {
    names.iter().map(|&n| n.to_owned()).collect()
}

fn of(body: &str) -> Vec<Failure> {
    check(
        "A thing is true",
        body,
        &labels(&["kind:feat", "priority: P4"]),
    )
}

#[test]
fn an_issue_in_the_issue_skills_shape_passes() {
    let json = include_str!("../../../../fixtures/gh-issue-view-check-1377.json");
    assert_eq!(recorded(json), []);
}

#[test]
fn an_issue_written_as_prose_fails_on_every_part_it_lacks() {
    let json = include_str!("../../../../fixtures/gh-issue-view-check-1393.json");
    assert_eq!(
        recorded(json),
        [
            Failure::NoTouches,
            Failure::NoWhatToBuild,
            Failure::NoCriteria,
            Failure::NoPriority,
        ]
    );
}

#[test]
fn a_sliced_issue_with_no_kind_fails_on_that_alone() {
    let json = include_str!("../../../../fixtures/gh-issue-view-check-908.json");
    assert_eq!(recorded(json), [Failure::NoKind]);
}

#[test]
fn a_whole_issue_passes() {
    assert_eq!(of(BODY), []);
}

#[test]
fn an_empty_title_fails() {
    let failures = check(" ", BODY, &labels(&["kind:fix", "priority: P3"]));
    assert_eq!(failures, [Failure::NoTitle]);
}

#[test]
fn a_touches_line_needs_a_path_in_backticks_and_may_sit_in_a_list_or_bold() {
    let bare = BODY.replace("`src/board.rs`", "the board");
    assert_eq!(of(&bare), [Failure::TouchesNoPath]);
    for form in ["- Touches: `src/board.rs`", "**Touches:** `src/board.rs`"] {
        let body = BODY.replace("Touches: `src/board.rs`", form);
        assert_eq!(of(&body), [], "{form}");
    }
    assert_eq!(
        of(&BODY.replace("Touches:", "Files:")),
        [Failure::NoTouches]
    );
}

#[test]
fn an_issue_with_a_touches_heading_passes() {
    let json = include_str!("../../../../fixtures/gh-issue-view-check-1302.json");
    assert_eq!(recorded(json), []);
}

#[test]
fn touches_may_be_a_heading_or_a_line_with_a_list_under_it() {
    let rest = BODY.replace("Touches: `src/board.rs`\n", "");
    for touches in [
        "## Touches\n\n- `src/board.rs`\n- `src/lib.rs` (new)\n",
        "### touches\n`src/board.rs`\n",
        "Touches:\n\n- `src/board.rs`\n- `src/lib.rs`\n",
        "**Touches:**\n* `src/board.rs`\n",
    ] {
        assert_eq!(of(&format!("{touches}{rest}")), [], "{touches}");
        assert_eq!(of(&format!("{rest}\n{touches}")), [], "{touches}");
    }
    for empty in [
        "## Touches\n\n- the board\n",
        "Touches:\n\n- the board\n",
        "Touches:\n",
    ] {
        assert_eq!(
            of(&format!("{empty}{rest}")),
            [Failure::TouchesNoPath],
            "{empty}"
        );
    }
    let adapter = "## Touches\n\n- `crates/adapters/src/adapters/pi/skills.rs`\n";
    assert_eq!(
        of(&format!("{rest}\n{adapter}")),
        [Failure::NoHarnessReach(
            "crates/adapters/src/adapters/pi/skills.rs".to_owned()
        )]
    );
}

#[test]
fn a_labelled_line_is_read_in_any_case_and_with_its_label_in_bold() {
    for form in [
        "touches: `src/board.rs`",
        "TOUCHES: `src/board.rs`",
        "**Touches**: `src/board.rs`",
        "- **touches**: `src/board.rs`",
    ] {
        assert_eq!(
            of(&BODY.replace("Touches: `src/board.rs`", form)),
            [],
            "{form}"
        );
    }
    let adapter = BODY.replace("src/board.rs", "crates/adapters/src/adapters/pi/x.rs");
    assert_eq!(of(&format!("{adapter}\nharness reach: pi\n")), []);
    let blocked = format!("BLOCKED BY: the workspace\n\n{BODY}");
    assert_eq!(of(&blocked), [Failure::BlockedByNoIssue]);
}

#[test]
fn an_issue_written_with_crlf_line_ends_reads_as_with_lf() {
    assert_eq!(of(&BODY.replace('\n', "\r\n")), []);
    let heading =
        BODY.replace("Touches: `src/board.rs`\n", "") + "\n## Touches\n\n- `src/board.rs`\n";
    assert_eq!(of(&heading.replace('\n', "\r\n")), []);
    let fenced = BODY.replace("- [ ] `board", "~~~\n- [ ] x\n~~~\n- `board");
    assert_eq!(
        of(&fenced.replace('\n', "\r\n")),
        [Failure::NoCriteriaItems]
    );
}

#[test]
fn a_heading_may_close_with_hashes_or_sit_in_bold() {
    for (from, to) in [
        ("## What to build", "## What to build ##"),
        ("## What to build", "## **What to build**"),
        ("## Acceptance criteria", "### Acceptance criteria ###"),
        ("## Acceptance criteria", "## __Acceptance criteria__"),
    ] {
        assert_eq!(of(&BODY.replace(from, to)), [], "{to}");
    }
}

#[test]
fn a_harness_adapter_on_the_touches_line_needs_a_harness_reach_line() {
    for path in [
        "crates/adapters/src/adapters/claude/tests.rs",
        "crates/adapters/src/adapters/pi/skills.rs",
        "crates/adapters/src/adapters/codex_usage.rs",
        "crates/adapters/src/adapters/goose/compact.rs",
    ] {
        let body = BODY.replace("src/board.rs", path);
        assert_eq!(
            of(&body),
            [Failure::NoHarnessReach(path.to_owned())],
            "{path}"
        );
        let reached = format!("{body}\nHarness reach: pi in this issue, Codex in #12\n");
        assert_eq!(of(&reached), [], "{path}");
        let empty = format!("{body}\nHarness reach:\n");
        assert_eq!(
            of(&empty),
            [Failure::NoHarnessReach(path.to_owned())],
            "{path}"
        );
    }
    for path in [
        "crates/adapters/src/adapters/harness/policy.rs",
        "crates/adapters/src/adapters/pages.rs",
        "crates/core/src/pi.rs",
    ] {
        assert_eq!(of(&BODY.replace("src/board.rs", path)), [], "{path}");
    }
}

#[test]
fn what_to_build_needs_text_under_its_heading_at_any_level_and_case() {
    let empty = BODY.replace("- A thing\n", "");
    assert_eq!(of(&empty), [Failure::NoWhatToBuild]);
    let gone = BODY.replace("## What to build\n\n- A thing\n\n", "");
    assert_eq!(of(&gone), [Failure::NoWhatToBuild]);
    let other = BODY.replace("## What to build", "### what to build");
    assert_eq!(of(&other), []);
}

#[test]
fn acceptance_criteria_need_a_checklist_item() {
    let plain = BODY.replace("- [ ] ", "- ");
    assert_eq!(of(&plain), [Failure::NoCriteriaItems]);
    let ticked = BODY.replace("- [ ] ", "- [x] ");
    assert_eq!(of(&ticked), []);
    let gone = BODY.replace("## Acceptance criteria", "## Tests");
    assert_eq!(of(&gone), [Failure::NoCriteria]);
}

#[test]
fn a_section_ends_at_the_next_heading_of_its_level() {
    let later = BODY.replace(
        "\n- [ ] `board::tests::a_thing` passes\n",
        "\n## Notes\n\n- [ ] not a criterion\n",
    );
    assert_eq!(of(&later), [Failure::NoCriteriaItems]);
    let deeper = BODY.replace("\n- [ ] `board", "\n### Tests\n\n- [ ] `board");
    assert_eq!(of(&deeper), []);
}

#[test]
fn a_tilde_fence_and_a_longer_backtick_fence_hide_what_they_hold() {
    let hidden = "## Acceptance criteria\n\n- [ ] in a fence\n";
    let head = "Touches: `src/board.rs`\n\n## What to build\n\n- A thing\n\n";
    for (open, close) in [
        ("~~~", "~~~"),
        ("````", "````"),
        ("~~~~ md", "~~~~~"),
        ("```", "```"),
    ] {
        let body = format!("{head}{open}\n{hidden}{close}\n");
        assert_eq!(of(&body), [Failure::NoCriteria], "{open}");
    }
    // A shorter mark, or the other one, closes nothing, so what follows stays in.
    for (open, inner) in [("````", "```"), ("~~~", "```"), ("```", "~~~")] {
        let body = format!("{head}{open}\n{inner}\n{hidden}");
        assert_eq!(of(&body), [Failure::NoCriteria], "{open} {inner}");
    }
    let closed = format!("{head}~~~\nx\n~~~\n\n{hidden}");
    assert_eq!(of(&closed), []);
}

#[test]
fn a_fenced_line_is_no_part_of_a_section_and_no_labelled_line() {
    let fenced = BODY.replace("- [ ] `board", "```\n- [ ] x\n```\n- `board");
    assert_eq!(of(&fenced), [Failure::NoCriteriaItems]);
    let only_code = BODY.replace("- A thing\n", "~~~\nfn a() {}\n~~~\n");
    assert_eq!(of(&only_code), [Failure::NoWhatToBuild]);
    let touches = BODY.replace(
        "Touches: `src/board.rs`",
        "```\nTouches: `src/board.rs`\n```",
    );
    assert_eq!(of(&touches), [Failure::NoTouches]);
    let blocked = format!("~~~\nBlocked by: the workspace\n~~~\n\n{BODY}");
    assert_eq!(of(&blocked), []);
    let reach = BODY.replace("src/board.rs", "crates/adapters/src/adapters/pi/x.rs")
        + "\n```\nHarness reach: pi\n```\n";
    assert_eq!(
        of(&reach),
        [Failure::NoHarnessReach(
            "crates/adapters/src/adapters/pi/x.rs".to_owned()
        )]
    );
}

#[test]
fn a_blocked_by_line_names_an_issue_or_a_local_ticket() {
    for line in [
        "Blocked by: #907, #901",
        "Blocked by: k12",
        "Blocked by: shep-pm/shep#623",
    ] {
        assert_eq!(of(&format!("{line}\n\n{BODY}")), [], "{line}");
    }
    for line in [
        "Blocked by: the workspace",
        "Blocked by: #",
        "Blocked by: k8s",
    ] {
        assert_eq!(
            of(&format!("{line}\n\n{BODY}")),
            [Failure::BlockedByNoIssue],
            "{line}"
        );
    }
}

// BODY with `criterion` as its one acceptance criterion.
fn criterion(criterion: &str) -> Vec<Failure> {
    let item = format!("- [ ] {criterion}\n");
    of(&BODY.replace("- [ ] `board::tests::a_thing` passes\n", &item))
}

#[test]
fn a_criterion_that_edits_another_issue_fails_with_why() {
    let text = "Edit #1075's step 7 row";
    let failures = criterion(text);
    assert_eq!(failures, [Failure::ChangesIssue(text.to_owned())]);
    let said = failures[0].to_string();
    assert!(said.contains("credential cannot change issues"), "{said}");
    assert!(said.contains("to the maintainer as a note"), "{said}");
    assert!(said.contains(text), "{said}");
}

#[test]
fn every_way_to_change_an_issue_is_caught() {
    for verb in [
        "Update",
        "Comment on",
        "Label",
        "Close",
        "Reopen",
        "Closes",
        "Edited",
    ] {
        let text = format!("{verb} #12 when it is done");
        assert_eq!(criterion(&text), [Failure::ChangesIssue(text)]);
    }
}

#[test]
fn a_criterion_that_adds_to_or_ticks_another_issue_is_caught() {
    for text in [
        "Add a row to shep-pm/shep#1075",
        "Tick the step 7 box on #1075",
        "Check off step 7 on the tracker #1075",
        "Append a row to issue #1075",
        "Retitle #12",
        "The worker should assign #12 to the maintainer",
        "Update the pull request body to say Resolves #12, and close #13",
        "Update the pull request body and close #13",
    ] {
        assert_eq!(
            criterion(text),
            [Failure::ChangesIssue(text.to_owned())],
            "{text}"
        );
    }
}

#[test]
fn a_change_word_that_is_not_about_the_issue_passes() {
    for text in [
        "Closing the window no longer loses state, as reported in #12",
        "The status label updates as #12 describes",
        "A code comment cites #12",
        "Add a regression test for #12",
        "Check #12 still passes",
        "Edit the check, which #12 asks for",
    ] {
        assert_eq!(criterion(text), [], "{text}");
    }
}

#[test]
fn a_criterion_that_only_names_an_issue_passes() {
    for text in [
        "Resolves #12",
        "The result matches #12's shape",
        "The same shape as shep-pm/shep#12",
        "Edit the check in `crates/core/src/board/check.rs`",
        "`board::update_label` fixes the bug in #12",
    ] {
        assert_eq!(criterion(text), [], "{text}");
    }
}

#[test]
fn a_criterion_naming_an_issue_in_another_repo_is_caught() {
    let text = "Add a row to `shep-pm/shep#1075`, then update it";
    assert_eq!(criterion(text), [Failure::ChangesIssue(text.to_owned())]);
    let text = "Comment on shep-pm/shep#1075";
    assert_eq!(criterion(text), [Failure::ChangesIssue(text.to_owned())]);
}

#[test]
fn a_criterion_about_the_pull_requests_own_body_passes() {
    for text in [
        "Update the pull request body to say `Resolves #12`",
        "The pull request's description is edited to name #12",
        "Edit the PR body: it ends `Resolves #12`",
    ] {
        assert_eq!(criterion(text), [], "{text}");
    }
}

#[test]
fn a_criterions_wrapped_line_counts_with_it_and_only_its_first_line_is_named() {
    let body = BODY.replace(
        "- [ ] `board::tests::a_thing` passes\n",
        "- [ ] Tracker row added:\n  update #1075 with it\n- [ ] It works\n",
    );
    assert_eq!(
        of(&body),
        [Failure::ChangesIssue("Tracker row added:".to_owned())]
    );
}

#[test]
fn an_issue_carries_one_priority_label() {
    let check = |names: &[&str]| check("fix: a thing", BODY, &labels(names));
    assert_eq!(check(&["P2"]), []);
    assert_eq!(check(&["priority: P0"]), []);
    assert_eq!(check(&["bug"]), [Failure::NoPriority]);
    assert_eq!(
        check(&["priority: P1", "priority: P3"]),
        [Failure::Priorities(labels(&[
            "priority: P1",
            "priority: P3"
        ]))]
    );
}

#[test]
fn an_issues_kind_is_its_kind_label_else_its_titles_conventional_prefix() {
    let check = |title: &str, names: &[&str]| check(title, BODY, &labels(names));
    let p4 = "priority: P4";
    assert_eq!(check("A thing is true", &["kind:feat", p4]), []);
    assert_eq!(check("A thing is true", &["kind: chore", p4]), []);
    for title in ["feat: a thing", "fix(board): a thing", "refactor!: a thing"] {
        assert_eq!(check(title, &[p4]), [], "{title}");
    }
    for title in ["A thing is true", "feature: a thing", "fix(): a thing"] {
        assert_eq!(check(title, &[p4]), [Failure::NoKind], "{title}");
    }
    assert_eq!(
        check("A thing", &["bug", "enhancement", p4]),
        [Failure::NoKind]
    );
    for label in ["kind:bug", "kind:build", "kind:revert"] {
        assert_eq!(
            check("fix: a thing", &[label, p4]),
            [Failure::UnknownKind(label.to_owned())]
        );
    }
    for title in ["build: a thing", "revert: a thing"] {
        assert_eq!(check(title, &[p4]), [Failure::NoKind], "{title}");
    }
    for kind in kinds() {
        assert_eq!(
            check("A thing", &[&format!("kind:{kind}"), p4]),
            [],
            "{kind}"
        );
    }
    assert_eq!(
        check("fix: a thing", &["kind:fix", "kind:docs", p4]),
        [Failure::Kinds(labels(&["kind:fix", "kind:docs"]))]
    );
}

#[test]
fn a_work_items_kind_is_the_one_kind_its_label_names() {
    let kind = |names: &[&str]| label_kind(&labels(names));
    assert_eq!(kind(&["priority: P4", "Kind: Fix "]), Some("fix"));
    assert_eq!(kind(&["kind:docs"]), Some("docs"));
    assert_eq!(kind(&["priority: P4"]), None);
    assert_eq!(kind(&["kind:bug"]), None);
    assert_eq!(kind(&["kind:fix", "kind:docs"]), None);
}

#[test]
fn each_failure_names_the_part_it_is_about() {
    let said = [
        (Failure::NoTouches, "`Touches:`"),
        (Failure::NoWhatToBuild, "`## What to build`"),
        (Failure::NoCriteriaItems, "`- [ ]`"),
        (
            Failure::NoHarnessReach("a/adapters/pi/x.rs".into()),
            "`Harness reach:`",
        ),
        (Failure::BlockedByNoIssue, "`Blocked by:`"),
        (
            Failure::ChangesIssue("Edit #1075's step 7 row".into()),
            "credential cannot change issues",
        ),
        (Failure::NoKind, "`kind:<type>`"),
        (Failure::UnknownKind("kind:bug".into()), "`kind:bug`"),
        (Failure::Priorities(labels(&["P1", "P2"])), "`P1` and `P2`"),
    ];
    for (failure, part) in said {
        assert!(failure.to_string().contains(part), "{failure}");
    }
}

#[test]
fn the_key_changes_with_what_the_check_reads_and_nothing_else() {
    let read = labels(&["kind:feat", "priority: P4"]);
    let base = key("A thing is true", BODY, &read);
    assert_eq!(
        key(
            "A thing is true",
            BODY,
            &labels(&["Priority: P4", "kind:feat"])
        ),
        base,
        "label order and case are not an edit"
    );
    let unread = labels(&[
        "kind:feat",
        "priority: P4",
        "needs-triage",
        "in-progress",
        "bug",
    ]);
    assert_eq!(key("A thing is true", BODY, &unread), base);
    assert_ne!(key("A thing is false", BODY, &read), base);
    assert_ne!(key("A thing is true", "Touches:", &read), base);
    assert_ne!(
        key(
            "A thing is true",
            BODY,
            &labels(&["kind:feat", "priority: P2"])
        ),
        base
    );
    assert_ne!(
        key("A thing is true", BODY, &labels(&["priority: P4"])),
        base
    );
}

#[test]
fn fnv1a_is_the_published_function() {
    assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
}
