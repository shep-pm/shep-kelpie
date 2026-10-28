use std::path::PathBuf;

use super::{Remote, Start, prepare, trust_git_of, trusted};
use shep_kelpie_test_support::git;

const SETTINGS: &[u8] = b"{\n  \"permissions\": {}\n}\n";

// A clone whose `main` holds a file with LF endings and whose own config asks
// for the CRLF checkout Git for Windows asks for in its system config
struct Rig {
    _dir: tempfile::TempDir,
    repo: PathBuf,
    worktree: PathBuf,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        git(
            &root,
            &["init", "--quiet", "--bare", "-b", "main", "origin.git"],
        );
        git(&root, &["clone", "--quiet", "origin.git", "repo"]);
        let repo = root.join("repo");
        std::fs::create_dir(repo.join(".claude")).unwrap();
        std::fs::write(repo.join(".claude/settings.json"), SETTINGS).unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "--quiet", "-m", "init"]);
        git(&repo, &["push", "--quiet", "origin", "main"]);
        git(&repo, &["config", "core.autocrlf", "true"]);
        let worktree = root.join("worktrees/7");
        Self {
            _dir: dir,
            repo,
            worktree,
        }
    }

    fn prepared(&self) {
        prepare(
            &self.repo,
            &self.worktree,
            "kelpie/7",
            Start::Main,
            "main",
            &Remote::Origin,
            &self.worktree.with_file_name("build"),
        )
        .unwrap();
    }
}

#[test]
fn a_worktree_checks_a_file_out_as_committed_under_core_autocrlf() {
    let rig = Rig::new();
    rig.prepared();

    let checked_out = std::fs::read(rig.worktree.join(".claude/settings.json")).unwrap();
    assert_eq!(checked_out, SETTINGS);
}

#[test]
fn trusted_git_runs_with_core_autocrlf_off() {
    let rig = Rig::new();
    rig.prepared();

    let value = trusted(&rig.repo, &rig.worktree).unwrap()(&["config", "core.autocrlf"]).unwrap();
    assert_eq!(value, "false");
}

#[test]
fn a_program_given_the_trusted_git_runs_with_core_autocrlf_off() {
    let rig = Rig::new();
    rig.prepared();

    let mut program = std::process::Command::new("git");
    program.args(["config", "core.autocrlf"]);
    trust_git_of(&mut program, &rig.repo, &rig.worktree).unwrap();
    let out = program.output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "false");
}
