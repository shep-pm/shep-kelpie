use std::path::PathBuf;
use std::process::Command as Process;

use serde_json::json;
use tempfile::TempDir;

use super::*;

const HOME: &str = "/home/tester";

fn call(cwd: &Path, command: &str, checkout: Checkout<'_>) -> Verdict {
    let call = json!({
        "tool_name": "Bash",
        "cwd": cwd,
        "tool_input": { "command": command },
    });
    judge(call.to_string().as_bytes(), Some(Path::new(HOME)), checkout)
}

// For what the command's own text carries: no git is read.
fn nowhere() -> Checkout<'static> {
    Checkout {
        git_common_dir: Path::new("/nowhere"),
        worktree: Path::new("/nowhere"),
    }
}

fn bash_in(cwd: &Path, command: &str) -> Verdict {
    call(cwd, command, nowhere())
}

fn bash(command: &str) -> Verdict {
    bash_in(Path::new("/nowhere"), command)
}

fn refusal(verdict: Verdict) -> String {
    match verdict {
        Verdict::Refuse(why) => why,
        Verdict::Allow => panic!("the call was let through"),
    }
}

fn git(dir: &Path, args: &[&str]) {
    let status = Process::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

// A project repo and a worker's worktree on its own branch, as kelpie cuts one.
struct WorkerTree(TempDir);

impl WorkerTree {
    fn new() -> Self {
        let tree = Self(tempfile::tempdir().unwrap());
        let repo = tree.repo();
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "--quiet"]);
        fs::write(repo.join("README.md"), "a project\n").unwrap();
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "--quiet", "-m", "first"]);
        git(
            &repo,
            &["worktree", "add", "--quiet", "-b", "kelpie/7", "../wt"],
        );
        tree
    }

    fn repo(&self) -> PathBuf {
        self.0.path().join("repo")
    }

    fn path(&self) -> PathBuf {
        self.0.path().join("wt")
    }

    fn write(&self, file: &str, text: &str) {
        let path = self.path().join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn git(&self, args: &[&str]) {
        git(&self.path(), args);
    }

    fn bash_in(&self, cwd: &Path, command: &str) -> Verdict {
        let common = self.repo().join(".git");
        let worktree = self.path();
        let checkout = Checkout {
            git_common_dir: &common,
            worktree: &worktree,
        };
        call(cwd, command, checkout)
    }

    fn bash(&self, command: &str) -> Verdict {
        self.bash_in(&self.path(), command)
    }
}

#[test]
fn a_conventional_pull_request_goes_through() {
    for command in [
        "gh pr create --draft --title 'fix(parser): keep the last line' --body 'Resolves #7'",
        "gh pr create -t 'feat!: drop the old flag' -b x",
        "gh pr create --title=\"docs: say how\" --body-file -",
        "gh pr edit 12 --title 'refactor(core)!: split the loop'",
        "gh pr edit 12 --body 'only the body'",
        "gh pr view 12",
        "git push origin HEAD",
    ] {
        assert_eq!(bash(command), Verdict::Allow, "{command}");
    }
}

#[test]
fn a_pull_request_title_that_is_not_a_conventional_commit_is_refused() {
    for title in [
        "Fix the parser",
        "fix:no space",
        "fix: ",
        "feature: x",
        "fix(): x",
        "fix(a(b)): x",
        "kelpie-108",
    ] {
        let why = refusal(bash(&format!("gh pr create --title '{title}' --body x")));
        assert!(why.contains("not a conventional commit"), "{title}: {why}");
        let why = refusal(bash(&format!("gh pr edit 3 -t '{title}'")));
        assert!(why.contains("not a conventional commit"), "{title}: {why}");
    }
}

#[test]
fn a_pull_request_with_no_title_is_refused() {
    for command in [
        "gh pr create --draft --fill",
        "git push origin HEAD && gh pr create --draft --body 'Resolves #7'",
        "cd sub; if gh pr create -d; then echo; fi",
    ] {
        let why = refusal(bash(command));
        assert!(why.contains("needs `--title`"), "{command}: {why}");
    }
}

#[test]
fn a_command_that_only_mentions_gh_pr_create_is_not_judged() {
    for command in [
        "grep -n 'gh pr create' docs/notes.md",
        "echo \"then run gh pr create\"",
        "git commit -m 'docs: say to run gh pr create'",
    ] {
        assert_eq!(bash(command), Verdict::Allow, "{command}");
    }
}

