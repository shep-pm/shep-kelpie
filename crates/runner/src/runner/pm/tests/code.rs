//! What the project manager reads of the code: its project's worktrees and
//! checkout, and each open branch's diff that kelpie writes it at a wake

use std::path::PathBuf;
use std::sync::Mutex;

use super::{answered, past_the_window, pm_seen, step_until, with_pm};
use crate::ports::AgentError;
use crate::profile::CREDENTIALS;
use crate::runner::pm::call::SECRET_SHAPED;
use crate::runner::{Runner, StepReport};
use crate::settings::Harness;
use crate::state::STATE_FILE;
use crate::test::{Rig, Scripted, Seen, assert_reads, assert_refuses, git, ticket, write_in};

const NOTHING: &str = "{\"pick\": null}";

/// A project with #7 open, whose worker committed `file` on its branch and
/// then failed, and the project manager's wake for it
fn with_open_item(
    project: &str,
    file: &'static str,
    text: &'static str,
) -> (Rig, Mutex<Runner>, Seen) {
    with_open_commit(project, Scripted::Commit(file, text))
}

/// [`with_open_item`] for a worker that commits as `commit` says
fn with_open_commit(project: &str, commit: Scripted) -> (Rig, Mutex<Runner>, Seen) {
    let (rig, runner) = with_pm(project);
    rig.ask(&runner, "add", Some("7"));
    let failed = AgentError::Failed(Harness::ClaudeCode, "it crashed".into());
    rig.claude
        .script([commit, Scripted::Fail(failed), Scripted::Say(NOTHING)]);
    step_until(
        &runner,
        |r| matches!(r, StepReport::Failed { issue, .. } if *issue == ticket(7)),
    );
    past_the_window(&rig);
    answered(Some(step_until(&runner, |r| {
        matches!(r, StepReport::PmAnswered { .. })
    })));
    let seen = pm_seen(&rig).pop().unwrap();
    (rig, runner, seen)
}

/// Tells the project manager something, and waits for its answer
fn wake(rig: &Rig, runner: &Mutex<Runner>) -> Seen {
    rig.ask(runner, "tell", Some("how is #7?"));
    rig.claude.script([Scripted::Say(NOTHING)]);
    answered(Some(step_until(runner, |r| {
        matches!(r, StepReport::PmAnswered { .. })
    })));
    pm_seen(rig).pop().unwrap()
}

#[test]
fn it_reads_the_worktrees_and_checkout_and_none_of_kelpies_secrets() {
    let (rig, runner, _) = with_open_item("koji", "src/lib.rs", "pub fn x() {}\n");
    let paths = rig.paths();
    let home = &paths.kelpie_home;
    write_in(
        &home.join("github/acme"),
        "key.pem",
        "-----BEGIN PRIVATE KEY-----\n",
    );
    write_in(&home.join("other"), STATE_FILE, "{}\n");
    write_in(&rig.repo(), "notes.txt", "the checkout\n");
    let seen = wake(&rig, &runner);

    assert_reads(&seen, &paths.worktree(ticket(7)).join("src/lib.rs"));
    assert_reads(&seen, &rig.repo().join("notes.txt"));
    assert_reads(&seen, &paths.pm.join("board.md"));
    let unread: [PathBuf; 4] = [
        home.join("github/acme/key.pem"),
        home.join("other/state.json"),
        paths.state.clone(),
        paths.folder.join("settings.toml"),
    ];
    for path in unread {
        assert!(path.is_file(), "{path:?} is a real file");
        assert_refuses(&seen, &path);
    }
    let deny = seen.settings["permissions"]["deny"].to_string();
    let sandbox = seen.sandbox["filesystem"]["denyRead"].to_string();
    for secret in CREDENTIALS.iter().chain(&["~/.config/gh/**"]) {
        assert!(
            deny.contains(&format!("Read({secret})")),
            "{secret}: {deny}"
        );
        assert!(
            sandbox.contains(&format!("\"{secret}\"")),
            "{secret}: {sandbox}"
        );
    }
}

#[test]
fn every_secret_shaped_file_in_the_code_it_reads_is_denied_in_both_layers() {
    let (rig, _runner, seen) = with_open_item("chelone", "src/lib.rs", "pub fn x() {}\n");
    let deny = seen.settings["permissions"]["deny"].as_array().unwrap();
    let sandbox = seen.sandbox["filesystem"]["denyRead"].as_array().unwrap();
    for tree in [rig.paths().worktrees(), rig.repo()] {
        for file in SECRET_SHAPED {
            let path = format!("{}/{file}", tree.display());
            let rule = format!("Read(/{path})");
            assert!(deny.contains(&rule.clone().into()), "{rule}: {deny:?}");
            assert!(
                sandbox.contains(&path.clone().into()),
                "{path}: {sandbox:?}"
            );
        }
    }
}

