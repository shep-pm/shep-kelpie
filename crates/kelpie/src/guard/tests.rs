use std::process::Command as Process;

use serde_json::json;

use super::*;

const HOME: &str = "/home/tester";

fn bash_in(cwd: &Path, command: &str) -> Verdict {
    let call = json!({
        "tool_name": "Bash",
        "cwd": cwd,
        "tool_input": { "command": command },
    });
    judge(call.to_string().as_bytes(), Some(Path::new(HOME)))
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

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "--quiet"]);
    fs::write(dir.path().join("README.md"), "a project\n").unwrap();
    git(dir.path(), &["add", "README.md"]);
    git(dir.path(), &["commit", "--quiet", "-m", "first"]);
    dir
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
}

#[test]
fn a_commit_adding_the_home_folder_is_refused_naming_the_file() {
    let dir = repo();
    fs::write(dir.path().join("notes.md"), "see /home/tester/x\n").unwrap();
    fs::write(dir.path().join("clean.md"), "see ~/x\n").unwrap();
    git(dir.path(), &["add", "notes.md", "clean.md"]);
    let why = refusal(bash_in(dir.path(), "git commit -m 'docs: notes'"));
    assert!(why.contains("`notes.md`"), "{why}");
    assert!(!why.contains("clean.md"), "{why}");
}

#[test]
fn a_commit_removing_the_home_folder_goes_through() {
    let dir = repo();
    fs::write(
        dir.path().join("README.md"),
        "a project\nsee /home/tester/x\n",
    )
    .unwrap();
    git(dir.path(), &["commit", "--quiet", "-am", "first leak"]);
    fs::write(dir.path().join("README.md"), "a project\n").unwrap();
    git(dir.path(), &["add", "README.md"]);
    assert_eq!(
        bash_in(dir.path(), "git commit -m 'fix: drop the path'"),
        Verdict::Allow
    );
}

#[test]
fn a_commit_of_every_tracked_change_reads_the_unstaged_lines_too() {
    let dir = repo();
    fs::write(dir.path().join("README.md"), "see /home/tester/x\n").unwrap();
    assert_eq!(
        bash_in(dir.path(), "git commit -m 'docs: x'"),
        Verdict::Allow,
        "nothing is staged"
    );
    for command in ["git commit -am 'docs: x'", "git commit --all -m 'docs: x'"] {
        let why = refusal(bash_in(dir.path(), command));
        assert!(why.contains("`README.md`"), "{command}: {why}");
    }
}

#[test]
fn a_commit_after_cd_reads_that_folders_repo() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    git(&sub, &["init", "--quiet"]);
    fs::write(sub.join("a.md"), "/home/tester/x\n").unwrap();
    git(&sub, &["add", "a.md"]);
    for command in [
        "cd sub && git commit -m 'docs: a'",
        "git -C sub commit -m 'docs: a'",
    ] {
        let why = refusal(bash_in(dir.path(), command));
        assert!(why.contains("`a.md`"), "{command}: {why}");
    }
}

// Live, a worker wrote a file and committed it in one call, before any was staged.
#[test]
fn a_push_sending_the_home_folder_is_refused_naming_where() {
    let dir = repo();
    let origin = tempfile::tempdir().unwrap();
    git(origin.path(), &["init", "--quiet", "--bare"]);
    let url = origin.path().to_str().unwrap();
    git(dir.path(), &["remote", "add", "origin", url]);
    fs::write(dir.path().join("old.md"), "/home/tester/pushed\n").unwrap();
    git(dir.path(), &["add", "old.md"]);
    git(
        dir.path(),
        &["commit", "--quiet", "-m", "docs: already out"],
    );
    git(
        dir.path(),
        &["push", "--quiet", "origin", "HEAD:refs/heads/x"],
    );
    assert_eq!(bash_in(dir.path(), "git push origin HEAD"), Verdict::Allow);

    fs::write(dir.path().join("notes.md"), "built in /home/tester/wt\n").unwrap();
    git(dir.path(), &["add", "notes.md"]);
    git(dir.path(), &["commit", "--quiet", "-m", "docs: notes"]);
    fs::remove_file(dir.path().join("notes.md")).unwrap();
    git(
        dir.path(),
        &["commit", "--quiet", "-am", "docs: from /home/tester/wt"],
    );
    let why = refusal(bash_in(dir.path(), "git push -u origin HEAD"));
    assert!(
        why.contains("`notes.md` in the commits this push sends"),
        "{why}"
    );
    assert!(why.contains("a message in the commits"), "{why}");
    assert!(why.contains("rewrite"), "{why}");
    assert!(!why.contains("old.md"), "{why}");
    assert!(!why.contains(HOME), "{why}");
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
    assert_eq!(
        judge(call.to_string().as_bytes(), Some(Path::new(HOME))),
        Verdict::Allow
    );
    let call = json!({ "tool_name": "Bash", "cwd": "/x", "tool_input": {
        "command": "git commit -m '/home/tester'",
    } });
    assert_eq!(judge(call.to_string().as_bytes(), None), Verdict::Allow);
    assert_eq!(
        judge(call.to_string().as_bytes(), Some(Path::new("/"))),
        Verdict::Allow
    );
}

#[test]
fn an_unreadable_call_is_refused() {
    assert!(matches!(
        judge(&b"not json"[..], Some(Path::new(HOME))),
        Verdict::Refuse(_)
    ));
}
