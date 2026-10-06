//! The pushed head, which every review round reads and the merge gate
//! takes, through the runner's stand-ins

use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Value, json};

use crate::ports::{Checks, Cost, PullRequestState, Usage};
use crate::runner::{CHECKS_SETTLE, Runner, StepReport, step};
use crate::test::{Hold, Rig, Scripted, ScriptedRound, git};

// The worker's first turn pushed `work.txt` and opened the pull request, and
// the rest of `script` is scripted after it. No review round has run.
fn pushed_once(project: &str, script: Vec<Scripted>) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    let mut all = vec![Scripted::Push("work.txt", "work\n")];
    all.extend(script);
    rig.claude.script(all);
    step(&runner).unwrap();
    (rig, runner)
}

// A file a command the worker left running writes once its turn has ended
fn left_behind(rig: &Rig, file: &str) {
    std::fs::write(rig.worktree_7().join(file), "late\n").unwrap();
}

fn head_of(rig: &Rig) -> (String, String) {
    let worktree = git(&rig.worktree_7(), &["rev-parse", "HEAD"]);
    (worktree, rig.forge.head_of("kelpie/7").unwrap())
}

// Steps until the work item reaches CI, and returns the head CI sees
fn to_ci(rig: &Rig, runner: &Mutex<Runner>) -> String {
    for _ in 0..20 {
        step(runner).unwrap();
        if rig.ask(runner, "status", None)["work_item"]["phase"]["state"] == "ci" {
            return rig.forge.head_of("kelpie/7").unwrap();
        }
    }
    panic!("the review never reached CI");
}

fn ruling_report(report: Option<StepReport>) -> (u64, String) {
    match report {
        Some(StepReport::Ruling { id, question, .. }) => (id, question),
        other => panic!("no ruling was raised: {other:?}"),
    }
}

fn kind(rig: &Rig, runner: &Mutex<Runner>) -> Value {
    rig.ask(runner, "status", None)["rulings"][0]["kind"].clone()
}

#[test]
fn a_worktree_left_dirty_gets_one_turn_to_push_before_the_round_reads_the_pushed_head() {
    let (rig, runner) = pushed_once(
        "shep",
        vec![
            Scripted::Push("late.txt", "late\n"),
            Scripted::Text("CLEAN"),
        ],
    );
    left_behind(&rig, "late.txt");
    let (head, _) = head_of(&rig);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Unpushed {
            issue: 7,
            pull_request: 71,
            round: 1,
            files: vec!["late.txt".into()],
            head: head.clone(),
            pushed: head,
        })
    );
    assert!(rig.reviewer.seen().is_empty(), "no round read the worktree");

    step(&runner).unwrap(); // the turn to push
    let [_, push] = rig.claude.calls().try_into().unwrap();
    assert!(
        push.prompt.starts_with(
            "Before the review reads your pull request, your worktree must hold exactly \
             what is pushed"
        ),
        "{}",
        push.prompt
    );
    assert!(
        push.prompt
            .contains("It does not: it holds uncommitted changes: late.txt."),
        "{}",
        push.prompt
    );

    let read = to_ci(&rig, &runner);
    assert_eq!(rig.reviewer.seen().len(), 1, "the round ran once pushed");
    assert_eq!(head_of(&rig), (read.clone(), read.clone()));
    rig.forge.set_checks(&read, Checks::Passed);
    ruling_report(rig.verdict(&runner));
    assert_eq!(
        kind(&rig, &runner),
        json!({ "kind": "merge", "head": read })
    );
}

