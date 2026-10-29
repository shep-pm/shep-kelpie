//! A copy of a test repo's git dir that marks when git runs a program for it

use std::path::{Path, PathBuf};
use std::process::Command;

use super::{git, write_script};

/// A copy of a repo's git dir that makes [`Self::ran`] when git runs a
/// program for it
#[derive(Debug)]
pub struct Elsewhere {
    git_dir: PathBuf,
    /// The file the program makes
    pub ran: PathBuf,
}

impl Elsewhere {
    /// Copies `repo`'s git dir into `home`
    pub fn copy_of(repo: &Path, home: &Path) -> Self {
        let git_dir = home.join("elsewhere.git");
        copy_dir(&repo.join(".git"), &git_dir);
        let ran = home.join("ran");
        let program = home.join("fsmonitor.sh");
        write_script(&program, &format!("#!/bin/sh\ntouch {}\n", quoted(&ran)));
        let config = git_dir.join("config");
        // Git runs the value through the shell.
        let value = quoted(&program);
        let set = [
            "config",
            "--file",
            config.to_str().unwrap(),
            "core.fsmonitor",
            &value,
        ];
        git(home, &set);
        Self { git_dir, ran }
    }

    /// Points `worktree`'s `.git` file here
    pub fn as_git_dir_of(&self, worktree: &Path) {
        let link = format!("gitdir: {}\n", self.git_dir.display());
        std::fs::write(worktree.join(".git"), link).unwrap();
    }

    /// Points the `commondir` of `worktree`'s own git dir here
    pub fn as_common_dir_of(&self, worktree: &Path) {
        let link = std::fs::read_to_string(worktree.join(".git")).unwrap();
        let own = PathBuf::from(link.trim().strip_prefix("gitdir: ").unwrap());
        let common = format!("{}\n", self.git_dir.display());
        std::fs::write(own.join("commondir"), common).unwrap();
    }

    /// Points the objects the clone at `git_dir` borrows here
    pub fn as_alternate_of(&self, git_dir: &Path) {
        let objects = format!("{}\n", self.git_dir.join("objects").display());
        std::fs::write(git_dir.join("objects/info/alternates"), objects).unwrap();
    }

    /// Asserts plain git on `worktree` runs the program
    pub fn assert_plain_git_starts_it(&self, worktree: &Path) {
        assert!(!self.ran.exists(), "the program ran before plain git");
        let plain = Command::new("git")
            .arg("-C")
            .arg(worktree)
            .args(["diff", "HEAD"])
            .output()
            .unwrap();
        assert!(plain.status.success(), "{plain:?}");
        assert!(self.ran.exists(), "plain git on the worktree starts it");
    }
}

// `from` and everything in it, copied to `to`. A git dir holds only files and
// folders. An entry that is gone by the time it is copied, as a file git
// removes after the listing or a link to nothing, is left out: it is nothing
// the copy needs, and any other failure names its path.
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        let copied = match entry.file_type() {
            Ok(kind) if kind.is_dir() => {
                copy_dir(&entry.path(), &target);
                continue;
            }
            Ok(_) => std::fs::copy(entry.path(), &target).map(drop),
            Err(e) => Err(e),
        };
        match copied {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => panic!("cannot copy {}: {e}", entry.path().display()),
            Ok(()) => {}
        }
    }
}

// `path` as one shell word.
fn quoted(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', r"'\''"))
}
