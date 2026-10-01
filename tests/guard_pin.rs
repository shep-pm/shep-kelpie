//! `kelpie guard --pin-folder`, as a Codex worker's hook runs it, against
//! the real binary: the command it answers with runs where Codex runs it,
//! once that folder has been judged.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::json;

const KELPIE: &str = env!("CARGO_BIN_EXE_shep-kelpie");

/// A worktree whose name needs quoting, with a script at its root that
/// guard allows and one in `sub` that it refuses
struct Tree {
    _dir: tempfile::TempDir,
    root: PathBuf,
    worktree: PathBuf,
}

impl Tree {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let worktree = root.join("it's wt");
        std::fs::create_dir_all(worktree.join("sub")).unwrap();
        std::fs::write(worktree.join("x.sh"), "echo root\n").unwrap();
        std::fs::write(
            worktree.join("sub/x.sh"),
            "echo ran\ngh pr create --title Parser\n",
        )
        .unwrap();
        Self {
            _dir: dir,
            root,
            worktree,
        }
    }

    // The command the hook answers `command` with, judged at the worktree's root.
    fn pinned(&self, command: &str) -> String {
        let mut hook = Command::new(KELPIE)
            .arg("guard")
            .arg(self.root.join("repo.git"))
            .arg(&self.worktree)
            .arg("--pin-folder")
            .env("HOME", self.root.join("home"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let call = json!({
            "tool_name": "Bash",
            "cwd": self.worktree,
            "tool_input": { "command": command },
        });
        hook.stdin
            .take()
            .unwrap()
            .write_all(call.to_string().as_bytes())
            .unwrap();
        let out = hook.wait_with_output().unwrap();
        assert!(out.status.success(), "{out:?}");
        let answer: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let out = &answer["hookSpecificOutput"];
        assert_eq!(out["permissionDecision"], "allow");
        out["updatedInput"]["command"].as_str().unwrap().to_owned()
    }

    // Runs `command` as Codex runs a command, in `workdir`, under `shell`.
    fn run(&self, shell: &str, command: &str, workdir: &Path) -> Output {
        Command::new(shell)
            .args(["-c", command])
            .current_dir(workdir)
            .env("HOME", self.root.join("home"))
            .output()
            .unwrap()
    }
}

// The shells Codex may run a command with that this machine has: `sh`
// always, and `bash` and `zsh` where they are installed.
fn shells() -> Vec<&'static str> {
    let shells: Vec<_> = ["sh", "bash", "zsh"]
        .into_iter()
        .filter(|shell| {
            Command::new(shell)
                .args(["-c", "true"])
                .status()
                .is_ok_and(|s| s.success())
        })
        .collect();
    assert!(shells.contains(&"sh"), "{shells:?}");
    shells
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_command_runs_in_the_folder_it_was_judged_in() {
    let tree = Tree::new();
    let command = tree.pinned("sh x.sh");
    for shell in shells() {
        let ran = tree.run(shell, &command, &tree.worktree);
        assert!(ran.status.success(), "{shell}: {ran:?}");
        assert_eq!(stdout(&ran), "root\n", "{shell}");
    }
}

#[test]
fn a_workdir_inside_the_worktree_is_kept_and_judged_there() {
    let tree = Tree::new();
    let sub = tree.worktree.join("sub");
    let (pwd, script) = (tree.pinned("pwd -P"), tree.pinned("sh x.sh"));
    for shell in shells() {
        let ran = tree.run(shell, &pwd, &sub);
        assert!(ran.status.success(), "{shell}: {ran:?}");
        assert_eq!(stdout(&ran).trim_end(), sub.to_str().unwrap(), "{shell}");

        // The root's `x.sh` passed the hook; `sub`'s is judged before it runs.
        let ran = tree.run(shell, &script, &sub);
        assert_eq!(ran.status.code(), Some(1), "{shell}: {ran:?}");
        assert_eq!(stdout(&ran), "", "{shell}");
        assert!(stderr(&ran).contains("title"), "{shell}: {}", stderr(&ran));
    }
}

#[test]
fn a_workdir_outside_the_worktree_is_refused_with_what_to_do() {
    let tree = Tree::new();
    let command = tree.pinned("pwd -P");
    for shell in shells() {
        let ran = tree.run(shell, &command, &tree.root);
        assert_eq!(ran.status.code(), Some(1), "{shell}: {ran:?}");
        assert_eq!(stdout(&ran), "", "{shell}");
        let why = stderr(&ran);
        assert!(why.contains("only inside the worktree"), "{shell}: {why}");
        assert!(why.contains("leave `workdir` out"), "{shell}: {why}");
    }
}