#[test]
fn a_turn_to_push_that_commits_without_pushing_starts_no_review_round() {
    let (rig, runner) = pushed_once(
        "koji",
        vec![
            Scripted::Commit("late.txt", "late\n"),
            Scripted::Push("more.txt", "more\n"),
            Scripted::Text("CLEAN"),
        ],
    );
    left_behind(&rig, "late.txt");
    step(&runner).unwrap(); // sent to push
    step(&runner).unwrap(); // the turn commits and pushes nothing
    let (head, pushed) = head_of(&rig);
    assert_ne!(head, pushed);

    let (id, question) = ruling_report(step(&runner).unwrap());
    assert_eq!(
        question,
        format!(
            "The worker on pull request #71 still has work not pushed after its turn to \
             push or discard it: it has {} checked out, and the head on origin is {}. \
             Round 1 of the review did not run. `shep kelpie rule {id} yes` gives the \
             worker another turn to push or discard it, and `shep kelpie rule {id} no \
             <note>` sends the worker your note.",
            &head[..7],
            &pushed[..7]
        )
    );
    assert_eq!(
        kind(&rig, &runner),
        json!({
            "kind": "stuck",
            "reason": "unpushed",
            "head": head,
            "pushed": pushed,
            "review": { "round": 1, "stage": { "stage": "round" }, "unread": true },
        })
    );
    assert!(rig.reviewer.seen().is_empty(), "no round ran");
    assert_eq!(rig.claude.calls().len(), 2, "no turn after the one to push");

    // A yes is another turn to push, and the round reads what it pushed.
    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    step(&runner).unwrap();
    let again = rig.claude.calls().pop().unwrap();
    assert!(
        again
            .prompt
            .contains(&format!("it has {} checked out", &head[..7])),
        "{}",
        again.prompt
    );
    let read = to_ci(&rig, &runner);
    assert_eq!(rig.reviewer.seen().len(), 1);
    assert_eq!(head_of(&rig), (read.clone(), read));
}

#[test]
fn a_turn_to_push_that_leaves_files_dirty_parks_naming_them_and_both_heads() {
    let (rig, runner) = pushed_once("chelone", vec![Scripted::Plant("other.txt", "other\n")]);
    left_behind(&rig, "late.txt");
    step(&runner).unwrap(); // sent to push
    step(&runner).unwrap(); // the turn writes another file and commits nothing
    let (head, pushed) = head_of(&rig);

    let (_, question) = ruling_report(step(&runner).unwrap());
    assert!(
        question.contains(
            "after its turn to push or discard it: it holds uncommitted changes: \
             late.txt, other.txt. Round 1 of the review did not run."
        ),
        "{question}"
    );
    assert_eq!(
        kind(&rig, &runner),
        json!({
            "kind": "stuck",
            "reason": "unpushed",
            "files": ["late.txt", "other.txt"],
            "head": head,
            "pushed": pushed,
            "review": { "round": 1, "stage": { "stage": "round" }, "unread": true },
        })
    );
    assert_eq!(
        rig.forge.comments(),
        [(
            71,
            "Work in this pull request's worktree is not pushed, so its review waits.\n\n\
             Waiting on the maintainer."
                .to_owned()
        )]
    );
    assert_eq!(
        rig.claude.calls().len(),
        2,
        "no follow-up to commit as well"
    );
    assert!(rig.reviewer.seen().is_empty());
}

// The fix of the last reviewer's findings goes to CI with no read, as the
// design sends it, and `auto` merges it.
#[test]
fn under_auto_the_last_reviewers_fix_merges_unread_with_no_ruling() {
    let rig = Rig::new("rotom");
    rig.merge_auto();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("HIGH|work.txt:1|wrong|it is"),
        Scripted::Push("fixed.txt", "fixed\n"),
    ]);
    let fixed = to_ci(&rig, &runner);
    rig.forge.set_checks(&fixed, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
    rig.clock.advance(CHECKS_SETTLE);
    step(&runner).unwrap();
    assert_eq!(rig.forge.merges(), [(71, fixed)]);
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
}

#[test]
fn under_auto_a_fix_for_red_ci_merges_with_no_ruling() {
    let (rig, runner, head) = Rig::with_pull_request_set("acme", Rig::merge_auto);
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".into()]));
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::CiFailed { .. })
    ));
    rig.claude.script([Scripted::Push("fix.txt", "fixed\n")]);
    step(&runner).unwrap();
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&fixed, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
    rig.clock.advance(CHECKS_SETTLE);
    step(&runner).unwrap();
    assert_eq!(rig.forge.merges(), [(71, fixed)]);
}

