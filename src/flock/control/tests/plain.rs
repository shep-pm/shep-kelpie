//! What a runner's `status` answer prints, plain and as JSON, and what the
//! other verbs print for theirs

use super::*;

// What a runner finishing two work items, one parked on a merge ruling,
// answered `status` with
const RECORDED: &str = r#"{"project":"koji","merging":"ask","run":"finishing",
    "work_item":null,
    "work_items":[
        {"issue":"7","title":"Add a thing","phase":{"state":"ruling","id":"1"},
         "turn":{"state":"ended","at":100},"pull_request":71},
        {"issue":"9","title":"Fix the other","phase":{"state":"implement"},
         "turn":{"state":"running","since":200},"pull_request":null}],
    "rulings":[{"id":"1","issue":"7","pull_request":71,
        "question":"Merge pull request #71 at abc1234 into main?",
        "kind":{"kind":"merge","head":"abc1234def"},"alerted":false}],
    "pacer":{"enabled":true,"claude":{
        "reading":{"at":300,"session":{"used_pct":3,"resets_at":400},
            "week":{"used_pct":15,"resets_at":500},"allowance_pct":14.3,
            "spent_today_pct":15,"per_hour_pct":1.8},
        "holding":{"kind":"allowance","reason":"today's allowance is spent","until":600}}}}"#;

#[test]
fn a_plain_status_names_what_the_maintainer_looks_for() {
    assert_eq!(
        plain_status("koji", RECORDED, None),
        [
            "koji (finishing)",
            "  run: finishing",
            "  build: not recorded",
            "  work items:",
            "    #7 Add a thing: phase ruling, turn ended, PR #71",
            "    #9 Fix the other: phase implement, turn running, no PR",
            "  rulings waiting: 1",
            "    ruling 1: merge, #7, PR #71",
            "  pacer: on",
            "    claude: session 3%, week 15%, 15% of the week spent today of 14.3% allowed",
            "    claude holding (allowance): today's allowance is spent",
        ]
    );
}

#[test]
fn a_plain_status_names_the_red_workflow_how_long_it_has_been_red_and_its_run() {
    let body = r#"{"project":"koji","work_items":[],"red_workflows":[
        {"workflow":"test","first_run":91,"run":91,"head":"head91","since":1700000000,
         "red_for":11100,"url":"https://github.com/shep-pm/shep/actions/runs/91",
         "doing":"kelpie reran its failed jobs and waits for the result"}]}"#;
    let lines = plain_status("koji", body, None);

    assert!(lines.contains(&"  red on main: 1".to_owned()), "{lines:#?}");
    assert!(
        lines.contains(
            &"    test: red for 3h05m, https://github.com/shep-pm/shep/actions/runs/91; \
               kelpie reran its failed jobs and waits for the result"
                .to_owned()
        ),
        "{lines:#?}"
    );
}

#[test]
fn a_plain_status_says_so_of_a_ruling_with_no_issue() {
    let body = r#"{"project":"koji","work_items":[],"rulings":[
        {"id":"1","issue":null,"pull_request":null,"kind":{"kind":"question"}}]}"#;
    assert_eq!(
        plain_status("koji", body, None)[4..6],
        [
            "  rulings waiting: 1",
            "    ruling 1: question, no issue, no PR"
        ]
    );
}

#[test]
fn a_plain_status_lists_each_contribution_with_its_author_and_head() {
    let body = r#"{"project":"koji","work_items":[],"contributions":[
        {"pull_request":12,"author":"newcomer","head":"2a8f1e6d0b4c","seen":1790000000},
        {"pull_request":14,"author":"","head":"1f7e0d5c9a3b","seen":1790000100}],"rulings":[]}"#;
    assert_eq!(
        plain_status("koji", body, None)[4..7],
        [
            "  contributions: 2",
            "    PR #12 by newcomer, head 2a8f1e6",
            "    PR #14 by a deleted account, head 1f7e0d5",
        ]
    );
}

#[test]
fn a_plain_status_lists_each_bump_with_where_it_stands() {
    let body = r#"{"project":"koji","work_items":[],"bumps":[
        {"pull_request":21,"author":"dependabot","head":"9c0d1e2f3a4b",
         "standing":"a web bump, and the project has no e2e host, so it is not merged"}],
        "rulings":[]}"#;
    assert_eq!(
        plain_status("koji", body, None)[4..6],
        [
            "  bumps: 1",
            "    PR #21 by dependabot, head 9c0d1e2: a web bump, and the project has no e2e host, so it is not merged",
        ]
    );
}