#[test]
fn a_message_naming_the_home_folder_is_refused_without_echoing_it() {
    for command in [
        "git commit -m 'fix: read /home/tester/.kelpie/wt/koji/7/src/a.rs'",
        "git commit -am 'fix: see /home/tester'",
        "git -C sub commit --message=\"fix: /HOME/TESTER/x\"",
        "git tag -a v1 -m 'from /home/tester/x'",
        "git commit -F - <<'EOF'\nfix: x\n\nBuilt in /home/tester/.kelpie/wt/koji/7.\nEOF",
        "gh pr create --title 'fix: x' --body 'ran in /home/tester/w'",
        "gh pr create --title 'fix: x' --body \"$(cat <<'EOF'\nran in /home/tester/w\nEOF\n)\"",
        "gh pr comment 3 -b '/home/tester/x'",
        "gh issue create --title 'fix: x' --body '/home/tester/x'",
        "gh release create v1 --notes 'from /home/tester'",
    ] {
        let why = refusal(bash(command));
        assert!(
            why.contains("home folder's absolute path"),
            "{command}: {why}"
        );
        assert!(!why.to_lowercase().contains(HOME), "{command}: {why}");
    }
}

#[test]
fn the_home_folder_in_a_command_but_not_its_message_goes_through() {
    for command in [
        "cd /home/tester/.kelpie/wt/koji/7 && git commit -m 'fix: x'",
        "git -C /home/tester/.kelpie/wt/koji/7 status",
        "gh pr create --title 'fix: x' --body '~/notes and /home/testers/x and /home/tester-2'",
        "cat /home/tester/.kelpie/wt/koji/7/src/a.rs",
    ] {
        assert_eq!(bash(command), Verdict::Allow, "{command}");
    }
}

#[test]
fn a_body_file_naming_the_home_folder_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("body.md"), "ran in /home/tester/x\n").unwrap();
    fs::write(dir.path().join("clean.md"), "Resolves #7\n").unwrap();
    let why = refusal(bash_in(
        dir.path(),
        "gh pr create -t 'fix: x' --body-file body.md",
    ));
    assert!(why.contains("`gh pr create`"), "{why}");
    let clean = "gh pr create -t 'fix: x' --body-file clean.md";
    assert_eq!(bash_in(dir.path(), clean), Verdict::Allow);
    let folder = "gh pr create -t 'fix: x' --body-file .";
    assert_eq!(bash_in(dir.path(), folder), Verdict::Allow);
}

#[test]
fn a_commit_adding_the_home_folder_is_refused_naming_the_file() {
    let tree = WorkerTree::new();
    tree.write("notes.md", "see /home/tester/x\n");
    tree.write("clean.md", "see ~/x\n");
    tree.git(&["add", "notes.md", "clean.md"]);
    let why = refusal(tree.bash("git commit -m 'docs: notes'"));
    assert!(why.contains("`notes.md`"), "{why}");
    assert!(!why.contains("clean.md"), "{why}");
}

#[test]
fn a_commit_removing_the_home_folder_goes_through() {
    let tree = WorkerTree::new();
    tree.write("README.md", "a project\nsee /home/tester/x\n");
    tree.git(&["commit", "--quiet", "-am", "first leak"]);
    tree.write("README.md", "a project\n");
    tree.git(&["add", "README.md"]);
    assert_eq!(
        tree.bash("git commit -m 'fix: drop the path'"),
        Verdict::Allow
    );
}

#[test]
fn a_commit_of_every_tracked_change_reads_the_unstaged_lines_too() {
    let tree = WorkerTree::new();
    tree.write("README.md", "see /home/tester/x\n");
    assert_eq!(
        tree.bash("git commit -m 'docs: x'"),
        Verdict::Allow,
        "nothing is staged"
    );
    for command in ["git commit -am 'docs: x'", "git commit --all -m 'docs: x'"] {
        let why = refusal(tree.bash(command));
        assert!(why.contains("`README.md`"), "{command}: {why}");
    }
}

#[test]
fn a_commit_from_a_folder_in_the_worktree_reads_its_repo() {
    let tree = WorkerTree::new();
    tree.write("sub/a.md", "/home/tester/x\n");
    tree.git(&["add", "sub/a.md"]);
    for command in [
        "cd sub && git commit -m 'docs: a'",
        "git -C sub commit -m 'docs: a'",
    ] {
        let why = refusal(tree.bash(command));
        assert!(why.contains("`sub/a.md`"), "{command}: {why}");
    }
}