// A work item saved before heads were recorded knows of no read of its head.
#[test]
fn under_auto_a_head_no_round_read_raises_the_merge_ruling_instead_of_merging() {
    let (rig, runner, head) = Rig::with_pull_request_set("golbat", Rig::merge_auto);
    drop(runner);
    let state = rig.paths().state;
    let mut saved: Value = serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
    saved["version"] = json!(12);
    let item = saved["work_items"][0].as_object_mut().unwrap();
    assert_eq!(item.remove("reviewed_heads"), Some(json!([head])));
    std::fs::write(&state, saved.to_string()).unwrap();

    let runner = rig.open().unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    let (_, question) = ruling_report(rig.verdict(&runner));
    assert!(
        question.starts_with(&format!(
            "Merge pull request #71 at {} into main? Kelpie has no record of a review \
             round reading this head, or of a fix turn it sent pushing it.",
            &head[..7]
        )),
        "{question}"
    );
    assert_eq!(
        kind(&rig, &runner),
        json!({ "kind": "merge", "head": head, "unread_head": true })
    );
    assert_eq!(rig.forge.merges(), []);
}

#[test]
fn a_head_pushed_by_hand_reaches_the_merge_ruling_named_unread() {
    let (rig, runner, head) = Rig::with_pull_request("xilriws");
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["lint".into()]));
    rig.verdict(&runner);
    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(1))]);
    step(&runner).unwrap();
    ruling_report(rig.verdict(&runner)); // still red, nothing pushed
    let fixed = rig.push_by_hand("kelpie/7", "lint.txt");
    rig.ask(&runner, "rule", Some("1 yes"));
    rig.forge.set_checks(&fixed, Checks::Passed);

    let (_, question) = ruling_report(rig.verdict(&runner));
    assert!(
        question.contains("no record of a review round reading this head"),
        "{question}"
    );
    assert_eq!(
        kind(&rig, &runner),
        json!({ "kind": "merge", "head": fixed, "unread_head": true })
    );
}

// #278's follow-up to commit is the worker's one turn to push, so a round
// that then finds the worktree off the pushed head parks with no other.
#[test]
fn a_follow_up_to_commit_counts_as_the_turn_to_push() {
    let rig = Rig::new("webapp");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([
        Scripted::Plant("work.txt", "work\n"),
        Scripted::Push("work.txt", "work\n"),
    ]);
    step(&runner).unwrap(); // the turn leaves work.txt uncommitted
    step(&runner).unwrap(); // the follow-up pushes it
    let [_, follow_up] = rig.claude.calls().try_into().unwrap();
    assert!(
        follow_up
            .prompt
            .starts_with("Your last turn ended with uncommitted changes"),
        "{}",
        follow_up.prompt
    );
    left_behind(&rig, "late.txt");

    let (_, question) = ruling_report(step(&runner).unwrap());
    assert!(
        question.contains("it holds uncommitted changes: late.txt."),
        "{question}"
    );
    assert_eq!(rig.claude.calls().len(), 2);
    assert!(rig.reviewer.seen().is_empty());
}

// Merged by hand with its branch deleted, the pull request leaves `origin`
// nothing to read, and the work item ends as the gate ends one merged.
#[test]
fn a_pull_request_merged_by_hand_with_its_branch_deleted_ends_at_the_round() {
    let (rig, runner) = pushed_once("acme", Vec::new());
    // The forge keeps the head a merged pull request had, branch or not.
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_lagging(71, Some(&head));
    git(
        &rig.worktree_7(),
        &["push", "--quiet", "origin", "--delete", "kelpie/7"],
    );
    rig.forge.set_state(71, PullRequestState::Merged);
    let report = step(&runner).unwrap();
    assert!(
        matches!(
            report,
            Some(StepReport::Finished {
                issue: 7,
                pull_request: Some(71),
                merged: true,
                ..
            })
        ),
        "{report:?}"
    );
    assert!(rig.reviewer.seen().is_empty());
    assert_eq!(rig.forge.merges(), []);
    assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
}

