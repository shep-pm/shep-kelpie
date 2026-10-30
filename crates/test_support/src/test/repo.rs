// Folders and git repos a test builds, with the maintainer's own git config kept out

use std::path::{Path, PathBuf};
use std::process::Command;

/// Writes `text` to `file` under `folder`, making the folders between
///
/// # Panics
///
/// When a folder or the file cannot be made.
pub fn write_in(folder: &Path, file: &str, text: &str) {
    let path = folder.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn path(p: &Path) -> &str {
    p.to_str().expect("a UTF-8 temporary path")
}

// When the rig's commits are made, as a test's clock reads it: long before any
// clock a test sets, so a commit counts as recent only where a test dates it so.
const LONG_AGO: &str = "@1700000000 +0000";

/// Runs git in `cwd` and returns its trimmed stdout
///
/// The maintainer's own git config never reaches the rig: no signing, no hooks.
/// A commit it makes is dated long before the rig's clock, which stands in 2026.
///
/// # Panics
///
/// When git cannot run, or exits with a failure.
pub fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=rig",
            "-c",
            "user.email=rig@example.invalid",
        ])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_COMMITTER_DATE", LONG_AGO)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "git {args:?} in {}: {stderr}",
        cwd.display()
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A repo at `home/repo` with one commit on `main`, and a worktree of it at
/// `home/wt` on its own branch, the way kelpie cuts one
///
/// # Panics
///
/// When a folder cannot be made or a git call fails.
pub fn linked_worktree(home: &Path) -> (PathBuf, PathBuf) {
    let repo = home.join("repo");
    let worktree = home.join("wt");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "--quiet", "-b", "main"]);
    git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "init"]);
    git(
        &repo,
        &["worktree", "add", "--quiet", "-b", "wt", path(&worktree)],
    );
    (repo, worktree)
}

/// Gives `main` in `repo` a commit that sets `file` to `contents`, and moves
/// `origin/main` to it
///
/// Before a branch is cut, it is the base the branch starts from. After, it
/// is `main` moving on.
pub fn main_commits(repo: &Path, file: &str, contents: &str) {
    let path = repo.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
    git(repo, &["add", file]);
    git(repo, &["commit", "--quiet", "-m", "main moves on"]);
    let tip = git(repo, &["rev-parse", "main"]);
    git(repo, &["update-ref", "refs/remotes/origin/main", &tip]);
}

/// Moves `worktree`'s branch to where `main` stands, the way a branch is cut
pub fn cut_from_main(worktree: &Path) {
    git(worktree, &["reset", "--quiet", "--hard", "main"]);
}
