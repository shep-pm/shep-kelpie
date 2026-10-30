//! What the board reads from git: heads, each branch's files, the
//! conflicts of each branch with `main` and between two, less the files
//! `main` clashes in, the files on `main`, and for a project manager each
//! branch's diff
//!
//! A board write takes its questions out of the runner as a [`GitJob`] and
//! runs them with the runner's lock let go. Every answer is kept by the
//! commits it was read at, so a write reads only what moved. Replace refs
//! are ignored, as everywhere kelpie reads a worker's commits.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::patch;
use crate::board::briefing::{Merge, named_paths};
use crate::ticket::TicketId;
use crate::worktree::{clone, in_repo, loose_commit_in};

/// Git's answers so far, by the commits they were read at
#[derive(Debug, Default)]
pub(in crate::runner) struct Git {
    tree: Option<(String, Vec<String>)>,
    diffs: BTreeMap<(String, String), Vec<String>>,
    merges: BTreeMap<(String, String), Vec<String>>,
    patches: BTreeMap<(String, String, PathBuf), Vec<u8>>,
}

/// The checkout, and the commits of the work items' clones it reads
///
/// A worker commits in its clone, whose objects the checkout does not hold,
/// so a git run in the checkout is told of each clone's object folder as an
/// alternate. A branch is read in its clone, which holds it.
#[derive(Debug, Clone)]
pub(in crate::runner) struct Reading {
    path: PathBuf,
    clones: Option<OsString>,
}

impl Reading {
    /// The checkout at `path`, reading no clone
    pub(in crate::runner) fn of(path: &Path) -> Self {
        Self {
            path: path.to_owned(),
            clones: None,
        }
    }

    /// The checkout at `path`, reading each of `worktrees` that is a clone of it
    pub(in crate::runner) fn of_clones<'a>(
        path: &Path,
        worktrees: impl IntoIterator<Item = &'a Path>,
    ) -> Self {
        let objects = (worktrees.into_iter())
            .filter_map(|worktree| clone::objects_of(path, worktree))
            .collect::<Vec<_>>();
        // A folder whose name holds the separator cannot be listed: the
        // commits in it read as missing, as git's own would.
        let clones = std::env::join_paths(objects)
            .ok()
            .filter(|joined| !joined.is_empty());
        Self {
            path: path.to_owned(),
            clones,
        }
    }

    /// The commit `branch` is at in `worktree`'s clone, in the checkout where it is none
    pub(super) fn head(&self, worktree: &Path, branch: &str) -> Option<String> {
        clone::branch_head(&self.path, worktree, branch)
    }

    /// `git` in the checkout
    pub(super) fn git(&self) -> Command {
        let mut git = in_repo(&self.path);
        if let Some(clones) = &self.clones {
            git.env("GIT_ALTERNATE_OBJECT_DIRECTORIES", clones);
        }
        git
    }
}

/// What one board write asks git, with the answers kept so far
#[derive(Debug)]
pub(in crate::runner) struct GitJob {
    pub(super) repo: Reading,
    /// The project's base branch
    pub(super) base: String,
    pub(super) git: Git,
    /// Each open work item's ticket and branch
    pub(super) branches: Vec<(TicketId, String)>,
    /// The bodies of the issues ready or open, whose named paths it reads
    pub(super) bodies: Vec<(TicketId, String)>,
    /// Where each open work item's worktree is
    pub(super) worktrees: BTreeMap<TicketId, PathBuf>,
    /// Whether it reads each branch's diff, for a project manager
    pub(super) patches: bool,
}

/// What git answered for one board write
#[derive(Debug, Default)]
pub(in crate::runner) struct GitAnswers {
    /// The answers kept, for the next write
    pub(super) git: Git,
    /// `origin/main`'s commit
    pub(super) main: Option<String>,
    /// The files each work item's branch changes, where git could say
    pub(super) files: BTreeMap<TicketId, Vec<String>>,
    /// The files each work item's branch conflicts with `main` in
    pub(super) behind: BTreeMap<TicketId, Vec<String>>,
    /// Each pair of work items, both clean with `main`, whose branches do not
    /// merge cleanly
    pub(super) merges: BTreeMap<(TicketId, TicketId), Merge>,
    /// The paths each issue's body names, as files on `main`
    pub(super) named: BTreeMap<TicketId, Vec<String>>,
    /// The bodies those paths were read from
    pub(super) bodies: Vec<(TicketId, String)>,
    /// Each work item's diff from `main`, cut to [`patch::CAP`], or why
    /// git could not give it, when asked
    pub(super) patches: BTreeMap<TicketId, Result<Vec<u8>, String>>,
}

