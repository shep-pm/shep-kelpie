//! A project's `publish` hooks, through the runner's stand-ins: a word its
//! hook refuses reaches the forge from no one's text
//!
//! The hook is the rig's `forbid_word`, which refuses with exit 2 and a
//! reason that does not repeat the word. A worker's commit is pinned in
//! `guard`, where `kelpie guard` runs the `commit` hook.

use std::sync::Mutex;

use serde_json::json;

use crate::confine::Verdict;
use crate::guard::{Checkout, judge_with};
use crate::local_paths::LocalPaths;
use crate::ports::Role;
use crate::runner::{Runner, StepReport, step};
use crate::settings::AgentHarness;
use crate::test::{Rig, Scripted, git, write_in};

const WORD: &str = "Acme";

const PI: &str = "---\nrole: implementer\nharness: pi\nmodel: qwen3.8:27b\neffort: low\n\
                  url: http://box:11434/v1\ncontext: 65536\n---\n";

const REFUSED: &str = "the `publish` hook `forbid-word.sh` refused: it names a word the project \
                       keeps private";

// A running project that has the forge refuse `WORD`, whose issue 7 is in flight and
// whose build folder is made. The App covers its repo, so the worker hands its pull
// request over in a file.
fn handing_over() -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("shep");
    rig.with_app();
    rig.forbid_word(&["publish"], WORD);
    let runner = rig.open().unwrap();
    assert_eq!(
        rig.ask(&runner, "add", Some("7"))["work_item"]["issue"],
        "7"
    );
    std::fs::create_dir_all(rig.build_7()).unwrap();
    (rig, runner)
}

fn hand_over(rig: &Rig, text: &str) {
    let file = super::pull_request_file::path(&rig.build_7());
    std::fs::write(file, text).unwrap();
}

#[test]
fn a_worker_s_pull_request_body_naming_the_word_is_not_posted_and_the_worker_is_told() {
    let (rig, runner) = handing_over();
    hand_over(
        &rig,
        &format!("feat: x\n\nBuilt for {WORD}.\n\nResolves #7\n"),
    );
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);

    step(&runner).unwrap(); // the turn pushes, and the body is refused

    assert_eq!(rig.forge.text_of(500), None);
    let notes = runner.lock().unwrap().take_notes().join("\n");
    assert!(notes.contains(REFUSED), "{notes}");

    hand_over(&rig, "feat: x\n\nBuilt for a customer.\n\nResolves #7\n");
    rig.claude.script([Scripted::Say("fixed the file")]);
    step(&runner).unwrap(); // sent back with the hook's reason
    let prompts: Vec<String> = rig
        .claude
        .seen()
        .into_iter()
        .map(|s| s.call.prompt)
        .collect();
    let told = prompts.last().unwrap();
    assert!(told.contains("pull-request.md"), "{told}");
    assert!(told.contains(REFUSED), "{told}");
    assert!(!told.contains(WORD), "{told}");

    assert_eq!(
        rig.forge.text_of(500),
        Some((
            "feat: x".to_owned(),
            "Built for a customer.\n\nResolves #7".to_owned()
        )),
        "opened from the fixed file"
    );
}

// The reason is kept in the state, so a restart between the turn and the next
// step still tells the worker which hook refused and why.
#[test]
fn a_restart_keeps_the_hooks_reason_for_the_worker() {
    let (rig, runner) = handing_over();
    hand_over(
        &rig,
        &format!("feat: x\n\nBuilt for {WORD}.\n\nResolves #7\n"),
    );
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the turn pushes, and the body is refused
    drop(runner);

    let runner = rig.open().unwrap();
    hand_over(&rig, "feat: x\n\nBuilt for a customer.\n\nResolves #7\n");
    rig.claude.script([Scripted::Say("fixed the file")]);
    step(&runner).unwrap(); // sent back with the hook's reason

    let prompts: Vec<String> = (rig.claude.seen().into_iter())
        .map(|s| s.call.prompt)
        .collect();
    let told = prompts.last().unwrap();
    assert!(told.contains(REFUSED), "{told}");
}

#[test]
fn a_pull_request_the_hook_keeps_refusing_parks_on_a_ruling_naming_the_hook() {
    let (rig, runner) = handing_over();
    hand_over(
        &rig,
        &format!("feat: x\n\nBuilt for {WORD}.\n\nResolves #7\n"),
    );
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Say("still the same"),
    ]);

    step(&runner).unwrap(); // the first turn, refused
    step(&runner).unwrap(); // sent back, and its turn refused again
    let Some(StepReport::Failed { question, .. }) = step(&runner).unwrap() else {
        panic!("the file the hook keeps refusing did not park the item");
    };

    assert_eq!(rig.forge.text_of(500), None);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "ruling");
    let ruling = question;
    assert!(ruling.contains("forbid-word.sh"), "{ruling}");
    assert!(
        ruling.contains("it names a word the project keeps private"),
        "{ruling}"
    );
    assert!(!ruling.contains(WORD), "{ruling}");
}

