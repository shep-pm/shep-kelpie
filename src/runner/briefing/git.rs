//! What the board reads from git: heads, each branch's files, the
//! conflicts between two branches, and the files on `main`
//!
//! A board write takes its questions out of the runner as a [`GitJob`] and
//! runs them with the runner's lock let go. Every answer is kept by the
//! commits it was read at, so a write reads only what moved. Replace refs
//! are ignored, as everywhere kelpie reads a worker's commits.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::board::briefing::{Merge, named_paths};
use crate::worktree::{BASE, in_repo};

/// Git's answers so far, by the commits they were read at
#[derive(Debug, Default)]
pub(in crate::runner) struct Git {
    tree: Option<(String, Vec<String>)>,
    diffs: BTreeMap<(String, String), Vec<String>>,
    merges: BTreeMap<(String, String), Vec<String>>,
}

/// What one board write asks git, with the answers kept so far
#[derive(Debug)]
pub(in crate::runner) struct GitJob {
    pub(super) repo: PathBuf,
    pub(super) git: Git,
    /// Each open work item's issue and branch
    pub(super) branches: Vec<(u64, String)>,
    /// The bodies of the issues ready or open, whose named paths it reads
    pub(super) bodies: Vec<(u64, String)>,
}

/// What git answered for one board write
#[derive(Debug, Default)]
pub(in crate::runner) struct GitAnswers {
    /// The answers kept, for the next write
    pub(super) git: Git,
    /// `origin/main`'s commit
    pub(super) main: Option<String>,
    /// The files each work item's branch changes, where git could say
    pub(super) files: BTreeMap<u64, Vec<String>>,
    /// Each pair of work items whose branches do not merge cleanly
    pub(super) merges: BTreeMap<(u64, u64), Merge>,
    /// The paths each issue's body names, as files on `main`
    pub(super) named: BTreeMap<u64, Vec<String>>,
    /// The bodies those paths were read from
    pub(super) bodies: Vec<(u64, String)>,
}

impl GitJob {
    /// Asks git everything the board shows
    pub(super) fn run(self) -> GitAnswers {
        let Self {
            repo,
            mut git,
            branches,
            bodies,
        } = self;
        let main = Git::main(&repo);
        let heads: Vec<(u64, String)> = branches
            .iter()
            .filter_map(|(issue, branch)| Some((*issue, Git::head(&repo, branch)?)))
            .collect();
        let mut files = BTreeMap::new();
        if let Some(main) = &main {
            for (issue, head) in &heads {
                if let Some(found) = git.files(&repo, main, head) {
                    files.insert(*issue, found);
                }
            }
        }
        // A branch still at `main` has nothing to merge.
        let moved: Vec<&(u64, String)> = heads
            .iter()
            .filter(|(_, head)| Some(head) != main.as_ref())
            .collect();
        let mut merges = BTreeMap::new();
        for (i, (a, head_a)) in moved.iter().map(|p| (p.0, &p.1)).enumerate() {
            for (b, head_b) in moved[i + 1..].iter().map(|p| (p.0, &p.1)) {
                let merge = match git.conflicts(&repo, head_a, head_b) {
                    Some(found) if found.is_empty() => continue,
                    Some(found) => Merge::Conflicts(found),
                    None => Merge::Unknown,
                };
                merges.insert((a.min(b), a.max(b)), merge);
            }
        }
        let tree = main.as_deref().map(|m| git.tree(&repo, m).to_vec());
        let named = (bodies.iter())
            .map(|(issue, body)| {
                (
                    *issue,
                    named_paths(body, tree.as_deref().unwrap_or_default()),
                )
            })
            .collect();
        let live: BTreeSet<String> = heads.into_iter().map(|(_, head)| head).collect();
        git.keep(main.as_deref(), &live);
        GitAnswers {
            git,
            main,
            files,
            merges,
            named,
            bodies,
        }
    }
}