#[test]
fn a_wake_writes_each_open_branchs_diff_and_none_once_it_closes() {
    let (rig, runner, seen) = with_open_item("rotom", "src/lib.rs", "pub fn x() {}\n");
    let pm = rig.paths().pm;
    assert_eq!(seen.call.cwd, pm);
    let diff = std::fs::read_to_string(pm.join("diffs/7.diff")).unwrap();
    assert!(diff.contains("+++ b/src/lib.rs\n"), "{diff}");
    assert!(diff.contains("+pub fn x() {}\n"), "{diff}");
    let board = std::fs::read_to_string(pm.join("board.md")).unwrap();
    let worktree = rig.paths().worktree(ticket(7)).display().to_string();
    assert!(
        board.contains(&format!("- #7: worktree `{worktree}`, diff `diffs/7.diff`")),
        "{board}"
    );

    rig.ask(&runner, "drop", Some("7"));
    wake(&rig, &runner);
    let left: Vec<_> = std::fs::read_dir(pm.join("diffs")).unwrap().collect();
    assert!(left.is_empty(), "a closed item's diff stays: {left:?}");
    let board = std::fs::read_to_string(pm.join("board.md")).unwrap();
    assert!(!board.contains("diffs/7.diff"), "{board}");
}

#[test]
fn an_oversized_diff_is_cut_and_says_how_big_it_was() {
    let line = "the same long line of a generated file, again and again\n";
    let text: &'static str = line.repeat(2_000).leak();
    let (rig, _runner, _) = with_open_item("eevee", "generated.txt", text);
    let diff = std::fs::read(rig.paths().pm.join("diffs/7.diff")).unwrap();
    assert!(diff.len() <= 64 * 1024 + 500, "{} bytes", diff.len());
    let diff = String::from_utf8(diff).unwrap();
    let (kept, cut) = diff.rsplit_once("\n[kelpie cut this diff at ").unwrap();
    assert!(kept.ends_with(&format!("+{line}")), "cut at a line's end");
    let (at, rest) = cut.split_once(" of its ").unwrap();
    assert_eq!(at, "64 KiB");
    let total: usize = rest.split_once(' ').unwrap().0.parse().unwrap();
    assert!(
        total > text.len() + 2_000,
        "{total} names the whole diff's bytes"
    );
    let worktree = rig.paths().worktree(ticket(7)).display().to_string();
    assert!(
        rest.ends_with(&format!(
            "; files cut or past the cut: generated.txt; read the worktree for the rest at {worktree}]\n"
        )),
        "{rest}"
    );
}

#[test]
fn a_cut_diffs_last_line_names_the_files_past_the_cut_and_the_worktree() {
    let block: &'static str = "one line of a file that fills the cap\n"
        .repeat(1_200)
        .leak();
    let files: &'static [(&str, &str)] = Box::leak(Box::new([
        ("a.txt", block),
        ("b.txt", block),
        ("c dir/c.txt", "small\n"),
        ("d.txt", "small\n"),
    ]));
    let (rig, _runner, _) = with_open_commit("snorlax", Scripted::CommitFiles(files));
    let diff = std::fs::read_to_string(rig.paths().pm.join("diffs/7.diff")).unwrap();
    let last = diff.trim_end().lines().last().unwrap();
    assert!(
        last.starts_with("[kelpie cut this diff at 64 KiB"),
        "{last}"
    );
    for past in ["b.txt", "c dir/c.txt", "d.txt"] {
        assert!(last.contains(past), "{past} is named: {last}");
    }
    let worktree = rig.paths().worktree(ticket(7)).display().to_string();
    assert!(last.contains(&worktree), "{worktree} is named: {last}");
    assert!(!last.contains("a.txt"), "a.txt was shown whole: {last}");
    assert!(diff.contains("+++ b/a.txt\n"), "a.txt is shown");
}

#[test]
fn one_line_longer_than_the_cap_is_cut_between_characters() {
    let text: &'static str = format!("{}\n", "é".repeat(40_000)).leak();
    let (rig, _runner, _) = with_open_item("ditto", "min.js", text);
    let diff = std::fs::read(rig.paths().pm.join("diffs/7.diff")).unwrap();
    let diff = String::from_utf8(diff).expect("cut on a character's boundary");
    let (kept, cut) = diff.rsplit_once("\n[kelpie cut this diff at ").unwrap();
    assert!(kept.len() > 64 * 1024 - 4, "{} bytes kept", kept.len());
    assert!(kept.ends_with('é'), "part of the long line is kept");
    assert!(cut.starts_with("64 KiB of its "), "{cut}");
}

#[test]
fn a_diff_git_cannot_give_is_left_out_and_named_and_the_wake_runs() {
    let (rig, runner, _) = with_open_item("golbat", "src/lib.rs", "pub fn x() {}\n");
    runner.lock().unwrap().take_notes();
    git(
        &rig.worktree_7(),
        &["update-ref", "-d", "refs/heads/kelpie/7"],
    );
    // Past the board's refresh, so its next write reads the branch again.
    rig.clock.advance(60);
    wake(&rig, &runner);

    let pm = rig.paths().pm;
    assert!(!pm.join("diffs/7.diff").exists(), "no stale diff is left");
    let why = "kelpie/7 is not in the checkout";
    let board = std::fs::read_to_string(pm.join("board.md")).unwrap();
    assert!(board.contains(&format!(", no diff: {why}\n")), "{board}");
    let notes = runner.lock().unwrap().take_notes();
    let note = format!("no diff of #7 for the project manager: {why}");
    assert!(notes.contains(&note), "{notes:?}");
}