impl GitJob {
    /// Asks git everything the board shows
    pub(super) fn run(self) -> GitAnswers {
        let Self {
            repo,
            base,
            mut git,
            branches,
            bodies,
            worktrees,
            patches,
        } = self;
        let main_ref = format!("refs/remotes/origin/{base}");
        let main = loose_commit_in(&repo.path, &main_ref)
            .or_else(|| commits(&repo, [&main_ref]).remove(&main_ref));
        let heads: Vec<(TicketId, String)> = (branches.iter())
            .filter_map(|(ticket, branch)| {
                let worktree = worktrees
                    .get(ticket)
                    .map_or(Path::new(""), PathBuf::as_path);
                Some((*ticket, repo.head(worktree, branch)?))
            })
            .collect();
        let mut files = BTreeMap::new();
        if let Some(main) = &main {
            for (ticket, head) in &heads {
                if let Some(found) = git.files(&repo, main, head) {
                    files.insert(*ticket, found);
                }
            }
        }
        // A branch still at `main` has nothing to merge. One that conflicts
        // with `main` is behind it, and the files it conflicts in are `main`'s
        // clash: they are left out of its pairs, since it clashes there with
        // any branch that holds `main`'s change. Its other clashes stay.
        let moved: Vec<&(TicketId, String)> = heads
            .iter()
            .filter(|(_, head)| Some(head) != main.as_ref())
            .collect();
        let mut behind = BTreeMap::new();
        for (ticket, head) in &moved {
            let found = main.as_deref().and_then(|m| git.conflicts(&repo, m, head));
            if let Some(found) = found.filter(|found| !found.is_empty()) {
                behind.insert(*ticket, found);
            }
        }
        let mains_clash = |ticket: &TicketId| behind.get(ticket).map_or(&[][..], Vec::as_slice);
        let mut merges = BTreeMap::new();
        for (i, (a, head_a)) in moved.iter().map(|p| (p.0, &p.1)).enumerate() {
            for (b, head_b) in moved[i + 1..].iter().map(|p| (p.0, &p.1)) {
                let merge = match git.conflicts(&repo, head_a, head_b) {
                    Some(mut found) => {
                        found.retain(|file| ![a, b].iter().any(|t| mains_clash(t).contains(file)));
                        if found.is_empty() {
                            continue;
                        }
                        Merge::Conflicts(found)
                    }
                    None => Merge::Unknown,
                };
                merges.insert((a.min(b), a.max(b)), merge);
            }
        }
        let tree = main.as_deref().map(|m| git.tree(&repo, m).to_vec());
        let named = (bodies.iter())
            .map(|(ticket, body)| {
                (
                    *ticket,
                    named_paths(body, tree.as_deref().unwrap_or_default()),
                )
            })
            .collect();
        let patches = match patches {
            true => (branches.iter())
                .map(|(ticket, branch)| {
                    let head = heads.iter().find(|(t, _)| t == ticket).map(|(_, h)| h);
                    let worktree = worktrees.get(ticket).cloned().unwrap_or_default();
                    let diff = git.patch(&repo, main.as_deref(), &base, branch, head, &worktree);
                    (*ticket, diff)
                })
                .collect(),
            false => BTreeMap::new(),
        };
        let live: BTreeSet<String> = heads.into_iter().map(|(_, head)| head).collect();
        git.keep(main.as_deref(), &live);
        GitAnswers {
            git,
            main,
            files,
            behind,
            merges,
            named,
            bodies,
            patches,
        }
    }
}

impl Git {
    /// `origin/main`'s commit, as last fetched
    fn main(repo: &Reading, base: &str) -> Option<String> {
        let tracking = format!("refs/remotes/origin/{base}");
        loose_commit_in(&repo.path, &tracking)
            .or_else(|| read(repo, &["rev-parse", "--verify", "--quiet", &tracking]).map(trimmed))
    }