#[test]
fn a_plain_status_names_what_a_closed_duplicate_was_closed_as_a_duplicate_of() {
    let body = r#"{"project":"koji","work_items":[],"triage":{"due":[],"issues":[
        {"issue":30,"comment":101,"step":"done","duplicate_of":41},
        {"issue":32,"comment":103,"step":"closed","duplicate_of":41},
        {"issue":31,"comment":102,"step":"commented"}]},"rulings":[]}"#;
    assert_eq!(
        plain_status("koji", body, None)[4..7],
        [
            "  triaged #30: comment 101, closed as a duplicate of #41",
            "  triaged #32: comment 103, to close as a duplicate of #41",
            "  triaged #31: comment 102, commented",
        ]
    );
}

// The ready path's two steps, as the plain status says them
#[test]
fn a_plain_status_says_an_issue_made_agent_ready_is_ready() {
    let body = r#"{"project":"koji","work_items":[],"triage":{"due":[],"issues":[
        {"issue":30,"comment":101,"step":"body-written"},
        {"issue":31,"comment":102,"step":"ready"}]},"rulings":[]}"#;
    assert_eq!(
        plain_status("koji", body, None)[4..6],
        [
            "  triaged #30: comment 101, body-written",
            "  triaged #31: comment 102, ready",
        ]
    );
}

#[test]
fn a_plain_status_says_an_issue_a_classifier_read_as_spam_was_labelled_for_the_maintainer() {
    let body = r#"{"project":"koji","work_items":[],"triage":{"due":[],"issues":[
        {"issue":30,"comment":0,"step":"done","author":"newcomer","calls":[1790000000],"spam":0.9},
        {"issue":31,"comment":102,"step":"done"}]},"rulings":[]}"#;
    assert_eq!(
        plain_status("koji", body, None)[4..6],
        [
            "  triaged #30: a classifier read it as 0.9 spam, so it is labelled possible-spam with no call \
             of its own",
            "  triaged #31: comment 102, done",
        ]
    );
}

#[test]
fn a_plain_status_lists_each_issue_triage_would_pick_up() {
    let body = r#"{"project":"koji","work_items":[],"triage":{"due":[
        {"issue":30,"title":"It crashes","author":"newcomer","created":1},
        {"issue":31,"title":"A question","author":"","created":2}]},"rulings":[]}"#;
    assert_eq!(
        plain_status("koji", body, None)[4..7],
        [
            "  triage due: 2",
            "    #30 It crashes by newcomer",
            "    #31 A question by a deleted account",
        ]
    );
}

#[test]
fn a_plain_status_says_the_days_triage_count_and_what_a_limit_held() {
    let full = r#"{"project":"koji","work_items":[],"triage":{"due":[],"issues":[
        {"issue":30,"comment":0,"step":"done","author":"newcomer","held":"author-cap"}],
        "daily":{"used":10,"cap":10,"opens":1790000000,"wait":7200}},"rulings":[]}"#;
    assert_eq!(
        plain_status("koji", full, None)[4..6],
        [
            "  triage daily: 10 of 10 used, next place in 2h",
            "  triaged #30: no call, its author already had the day's triage calls",
        ]
    );

    let room = r#"{"project":"koji","work_items":[],"triage":{"due":[],"issues":[],
        "daily":{"used":3,"cap":10}},"rulings":[]}"#;
    assert_eq!(
        plain_status("koji", room, None)[4..5],
        ["  triage daily: 3 of 10 used"]
    );

    // A new account held by the age limit says so in its own words
    let new = r#"{"project":"koji","work_items":[],"triage":{"due":[],"issues":[
        {"issue":31,"comment":0,"step":"done","author":"newcomer","held":"new-account"}]},
        "rulings":[]}"#;
    assert_eq!(
        plain_status("koji", new, None)[4..5],
        ["  triaged #31: no call, its author's account is new and has no history in the repo",]
    );
}