impl Git {
    /// `origin/main`'s commit, as last fetched
    fn main(repo: &Path) -> Option<String> {
        let tracking = format!("refs/remotes/origin/{BASE}");
        read(repo, &["rev-parse", "--verify", "--quiet", &tracking]).map(trimmed)
    }

    /// The commit local `branch` is at, which the worker's worktree has out
    fn head(repo: &Path, branch: &str) -> Option<String> {
        let full = format!("refs/heads/{branch}");
        read(repo, &["rev-parse", "--verify", "--quiet", &full]).map(trimmed)
    }

    /// Every file on `main`
    fn tree(&mut self, repo: &Path, main: &str) -> &[String] {
        if self.tree.as_ref().is_none_or(|(at, _)| at != main) {
            let files = read(repo, &["ls-tree", "-r", "--name-only", "-z", main]);
            self.tree = Some((main.to_owned(), files.map(names).unwrap_or_default()));
        }
        self.tree.as_ref().map_or(&[], |(_, files)| files)
    }

    /// The files `head` changes since it left `main`, `None` when git cannot say
    fn files(&mut self, repo: &Path, main: &str, head: &str) -> Option<Vec<String>> {
        let key = (main.to_owned(), head.to_owned());
        if let Some(files) = self.diffs.get(&key) {
            return Some(files.clone());
        }
        let range = format!("{main}...{head}");
        let args = ["diff", "--name-only", "--no-renames", "-z", &range];
        let files = names(read(repo, &args)?);
        self.diffs.insert(key, files.clone());
        Some(files)
    }

    /// The files in conflict merging heads `a` and `b`, empty when they
    /// merge cleanly, and `None` when git cannot say
    pub(super) fn conflicts(&mut self, repo: &Path, a: &str, b: &str) -> Option<Vec<String>> {
        let key = (a.min(b).to_owned(), a.max(b).to_owned());
        if let Some(files) = self.merges.get(&key) {
            return Some(files.clone());
        }
        let args = [
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            "-z",
            &key.0,
            &key.1,
        ];
        // Exit 1 with a tree first is a merge with conflicts. A missing commit
        // also exits 1, with nothing on stdout, and git before 2.38 fails
        // otherwise: neither is kept, so the next write asks again.
        let files = match run(repo, &args)? {
            (0, _) => Vec::new(),
            (1, out) => {
                let mut found = names(out).into_iter();
                let tree = found.next()?;
                if !tree.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return None;
                }
                found.collect()
            }
            _ => return None,
        };
        self.merges.insert(key, files.clone());
        Some(files)
    }

    /// Forgets every answer about a commit not in `heads` or not on `main`
    fn keep(&mut self, main: Option<&str>, heads: &BTreeSet<String>) {
        let kept = |h: &String| heads.contains(h);
        self.diffs
            .retain(|(at, head), _| Some(at.as_str()) == main && kept(head));
        self.merges.retain(|(a, b), _| kept(a) && kept(b));
    }
}

fn read(repo: &Path, args: &[&str]) -> Option<String> {
    match run(repo, args)? {
        (0, out) => Some(out),
        _ => None,
    }
}

fn run(repo: &Path, args: &[&str]) -> Option<(i32, String)> {
    let output = in_repo(repo)
        .arg("--no-replace-objects")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let code = output.status.code()?;
    Some((code, String::from_utf8_lossy(&output.stdout).into_owned()))
}

fn trimmed(text: String) -> String {
    text.trim().to_owned()
}

// The names in git's `-z` output, each once, in order
fn names(out: String) -> Vec<String> {
    let mut seen = BTreeSet::new();
    out.split('\0')
        .filter(|name| !name.is_empty() && seen.insert(name.to_owned()))
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::git;

    #[test]
    fn a_merge_git_cannot_run_is_unknown_and_asked_again() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "--quiet", "--initial-branch=main"]);
        let mut cache = Git::default();
        let missing = "0123456789abcdef0123456789abcdef01234567";
        let other = "89abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(cache.conflicts(dir.path(), missing, other), None);
        assert!(cache.merges.is_empty(), "an unknown merge is not kept");
    }
}