#[test]
fn a_ruling_post_naming_the_word_is_not_posted_and_the_item_stays_parked() {
    let rig = Rig::new("shep");
    rig.forbid_word(&["publish"], WORD);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Say(
        "<kelpie-question>\nShould Acme's flag be `--dry-run`?\n</kelpie-question>\n",
    )]);

    let Some(StepReport::Asked {
        pull_request: Some(71),
        question,
        comment_failed,
        ..
    }) = step(&runner).unwrap()
    else {
        panic!("the question was not about pull request 71");
    };

    assert_eq!(
        comment_failed.as_deref(),
        Some(format!("not posted: the comment: {REFUSED}").as_str())
    );
    assert_eq!(rig.forge.comments(), []);
    assert!(
        question.contains(WORD),
        "the maintainer still gets the question"
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
        "ruling"
    );
}

#[test]
fn a_hook_that_fails_to_run_refuses_the_post_and_says_so() {
    let rig = Rig::new("shep");
    rig.forbid_word(&["publish"], WORD);
    std::fs::write(
        rig.home.path().join("forbid-word.sh"),
        "#!/bin/sh\nexit 1\n",
    )
    .unwrap();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Say(
        "<kelpie-question>\nWhich flag?\n</kelpie-question>\n",
    )]);

    let Some(StepReport::Asked { comment_failed, .. }) = step(&runner).unwrap() else {
        panic!("the question raised no ruling");
    };

    let why = comment_failed.expect("the comment was not posted");
    assert!(why.contains("`forbid-word.sh` failed"), "{why}");
    assert!(why.contains("it exited with 1"), "{why}");
    assert_eq!(rig.forge.comments(), []);
}

// The call a harness makes of `kelpie guard` for a Bash command in `fence`.
fn guarded(fence: &crate::ports::Fence, command: &str) -> Verdict {
    let guard = &fence.guard;
    let call = json!({
        "tool_name": "Bash",
        "cwd": guard.worktree,
        "tool_input": { "command": command },
    });
    let checkout = Checkout {
        git_common_dir: &guard.git_common_dir,
        worktree: &guard.worktree,
        base: "main",
        kelpie_pushes: false,
    };
    judge_with(
        call.to_string().as_bytes(),
        None,
        LocalPaths::new(None::<&std::path::Path>),
        &guard.hooks,
        checkout,
    )
}

#[test]
fn a_pi_implementer_with_a_commit_hook_is_not_refused_at_load_and_its_commit_is_refused_by_the_hook()
 {
    let rig = Rig::new("shep");
    rig.write_agent("pi-box", PI);
    rig.agent_board.ranks(&["pi-box"]);
    rig.forbid_word(&["commit"], WORD);
    let runner = rig.open().expect("a hook is not a Claude Code hook");
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Say("done")]);

    step(&runner).unwrap();

    let [seen] = rig.claude.seen().try_into().unwrap();
    assert_eq!(
        (
            seen.call.role,
            matches!(seen.call.harness, AgentHarness::Pi(_))
        ),
        (Role::Worker, true)
    );
    let fence = seen
        .call
        .reach
        .fence
        .as_deref()
        .expect("a worker is fenced");
    let worktree = &fence.guard.worktree;
    write_in(worktree, "notes.md", "built for Acme\n");
    git(worktree, &["add", "notes.md"]);
    let Verdict::Refuse(why) = guarded(fence, "git commit -m 'docs: notes'") else {
        panic!("the commit was let through");
    };
    assert!(
        why.contains(
            "this commit: the `commit` hook `forbid-word.sh` refused: it names a word the \
             project keeps private"
        ),
        "{why}"
    );
    git(worktree, &["reset", "--quiet"]);
    write_in(worktree, "notes.md", "built for a customer\n");
    git(worktree, &["add", "notes.md"]);
    assert_eq!(
        guarded(fence, "git commit -m 'docs: notes'"),
        Verdict::Allow
    );
}

// How many runs of the project's hooks the metrics endpoint counted as `outcome`
fn hook_runs(rig: &Rig, outcome: &str) -> Option<f64> {
    let labels = [("feature", "hooks"), ("outcome", outcome)];
    rig.samples.value("kelpie_feature_runs_total", &labels)
}

#[test]
fn a_hook_that_exits_0_or_refuses_on_purpose_counts_as_an_ok_run() {
    let passing = Rig::new("shep");
    passing.hook("gate.sh", &["turn_start"], "exit 0");
    let runner = passing.open().unwrap();
    passing.ask(&runner, "add", Some("7"));
    step(&runner).unwrap();
    assert!(hook_runs(&passing, "ok") >= Some(1.0));
    assert_eq!(hook_runs(&passing, "failed"), None);

    let refusing = Rig::new("shep");
    refusing.hook("gate.sh", &["turn_start"], "echo 'not now' >&2; exit 2");
    let runner = refusing.open().unwrap();
    refusing.ask(&runner, "add", Some("7"));
    step(&runner).unwrap();
    assert!(hook_runs(&refusing, "ok") >= Some(1.0));
    assert_eq!(hook_runs(&refusing, "failed"), None);
}

#[test]
fn a_hook_that_fails_to_run_counts_as_a_failed_run_and_none_as_ok() {
    let rig = Rig::new("shep");
    rig.hook("gate.sh", &["turn_start"], "exit 1");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    step(&runner).unwrap();
    assert!(hook_runs(&rig, "failed") >= Some(1.0));
    assert_eq!(hook_runs(&rig, "ok"), None);
}

#[test]
fn a_project_with_no_hooks_counts_no_hook_runs() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    step(&runner).unwrap();
    assert_eq!(hook_runs(&rig, "ok"), None);
    assert_eq!(hook_runs(&rig, "failed"), None);
}