// The hook runs outside the sandbox: git run in a repo the worker made
// would start the program its config names.
#[test]
fn a_repo_the_worker_made_is_never_read() {
    let tree = WorkerTree::new();
    let evil = tree.path().join("evil");
    let ran = tree.0.path().join("ran");
    let program = tree.0.path().join("fsmonitor.sh");
    fs::write(&program, format!("#!/bin/sh\ntouch '{}'\n", ran.display())).unwrap();
    let mut mode = fs::metadata(&program).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
    fs::set_permissions(&program, mode).unwrap();
    fs::create_dir(&evil).unwrap();
    git(&evil, &["init", "--quiet"]);
    fs::write(evil.join("a.md"), "a\n").unwrap();
    git(&evil, &["add", "a.md"]);
    git(&evil, &["commit", "--quiet", "-m", "a"]);
    git(
        &evil,
        &["config", "core.fsmonitor", program.to_str().unwrap()],
    );
    fs::write(evil.join("a.md"), "/home/tester/x\n").unwrap();

    for command in [
        "git -C evil commit -am 'docs: a'",
        "cd evil && git commit -am 'docs: a' && git push",
    ] {
        assert_eq!(tree.bash(command), Verdict::Allow, "{command}");
    }
    assert!(!ran.exists(), "the guard ran the repo's program");
    let plain = Process::new("git")
        .arg("status")
        .current_dir(&evil)
        .output();
    assert!(plain.is_ok() && ran.exists(), "git run there starts it");
}

// Live, a worker wrote a file and committed it in one call, before any was staged.
#[test]
fn a_push_sending_the_home_folder_is_refused_naming_where() {
    let tree = WorkerTree::new();
    let origin = tempfile::tempdir().unwrap();
    git(origin.path(), &["init", "--quiet", "--bare"]);
    let url = origin.path().to_str().unwrap();
    tree.git(&["remote", "add", "origin", url]);
    tree.write("old.md", "/home/tester/pushed\n");
    tree.git(&["add", "old.md"]);
    tree.git(&["commit", "--quiet", "-m", "docs: already out"]);
    tree.git(&["push", "--quiet", "origin", "HEAD:refs/heads/x"]);
    assert_eq!(tree.bash("git push origin HEAD"), Verdict::Allow);

    tree.write("notes.md", "built in /home/tester/wt\n");
    tree.git(&["add", "notes.md"]);
    tree.git(&["commit", "--quiet", "-m", "docs: notes"]);
    fs::remove_file(tree.path().join("notes.md")).unwrap();
    tree.git(&["commit", "--quiet", "-am", "docs: drop the notes"]);
    let why = refusal(tree.bash("git push -u origin HEAD"));
    assert!(
        why.contains("`notes.md` in the commits this push sends"),
        "{why}"
    );
    assert!(why.contains("rewrite"), "{why}");
    assert!(!why.contains("a message"), "no message names it: {why}");
    assert!(!why.contains("old.md"), "{why}");
    assert!(!why.contains(HOME), "{why}");
}

#[test]
fn a_push_sending_a_message_naming_the_home_folder_is_refused() {
    let tree = WorkerTree::new();
    tree.write("notes.md", "built in ~/wt\n");
    tree.git(&["add", "notes.md"]);
    tree.git(&["commit", "--quiet", "-m", "docs: built in /home/tester/wt"]);
    let why = refusal(tree.bash("git push origin HEAD"));
    assert!(why.contains("a message in the commits"), "{why}");
    assert!(!why.contains("notes.md"), "the file is clean: {why}");
}

// The worker can rewrite its worktree's `.git` file and its own git dir.
#[test]
fn a_worktree_whose_git_was_repointed_is_refused_not_read() {
    let tree = WorkerTree::new();
    let own = fs::canonicalize(tree.repo().join(".git/worktrees/wt")).unwrap();
    let elsewhere = tree.0.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::write(own.join("commondir"), format!("{}\n", elsewhere.display())).unwrap();
    let why = refusal(tree.bash("git push origin HEAD"));
    assert!(why.contains("cannot read this worktree's git"), "{why}");
}

#[test]
fn every_problem_in_one_call_is_named_once() {
    let why = refusal(bash(
        "gh pr create --title 'Parser' --body /home/tester && gh pr create --title 'Parser'",
    ));
    assert_eq!(why.matches("not a conventional commit").count(), 1, "{why}");
    assert_eq!(
        why.matches("home folder's absolute path").count(),
        1,
        "{why}"
    );
}

#[test]
fn other_tools_and_a_missing_home_are_let_through() {
    let call =
        json!({ "tool_name": "Write", "cwd": "/x", "tool_input": { "command": "gh pr create" } });
    let judged = |call: &serde_json::Value, home: Option<&str>| {
        judge(call.to_string().as_bytes(), home.map(Path::new), nowhere())
    };
    assert_eq!(judged(&call, Some(HOME)), Verdict::Allow);
    let call = json!({ "tool_name": "Bash", "cwd": "/x", "tool_input": {
        "command": "git commit -m '/home/tester'",
    } });
    assert_eq!(judged(&call, None), Verdict::Allow);
    assert_eq!(judged(&call, Some("/")), Verdict::Allow);
}

#[test]
fn an_unreadable_call_is_refused() {
    assert!(matches!(
        judge(&b"not json"[..], Some(Path::new(HOME)), nowhere()),
        Verdict::Refuse(_)
    ));
}
