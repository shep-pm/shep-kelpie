//! The standing lines that name a work item, in its worker's first prompt and its reviewers' prompts

use crate::ports::CLEAN;
use crate::runner::step;
use crate::test::{FakeForge, Rig, Scripted};

/// An issue body that passes the issue check, and touches `src/issue_7.rs`
fn body() -> String {
    FakeForge::passing_body(7)
}

const STANDING: &str = "Never reorder `src/issue_7.rs` handlers (until #40)\n\
    ## 2026-09-20 09:00\n\
    Hold #7 for the release\n\
    Wait for #9 before any retry change\n\
    Review every diff before a merge\n\
    Leave #70 and `src/other.rs` alone\n";

/// The first prompt of issue 7, whose body is `body`, with `standing` in `pm-standing.md`
fn worker_prompt(body: &str, standing: Option<&str>) -> String {
    let rig = Rig::new("shep");
    rig.forge.open_issue(7, "Title of #7", body);
    if let Some(standing) = standing {
        std::fs::create_dir_all(rig.paths().pm).unwrap();
        std::fs::write(rig.paths().pm.join("pm-standing.md"), standing).unwrap();
    }
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Say("Done.")]);
    step(&runner).unwrap();
    let [seen] = rig.claude.seen().try_into().unwrap();
    seen.call.prompt
}

#[test]
fn a_workers_opening_prompt_carries_the_standing_lines_that_name_its_files() {
    let prompt = worker_prompt(&body(), Some(STANDING));

    assert!(
        prompt.contains("Never reorder `src/issue_7.rs` handlers (until #40)"),
        "{prompt}"
    );
    assert!(prompt.contains("Hold #7 for the release"), "{prompt}");
    assert!(prompt.contains("Follow each line"), "{prompt}");
    assert!(prompt.contains("say so in the pull request"), "{prompt}");
    assert!(!prompt.contains("Review every diff"), "{prompt}");
    assert!(!prompt.contains("Leave #70"), "{prompt}");
    assert!(!prompt.contains("## 2026-09-20"), "{prompt}");
}

#[test]
fn a_workers_opening_prompt_carries_the_standing_lines_that_name_its_blocker() {
    let prompt = worker_prompt("Change the retries.\n\nBlocked by: #9\n", Some(STANDING));

    assert!(
        prompt.contains("Wait for #9 before any retry change"),
        "{prompt}"
    );
    assert!(!prompt.contains("Never reorder"), "{prompt}");
}

#[test]
fn a_workers_opening_prompt_with_no_line_that_applies_is_unchanged() {
    let without = worker_prompt("Change the docs.\n", None);
    let with = worker_prompt(
        "Change the docs.\n",
        Some("Leave #70 and `src/other.rs` alone\n"),
    );

    assert_eq!(with, without);
}

#[test]
fn a_reviewers_prompt_carries_the_standing_lines_that_apply() {
    let rig = Rig::new("shep");
    rig.allow(&["sonnet-high", "qwen", "claude"]);
    // Dispatched from the ready queue
    std::fs::create_dir_all(rig.paths().pm).unwrap();
    std::fs::write(rig.paths().pm.join("pm-standing.md"), STANDING).unwrap();
    let runner = rig.open().unwrap();
    rig.forge.open_issue(7, "Title of #7", &body());
    rig.forge.list_ready(7, false);
    rig.claude
        .script([Scripted::Push("work.txt", "work\n"), Scripted::Text(CLEAN)]);
    step(&runner).unwrap(); // the board reads the queue
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    // The dispatch and the worker's turn, round 1 (qwen) and round 2 (claude)
    for _ in 0..3 {
        step(&runner).unwrap();
    }
    let prompt = rig.claude.all_calls().pop().unwrap().prompt;
    assert!(prompt.contains("Review the change"), "{prompt}");

    assert!(
        prompt.contains("Never reorder `src/issue_7.rs` handlers (until #40)"),
        "{prompt}"
    );
    assert!(prompt.contains("Hold #7 for the release"), "{prompt}");
    assert!(prompt.contains("breaks a line"), "{prompt}");
    assert!(!prompt.contains("Review every diff"), "{prompt}");
}

#[test]
fn a_project_with_no_standing_file_prompts_as_before() {
    let prompt = worker_prompt(&body(), None);
    assert!(!prompt.contains("standing rulings"), "{prompt}");

    let rig = Rig::new("shep");
    rig.allow(&["sonnet-high", "qwen", "claude"]);
    rig.forge.open_issue(7, "Title of #7", &body());
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap();
    step(&runner).unwrap();
    rig.claude.script([Scripted::Text(CLEAN)]);
    step(&runner).unwrap();
    let prompt = rig.claude.all_calls().pop().unwrap().prompt;
    assert!(!prompt.contains("standing rulings"), "{prompt}");
}

#[test]
fn a_reviewers_prompt_carries_the_lines_naming_the_blocker_of_an_item_the_board_never_read() {
    let rig = Rig::new("shep");
    rig.allow(&["sonnet-high", "qwen", "claude"]);
    rig.forge
        .open_issue(7, "Title of #7", "Change the retries.\n\nBlocked by: #9\n");
    std::fs::create_dir_all(rig.paths().pm).unwrap();
    std::fs::write(rig.paths().pm.join("pm-standing.md"), STANDING).unwrap();
    let runner = rig.open().unwrap();
    // Added by hand, so the board holds no body for it
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude
        .script([Scripted::Push("work.txt", "work\n"), Scripted::Text(CLEAN)]);
    for _ in 0..3 {
        step(&runner).unwrap(); // the worker's turn, then rounds 1 and 2
    }
    let prompt = rig.claude.all_calls().pop().unwrap().prompt;

    assert!(prompt.contains("Review the change"), "{prompt}");
    assert!(
        prompt.contains("Wait for #9 before any retry change"),
        "{prompt}"
    );
}