// A push by someone else while the worker fixes red CI is not the worker's
// fix, so nothing vouches for it.
#[test]
fn under_auto_a_push_by_someone_else_during_a_fix_turn_is_named_unread() {
    let (rig, runner, head) = Rig::with_pull_request_set("rotom", Rig::merge_auto);
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".into()]));
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::CiFailed { .. })
    ));
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    // The fix turn pushes nothing of its own, and the hand push lands while it runs.
    let hand = std::thread::scope(|scope| {
        let pusher = scope.spawn(|| {
            assert!(
                hold.entered(Duration::from_secs(10)),
                "the turn never began"
            );
            let hand = rig.push_by_hand("kelpie/7", "hand.txt");
            hold.release();
            hand
        });
        step(&runner).unwrap();
        pusher.join().unwrap()
    });
    rig.forge.set_checks(&hand, Checks::Passed);

    let (_, question) = ruling_report(rig.verdict(&runner));
    assert!(
        question.contains("no record of a review round reading this head"),
        "{question}"
    );
    assert_eq!(
        kind(&rig, &runner),
        json!({ "kind": "merge", "head": hand, "unread_head": true })
    );
    assert_eq!(rig.forge.merges(), []);
}

// Who reviews is chosen from the worktree's diff, so a worktree that does
// not hold a pushed path must not pass the reviewer limited to it over.
#[test]
fn a_worktree_hiding_a_pushed_path_is_brought_to_the_pushed_head_before_the_reviewer_is_chosen() {
    let rig = Rig::new("koji");
    rig.merge_auto();
    rig.reviewers(&["opus"]);
    rig.write_agent(
        "opus",
        "---\nrole: reviewer\nharness: claude-code\nmodel: claude-opus-5-5\n\
         effort: high\npaths: [\"src/**\"]\n---\nRead {{DIFF}}.\n",
    );
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([
        Scripted::Push("src/lib.rs", "pub fn f() {}\n"),
        Scripted::Say("pulled"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the worker's first turn pushes src/lib.rs
    let worktree = rig.worktree_7();
    git(&worktree, &["reset", "--quiet", "--hard", "HEAD~1"]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Unpushed { .. })
    ));
    // The worker's turn brings its worktree back to the pushed head.
    git(
        &worktree,
        &["reset", "--quiet", "--hard", "origin/kelpie/7"],
    );
    let read = to_ci(&rig, &runner);
    let opus = rig
        .claude
        .all_calls()
        .into_iter()
        .filter(|c| c.model == "claude-opus-5-5");
    assert_eq!(opus.count(), 1, "the reviewer limited to src/ read it");
    rig.forge.set_checks(&read, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
    rig.clock.advance(CHECKS_SETTLE);
    step(&runner).unwrap();
    assert_eq!(rig.forge.merges(), [(71, read)]);
}

// Under `auto`, with qwen alone listed, a local round whose worktree `meddle`
// changes while it runs: its read vouches for nothing, so green CI raises the
// merge ruling naming the head unread.
fn changed_while_read(project: &str, meddle: impl Fn(&std::path::Path) + Sync) {
    let rig = Rig::new(project);
    rig.merge_auto();
    rig.reviewers(&["qwen"]);
    let (queued, running) = (Hold::default(), Hold::default());
    rig.reviewer.script([ScriptedRound::Queued {
        queued: queued.clone(),
        running: running.clone(),
    }]);
    let (rig, runner) = {
        let runner = rig.open().unwrap();
        rig.ask(&runner, "add", Some("7"));
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        rig.claude.script([Scripted::Push("work.txt", "work\n")]);
        step(&runner).unwrap();
        (rig, runner)
    };
    queued.release();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            assert!(
                running.entered(Duration::from_secs(10)),
                "the round never ran"
            );
            meddle(&rig.worktree_7());
            running.release();
        });
        step(&runner).unwrap(); // the round, clean
    });
    let head = to_ci(&rig, &runner);
    rig.forge.set_checks(&head, Checks::Passed);
    let (_, question) = ruling_report(rig.verdict(&runner));
    assert!(
        question.contains("no record of a review round reading this head"),
        "{question}"
    );
    assert_eq!(rig.forge.merges(), []);
}

#[test]
fn a_head_moved_while_a_round_reads_it_is_not_vouched_for() {
    changed_while_read("koji", |worktree| {
        std::fs::write(worktree.join("moved.txt"), "moved\n").unwrap();
        git(worktree, &["add", "moved.txt"]);
        git(worktree, &["commit", "--quiet", "-m", "moved"]);
    });
}

#[test]
fn a_worktree_dirtied_while_a_round_reads_it_is_not_vouched_for() {
    changed_while_read("rotom", |worktree| {
        std::fs::write(worktree.join("dirty.txt"), "dirty\n").unwrap();
    });
}