#[test]
fn a_plain_status_says_how_long_a_contribution_has_waited_on_its_contributor() {
    let body = r#"{"project":"koji","work_items":[],"contributions":[
        {"pull_request":12,"author":"newcomer","head":"2a8f1e6d0b4c","seen":1,
         "waiting_since":1790000000,
         "clock":{"waiting_for":90000,"days_to_nudge":2,"days_to_quiet":6}},
        {"pull_request":14,"author":"other","head":"1f7e0d5c9a3b","seen":2,
         "waiting_since":1789000000,
         "clock":{"waiting_for":700000,"days_to_nudge":0,"days_to_quiet":0,"due":"quiet"}}],
        "rulings":[]}"#;
    assert_eq!(
        plain_status("koji", body, None)[4..9],
        [
            "  contributions: 2",
            "    PR #12 by newcomer, head 2a8f1e6",
            "      waiting on the contributor for 1d, nudge in 2d, quiet in 6d",
            "    PR #14 by other, head 1f7e0d5",
            "      waiting on the contributor for 8d, nudge due, quiet",
        ]
    );
}

#[test]
fn a_plain_status_names_the_build_the_runner_is_on() {
    let body = r#"{"project":"koji","work_items":[],"commit":"1f2e3a4","rulings":[]}"#;

    assert_eq!(
        plain_status("koji", body, Some("1f2e3a4"))[2],
        "  build: 1f2e3a4"
    );
}

#[test]
fn a_plain_status_marks_a_runner_that_is_not_on_the_installed_build() {
    let body = r#"{"project":"koji","work_items":[],"commit":"1f2e3a4","rulings":[]}"#;

    assert_eq!(
        plain_status("koji", body, Some("9c8b7a6"))[2],
        "  build: 1f2e3a4, not the installed 9c8b7a6"
    );
}

#[test]
fn a_plain_status_says_a_build_made_from_an_uncommitted_tree_is_dirty() {
    let body = r#"{"project":"koji","work_items":[],"commit":"1f2e3a4-dirty","rulings":[]}"#;

    assert_eq!(
        plain_status("koji", body, Some("1f2e3a4-dirty"))[2],
        "  build: 1f2e3a4-dirty"
    );
}

#[test]
fn a_plain_status_marks_an_uncommitted_build_as_not_the_installed_clean_one() {
    // The runner runs a build made from changes on top of the installed
    // commit, which is a different binary from the one installed.
    let body = r#"{"project":"koji","work_items":[],"commit":"1f2e3a4-dirty","rulings":[]}"#;

    assert_eq!(
        plain_status("koji", body, Some("1f2e3a4"))[2],
        "  build: 1f2e3a4-dirty, not the installed 1f2e3a4"
    );
}

#[test]
fn a_plain_status_says_a_runner_that_names_no_build_is_not_the_installed_one() {
    let body = r#"{"project":"koji","work_items":[],"rulings":[]}"#;

    assert_eq!(
        plain_status("koji", body, Some("9c8b7a6"))[2],
        "  build: not recorded, the installed build is 9c8b7a6"
    );
}