    /// Every file on `main`
    fn tree(&mut self, repo: &Reading, main: &str) -> &[String] {
        if self.tree.as_ref().is_none_or(|(at, _)| at != main) {
            self.tree = Some((main.to_owned(), files_at(repo, main)));
        }
        self.tree.as_ref().map_or(&[], |(_, files)| files)
    }

    /// The files `head` changes since it left `main`, `None` when git cannot say
    fn files(&mut self, repo: &Reading, main: &str, head: &str) -> Option<Vec<String>> {
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

    /// The files in conflict merging commits `a` and `b`, two heads or a head
    /// and `main`, empty when they merge cleanly, and `None` when git cannot say
    pub(super) fn conflicts(&mut self, repo: &Reading, a: &str, b: &str) -> Option<Vec<String>> {
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
        let merged = |h: &String| kept(h) || Some(h.as_str()) == main;
        self.diffs
            .retain(|(at, head), _| Some(at.as_str()) == main && kept(head));
        self.merges.retain(|(a, b), _| merged(a) && merged(b));
        self.patches
            .retain(|(at, head, _), _| Some(at.as_str()) == main && kept(head));
    }

    /// `branch`'s diff from `main`, at `head`, its commit where git read one
    fn patch(
        &mut self,
        repo: &Reading,
        main: Option<&str>,
        base: &str,
        branch: &str,
        head: Option<&String>,
        worktree: &Path,
    ) -> Result<Vec<u8>, String> {
        let main = main.ok_or_else(|| format!("origin/{base} is not in the checkout"))?;
        let head = head.ok_or_else(|| format!("{branch} is not in the checkout"))?;
        let key = (main.to_owned(), head.clone(), worktree.to_owned());
        if let Some(diff) = self.patches.get(&key) {
            return Ok(diff.clone());
        }
        let files = self.files(repo, main, head);
        let diff = patch::read(repo, main, head, files.as_deref(), worktree)?;
        self.patches.insert(key, diff.clone());
        Ok(diff)
    }
}

/// The commit each of `refs` that exists is at, by ref, from one git call
///
/// `rev-parse --verify` takes a single revision, so `for-each-ref` lists them.
pub(super) fn commits<'a>(
    repo: &Reading,
    refs: impl IntoIterator<Item = &'a String>,
) -> BTreeMap<String, String> {
    let mut args = vec!["for-each-ref", "--format=%(objectname) %(refname)"];
    args.extend(refs.into_iter().map(String::as_str));
    let listing = read(repo, &args).unwrap_or_default();
    (listing.lines())
        .filter_map(|line| line.split_once(' '))
        .map(|(id, name)| (name.to_owned(), id.to_owned()))
        .collect()
}

/// Every file on `origin/<base>` as last fetched, none where git cannot say
pub(in crate::runner) fn files_on(repo: &Path, base: &str) -> Vec<String> {
    let repo = Reading::of(repo);
    Git::main(&repo, base).map_or_else(Vec::new, |main| files_at(&repo, &main))
}

// Every file at commit `at`.
fn files_at(repo: &Reading, at: &str) -> Vec<String> {
    let files = read(repo, &["ls-tree", "-r", "--name-only", "-z", at]);
    files.map(names).unwrap_or_default()
}

fn read(repo: &Reading, args: &[&str]) -> Option<String> {
    match run(repo, args)? {
        (0, out) => Some(out),
        _ => None,
    }
}

fn run(repo: &Reading, args: &[&str]) -> Option<(i32, String)> {
    let output = repo
        .git()
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
        assert_eq!(
            cache.conflicts(&Reading::of(dir.path()), missing, other),
            None
        );
        assert!(cache.merges.is_empty(), "an unknown merge is not kept");
    }

    #[test]
    fn the_answer_for_a_head_against_main_is_kept_while_both_stand() {
        let mut cache = Git::default();
        cache
            .merges
            .insert(("aaa".into(), "bbb".into()), vec!["README.md".into()]);
        let heads = BTreeSet::from(["bbb".to_owned()]);
        cache.keep(Some("aaa"), &heads);
        assert_eq!(cache.merges.len(), 1, "main and the head stand");
        cache.keep(Some("ccc"), &heads);
        assert!(cache.merges.is_empty(), "main moved on");
    }
}