#[tokio::test]
async fn status_names_the_build_of_the_kelpie_the_dog_runs() {
    let shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    let installed = shepherd.scratch("bin").join("shep-kelpie");
    crate::test::write_script(
        &installed,
        &format!(
            "#!/bin/sh\necho '{}'\n",
            r#"{"kelpie":"0.1.0","shep":"0.17.0","commit":"9c8b7a6"}"#
        ),
    );
    shepherd.holds_dog_at("kelpie", &installed.display().to_string(), true);
    shepherd.says("koji", &[r#"{"work_items":[],"commit":"1f2e3a4"}"#]);

    let client = client(&shepherd).await;
    let lines = in_time(status(&reach(), &client, Format::Plain))
        .await
        .unwrap();

    assert!(
        lines.contains(&"  build: 1f2e3a4, not the installed 9c8b7a6".to_owned()),
        "{lines:?}"
    );
}

#[test]
fn a_plain_board_numbers_each_issue_with_its_title_priority_and_why_it_waits() {
    let list = r#"{"project":"koji","board":[
        {"position":1,"issue":"284","title":"Add the list","priority":"P0","blocked":false,"held":false},
        {"position":2,"issue":"282","title":"Order it","priority":null,"blocked":true,"held":false},
        {"position":3,"issue":"k12","title":"Show it","priority":"P2","blocked":true,"held":true}]}"#;
    assert_eq!(
        plain_reply("koji", "board", None, list),
        [
            "1. #284 Add the list (P0)",
            "2. #282 Order it (no priority, blocked)",
            "3. k12 Show it (P2, blocked, held)",
        ]
    );
    assert_eq!(
        plain_reply("koji", "board", None, r#"{"project":"koji","board":[]}"#),
        ["koji: no issue is ready on the board"]
    );
    // A board from a runner from before ticket ids, which wrote a number.
    assert_eq!(
        plain_reply(
            "koji",
            "board",
            None,
            r#"{"project":"koji","board":[{"position":1,"issue":284,"title":"Add the list","priority":"P0"}]}"#,
        ),
        ["1. #284 Add the list (P0)"]
    );
}

const STATUS_OBJECT: &str = r#"{"project":"koji","work_items":[],"rulings":[]}"#;

#[test]
fn a_plain_triage_says_the_issue_is_queued_for_triage() {
    assert_eq!(
        plain_reply("koji", "triage", Some("80"), STATUS_OBJECT),
        ["koji: queued #80 for triage"]
    );
    assert_eq!(
        plain_reply("koji", "triage", Some("k12"), STATUS_OBJECT),
        ["koji: queued k12 for triage"]
    );
    assert_eq!(
        plain_reply("koji", "triage", None, STATUS_OBJECT),
        ["koji: queued ? for triage"]
    );
    assert_eq!(
        plain_reply("koji", "triage", Some("80"), "not json"),
        ["not json"]
    );
}

#[test]
fn a_plain_rework_and_adopt_say_the_pull_request_is_queued() {
    assert_eq!(
        plain_reply("koji", "rework", Some("40"), STATUS_OBJECT),
        ["koji: queued a rework of PR #40"]
    );
    assert_eq!(
        plain_reply("koji", "adopt", Some("40"), STATUS_OBJECT),
        ["koji: queued adoption of PR #40"]
    );
}

#[test]
fn a_plain_status_of_an_idle_runner_says_so() {
    let idle = r#"{"project":"koji","work_items":[],"rulings":[],
        "pacer":{"enabled":false,"claude":{"reading":null,"holding":null}}}"#;
    assert_eq!(
        plain_status("koji", idle, None),
        [
            "koji",
            "  run: running",
            "  build: not recorded",
            "  work items: none",
            "  rulings waiting: none",
            "  pacer: off",
            "    claude: not read",
        ]
    );
}

#[test]
fn a_plain_status_shows_a_refused_settings_change_and_when() {
    let refused = r#"{"project":"koji","work_items":[],"rulings":[],
        "settings_refused":{"reason":"agents.reviewers: free plan reviews public repos only",
        "at":1790000000},
        "pacer":{"enabled":false,"claude":{"reading":null,"holding":null}}}"#;
    assert_eq!(
        plain_status("koji", refused, None)[..4],
        [
            "koji",
            "  run: running",
            "  build: not recorded",
            "  settings change refused at 2026-09-21 14:13 UTC, so the old settings stay: \
             agents.reviewers: free plan reviews public repos only",
        ]
    );
}

#[test]
fn a_plain_status_shows_an_adopted_pull_request_that_keeps_failing_and_since_when() {
    let failing = r#"{"project":"koji","work_items":[],"rulings":[],
        "skipped":[{"reason":"adopt","pull_request":80,"error":"cannot prepare its worktree: busy",
        "since":1790000000}],
        "pacer":{"enabled":false,"claude":{"reading":null,"holding":null}}}"#;
    assert_eq!(
        plain_status("koji", failing, None)[3..5],
        [
            "  work items: none",
            "  adopted, cannot start: PR #80 since 2026-09-21 14:13 UTC: \
             cannot prepare its worktree: busy",
        ]
    );
}

#[tokio::test]
async fn a_plain_status_keeps_a_project_that_is_not_running_to_a_line() {
    let shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), false);
    shepherd.holds_dog("kelpie", true);
    let client = client(&shepherd).await;
    assert_eq!(
        in_time(status(&reach(), &client, Format::Plain))
            .await
            .unwrap(),
        ["koji: not running"]
    );
}

#[test]
fn a_json_status_is_the_runner_s_object_as_it_came() {
    assert_eq!(
        json_status("koji", RECORDED),
        format!("koji (finishing): {RECORDED}")
    );
}
