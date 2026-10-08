//! A work item's worktree and build folder
//!
//! Each work item gets its own git worktree, on a branch cut from the latest
//! `origin/main`, and its own build folder. A rework's branch starts from
//! itself on `origin` instead, and reuses a local branch left behind that
//! matches it exactly. Preparing is idempotent, so a restarted runner
//! finds the worktree it made before and keeps it.

use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The branch every work item is cut from, on `origin`
pub const BASE: &str = "main";

/// Where a new worktree's branch starts
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// Cut from `origin/main`
    Main,
    /// The branch as `origin` holds it, whoever pushed to it
    Pushed,
}

/// A work item's worktree and the project repo it belongs to, whose git
/// dirs kelpie checks it against before running git on it
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Linked<'a> {
    /// The project's repo
    pub repo: &'a Path,
    /// The work item's worktree, which its worker writes
    pub worktree: &'a Path,
}

/// A prepared worktree, and the git dirs a commit from it writes to
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// The repo's common git dir: objects, refs and config
    pub git_common_dir: PathBuf,
    /// The worktree's own git dir: its index and HEAD
    pub git_dir: PathBuf,
}

/// Why a worktree or build folder could not be prepared
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeError {
    /// git could not be started, with the OS's reason
    Spawn(String),
    /// A git command exited unsuccessfully
    Git {
        /// Its arguments
        args: String,
        /// What it printed on stderr
        stderr: String,
    },
    /// The folder exists but is not a worktree on the work item's branch
    Foreign(PathBuf),
    /// The branch exists with no worktree, so it is not kelpie's to reuse
    BranchTaken(String),
    /// The worktree is off the head kelpie knew, or has changes not committed
    Unsettled(PathBuf),
    /// A folder could not be created
    Folder {
        /// The folder
        path: PathBuf,
        /// What creating it failed with
        kind: io::ErrorKind,
    },
    /// A file or folder in the worktree could not be read
    Unreadable {
        /// The file or folder
        path: PathBuf,
        /// What reading it failed with
        kind: io::ErrorKind,
    },
    /// A build folder could not be removed
    Remove {
        /// The folder
        path: PathBuf,
        /// What removing it failed with
        kind: io::ErrorKind,
    },
}

impl fmt::Display for WorktreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "cannot run git: {e}"),
            Self::Git { args, stderr } => write!(f, "git {args} failed: {}", stderr.trim()),
            Self::Foreign(path) => write!(
                f,
                "{} exists and is not this work item's worktree",
                path.display()
            ),
            Self::BranchTaken(branch) => {
                write!(f, "branch {branch} already exists without its worktree")
            }
            Self::Unsettled(path) => write!(
                f,
                "{} holds work kelpie has not seen pushed",
                path.display()
            ),
            Self::Folder { path, kind } => {
                write!(f, "cannot create {}: {kind}", path.display())
            }
            Self::Unreadable { path, kind } => {
                write!(f, "cannot read {}: {kind}", path.display())
            }
            Self::Remove { path, kind } => {
                write!(f, "cannot remove {}: {kind}", path.display())
            }
        }
    }
}

impl core::error::Error for WorktreeError {}

/// Makes sure `worktree` is a worktree of `repo` on `branch`, and `build` exists
///
/// A new worktree's branch starts where `start` says, just fetched, and does
/// not track it.
///
/// # Errors
///
/// [`WorktreeError`] naming the git command or folder that failed.
pub fn prepare(
    repo: &Path,
    worktree: &Path,
    branch: &str,
    start: Start,
    build: &Path,
) -> Result<Worktree, WorktreeError> {
    let foreign = || WorktreeError::Foreign(worktree.to_owned());
    let full_ref = format!("refs/heads/{branch}");
    if worktree.exists() {
        if listed_branch(repo, worktree)?.as_deref() != Some(full_ref.as_str()) {
            return Err(foreign());
        }
    } else {
        let local = git(repo, ["rev-parse", "--verify", "--quiet", &full_ref]).ok();
        if local.is_some() && start == Start::Main {
            return Err(WorktreeError::BranchTaken(branch.to_owned()));
        }
        if let Some(parent) = worktree.parent() {
            create(parent)?;
        }
        let from = match start {
            Start::Main => BASE,
            Start::Pushed => branch,
        };
        git(repo, ["fetch", "--quiet", "origin", from])?;
        let base = format!("origin/{from}");
        let wt = worktree.as_os_str();
        if let Some(local) = local {
            // A rework's branch left behind by an earlier attempt is reused
            // when it is at the same commit as `origin`'s. One ahead holds
            // work that is not kelpie's to throw away, and one behind is
            // not the pull request's branch as `origin` holds it.
            if git(repo, ["rev-parse", "--verify", "--quiet", &base])? != local {
                return Err(WorktreeError::BranchTaken(branch.to_owned()));
            }
            git(
                repo,
                [
                    "worktree".as_ref(),
                    "add".as_ref(),
                    "--quiet".as_ref(),
                    wt,
                    OsStr::new(branch),
                ],
            )?;
        } else {
            git(
                repo,
                [
                    "worktree".as_ref(),
                    "add".as_ref(),
                    "--quiet".as_ref(),
                    "--no-track".as_ref(),
                    "-b".as_ref(),
                    OsStr::new(branch),
                    wt,
                    base.as_ref(),
                ],
            )?;
        }
    }
    let common = git(
        repo,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let git_common_dir = canonical(Path::new(&common));
    let git_dir = own_git_dir(&git_common_dir, worktree).ok_or_else(foreign)?;
    create(build)?;
    Ok(Worktree {
        git_common_dir,
        git_dir,
    })
}

/// Removes a work item's worktree, its branch and its build folder
///
/// `remote` also deletes the branch on `origin`. Whatever is already gone,
/// a branch the forge deleted first included, is skipped, so a removal cut short can run again.
///
/// # Errors
///
/// [`WorktreeError`] naming the git command or folder that failed.
pub fn remove(
    repo: &Path,
    worktree: &Path,
    branch: &str,
    build: &Path,
    remote: bool,
) -> Result<(), WorktreeError> {
    if worktree.exists() {
        let wt = worktree.as_os_str();
        git(
            repo,
            [
                "worktree".as_ref(),
                "remove".as_ref(),
                "--force".as_ref(),
                wt,
            ],
        )?;
    }
    git(repo, ["worktree", "prune"])?;
    let full_ref = format!("refs/heads/{branch}");
    if git(repo, ["rev-parse", "--verify", "--quiet", &full_ref]).is_ok() {
        git(repo, ["branch", "--quiet", "-D", branch])?;
    }
    if remote && on_origin(repo, &full_ref)? {
        // The forge deletes a merged head branch itself on some repos, and
        // may do it between the check above and this push. Git's error for
        // that varies by version and host, so a failed delete is judged by
        // whether the ref is still there.
        let deleted = git(repo, ["push", "--quiet", "origin", "--delete", &full_ref]);
        if let Err(e) = deleted
            && on_origin(repo, &full_ref)?
        {
            return Err(e);
        }
    }
    match std::fs::remove_dir_all(build) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(WorktreeError::Remove {
            path: build.to_owned(),
            kind: e.kind(),
        }),
        _ => Ok(()),
    }
}

/// Makes `view` a detached worktree of `repo` at `origin/main`, just
/// fetched, for a session that reads the repo and changes nothing
///
/// # Errors
///
/// [`WorktreeError`] naming the git command or folder that failed.
pub fn view(repo: &Path, view: &Path) -> Result<(), WorktreeError> {
    if let Some(parent) = view.parent() {
        create(parent)?;
    }
    git(repo, ["fetch", "--quiet", "origin", BASE])?;
    let base = format!("origin/{BASE}");
    let args: [&OsStr; 5] = [
        "worktree".as_ref(),
        "add".as_ref(),
        "--detach".as_ref(),
        view.as_os_str(),
        base.as_ref(),
    ];
    git(repo, args).map(drop)
}

/// Removes the detached worktree [`view`] made, if it is there
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn remove_view(repo: &Path, view: &Path) -> Result<(), WorktreeError> {
    if view.exists() {
        let args: [&OsStr; 4] = [
            "worktree".as_ref(),
            "remove".as_ref(),
            "--force".as_ref(),
            view.as_os_str(),
        ];
        git(repo, args)?;
    }
    git(repo, ["worktree", "prune"]).map(drop)
}

/// Whether `full_ref` is a branch on `origin` right now, asked of the remote
fn on_origin(repo: &Path, full_ref: &str) -> Result<bool, WorktreeError> {
    Ok(!git(repo, ["ls-remote", "--heads", "origin", full_ref])?.is_empty())
}

/// Where a pull request's head stands against `origin`, just fetched
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Base {
    /// The head is the branch on `origin`, and has the latest `main`
    Current,
    /// The head is the branch on `origin`, and lacks the latest `main`
    Behind,
    /// The branch on `origin` is not the head the forge reported, which
    /// lags a push by a moment
    Lagging,
}

/// Fetches `origin`, and says where `head`, the forge's head of `branch`, stands
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn base_of(repo: &Path, branch: &str, head: &str) -> Result<Base, WorktreeError> {
    git(repo, ["fetch", "--quiet", "origin", BASE, branch])?;
    let tracking = format!("refs/remotes/origin/{branch}");
    if git(repo, ["rev-parse", "--verify", "--quiet", &tracking])? != head {
        return Ok(Base::Lagging);
    }
    // `--is-ancestor` answers no with exit 1, and fails with any other code.
    let base = format!("origin/{BASE}");
    let args = ["merge-base", "--is-ancestor", &base, head];
    let output = in_repo(repo)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| WorktreeError::Spawn(e.to_string()))?;
    match output.status.code() {
        Some(0) => Ok(Base::Current),
        Some(1) => Ok(Base::Behind),
        _ => Err(WorktreeError::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).into(),
        }),
    }
}

/// Fetches `branch` from `origin`, and returns its head there
///
/// Ask git rather than the forge: the forge's head lags a push by a moment.
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn origin_head(repo: &Path, branch: &str) -> Result<String, WorktreeError> {
    git(repo, ["fetch", "--quiet", "origin", branch])?;
    let tracking = format!("refs/remotes/origin/{branch}");
    git(repo, ["rev-parse", "--verify", "--quiet", &tracking])
}

/// The commit `worktree` has checked out
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn head(repo: &Path, worktree: &Path) -> Result<String, WorktreeError> {
    trusted(repo, worktree)?(&["rev-parse", "HEAD"])
}

/// The files `worktree` holds that its last commit does not: changed, staged
/// or new and not ignored
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn uncommitted(repo: &Path, worktree: &Path) -> Result<Vec<String>, WorktreeError> {
    // One `status` sees the index, the working tree and the new files. Its
    // second format, since the first starts an entry with a space that `git`
    // trims off the first one.
    let status =
        trusted(repo, worktree)?(&["status", "--porcelain=v2", "-z", "--untracked-files=all"])?;
    let mut files = status_names(&status);
    files.sort();
    files.dedup();
    Ok(files)
}

// The paths in `git status --porcelain=v2 -z`, where an entry is a kind, its
// fields and then the path, and a rename or copy adds the old path as an
// entry of its own.
fn status_names(status: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut entries = status.split('\0').filter(|entry| !entry.is_empty());
    while let Some(entry) = entries.next() {
        let fields = match entry.chars().next() {
            Some('?') => 2,
            Some('1') => 9,
            Some('u') => 11,
            Some('2') => {
                names.extend(entries.next().map(str::to_owned));
                10
            }
            _ => continue,
        };
        names.extend(entry.splitn(fields, ' ').last().map(str::to_owned));
    }
    names
}

#[cfg(test)]
mod tests {
    use super::status_names;

    #[test]
    fn a_status_names_every_changed_staged_renamed_and_new_path() {
        let status = "1 .M N... 100644 100644 100644 aaaa bbbb src/a b.rs\0\
                      1 AD N... 000000 100644 000000 0000 cccc staged.txt\0\
                      2 R. N... 100644 100644 100644 dddd eeee R100 new.rs\0old.rs\0\
                      u UU N... 100644 100644 100644 100644 ffff gggg hhhh clash.rs\0\
                      ? left.txt\0";
        assert_eq!(
            status_names(status),
            [
                "src/a b.rs",
                "staged.txt",
                "old.rs",
                "new.rs",
                "clash.rs",
                "left.txt"
            ]
        );
    }
}

/// Moves the worktree's branch from `from`, the head kelpie knew, to `to`
///
/// `to` is a head on `origin` the maintainer accepted. A worktree at neither,
/// or with changes not committed, holds work of the worker's, and is refused.
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed, or
/// [`WorktreeError::Unsettled`] for a worktree refused.
pub fn adopt(
    repo: &Path,
    worktree: &Path,
    branch: &str,
    from: &str,
    to: &str,
) -> Result<(), WorktreeError> {
    let in_worktree = trusted(repo, worktree)?;
    let unsettled = || WorktreeError::Unsettled(worktree.to_owned());
    let full_ref = format!("refs/heads/{branch}");
    let on_branch = in_worktree(&["symbolic-ref", "--quiet", "HEAD"])
        .ok()
        .as_deref()
        == Some(&full_ref);
    if !on_branch || !in_worktree(&["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
        return Err(unsettled());
    }
    let at = in_worktree(&["rev-parse", "HEAD"])?;
    if at == to {
        return Ok(());
    }
    if at != from {
        return Err(unsettled());
    }
    git(repo, ["fetch", "--quiet", "origin", branch])?;
    in_worktree(&["reset", "--quiet", "--hard", to])?;
    Ok(())
}

/// What a rebase onto `origin/main` came to
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rebase {
    /// Rebased and pushed: the branch's new head
    Pushed(String),
    /// Aborted, the branch left as it was: it conflicts with `main`, which
    /// the worker can resolve
    Conflicts {
        /// The `origin/main` commit it conflicts with
        main: String,
        /// The files that conflict
        files: Vec<String>,
    },
    /// Left as it was, for this reason, which only the maintainer can settle
    Refused(String),
}

/// Catches the worktree's branch, at `head`, up with `origin/main` and pushes it
///
/// A branch with a merge commit in it (the worker's resolution of an earlier
/// conflict), or one whose commits are not kelpie's to `rewrite`, is merged
/// with `origin/main` and pushed without force. Any other is rebased, and the
/// push is forced with a lease on `head`, so it fails rather than drop a
/// commit pushed since. A conflict aborts and names its files, and a failed
/// push puts the branch back at `head`. Run [`base_of`] first, which fetches.
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn rebase(
    repo: &Path,
    worktree: &Path,
    branch: &str,
    head: &str,
    rewrite: bool,
) -> Result<Rebase, WorktreeError> {
    let in_worktree = trusted(repo, worktree)?;
    let full_ref = format!("refs/heads/{branch}");
    let on_branch = in_worktree(&["symbolic-ref", "--quiet", "HEAD"])
        .ok()
        .as_deref()
        == Some(&full_ref);
    if !on_branch || in_worktree(&["rev-parse", "HEAD"])? != head {
        let short = head.get(..7).unwrap_or(head);
        return Ok(Rebase::Refused(format!(
            "its worktree is not at the pull request's head {short}"
        )));
    }
    if !in_worktree(&["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
        return Ok(Rebase::Refused(
            "its worktree has changes that are not committed".into(),
        ));
    }
    // The rebased commits take the head's committer, the worker's one
    // identity, so the rebase needs no identity of its own.
    let name = format!(
        "user.name={}",
        in_worktree(&["log", "-1", "--format=%cn", head])?
    );
    let email = format!(
        "user.email={}",
        in_worktree(&["log", "-1", "--format=%ce", head])?
    );
    let base = format!("origin/{BASE}");
    // A rebase replays the branch's own commits and drops its merge commits,
    // and with them the worker's hand resolution of an earlier conflict. A
    // branch holding one is caught up by merging instead, and pushed plain.
    let ahead = format!("{base}..HEAD");
    let merging =
        !rewrite || !in_worktree(&["rev-list", "--merges", "--max-count=1", &ahead])?.is_empty();
    let (verb, abort): (&[&str], &[&str]) = if merging {
        (
            &["merge", "--quiet", "--no-edit", &base],
            &["merge", "--abort"],
        )
    } else {
        (&["rebase", "--quiet", &base], &["rebase", "--abort"])
    };
    let mut caught_up = vec!["-c", &name, "-c", &email];
    caught_up.extend_from_slice(verb);
    if let Err(e) = in_worktree(&caught_up) {
        let conflicts =
            in_worktree(&["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
        let aborted = in_worktree(abort);
        if conflicts.is_empty() {
            return Err(e);
        }
        aborted?;
        return Ok(Rebase::Conflicts {
            main: git(repo, ["rev-parse", &base])?,
            files: conflicts.lines().map(str::to_owned).collect(),
        });
    }
    let rebased = in_worktree(&["rev-parse", "HEAD"])?;
    let lease = format!("--force-with-lease={full_ref}:{head}");
    let target = format!("HEAD:{full_ref}");
    let pushed = if merging {
        in_worktree(&["push", "--quiet", "origin", &target])
    } else {
        in_worktree(&["push", "--quiet", &lease, "origin", &target])
    };
    if let Err(e) = pushed {
        // Best effort: a branch left off the head is refused on the next look.
        let _ = in_worktree(&["reset", "--quiet", "--hard", head]);
        return Err(e);
    }
    Ok(Rebase::Pushed(rebased))
}

// Git for the worktree, with its git dirs named rather than found, as every
// git kelpie runs on a worker's worktree must be: git that finds them reads
// the config of whatever repo the worker points it at. The worker can write
// the worktree's own git dir, so its `commondir` is checked against the
// repo's, and hooks are off: none of them is kelpie's to run. Replace refs
// are ignored too: a worker's `git replace` would show a read one commit
// while a push packs another.
pub(crate) fn trusted<'a>(
    repo: &Path,
    worktree: &'a Path,
) -> Result<impl Fn(&[&str]) -> Result<String, WorktreeError> + 'a, WorktreeError> {
    let dirs = git_dirs(repo, worktree)?;
    Ok(move |args: &[&str]| output(trusted_git(worktree, &dirs), args))
}

/// `git` about `worktree`, as [`trusted`] runs it, for a call that reads
/// its exit code or its output whole
pub(crate) fn trusted_command(repo: &Path, worktree: &Path) -> Result<Command, WorktreeError> {
    Ok(trusted_git(worktree, &git_dirs(repo, worktree)?))
}

/// Sets the environment that makes the git `program` runs on `worktree` the
/// trusted one, as [`trusted`] runs it
///
/// # Errors
///
/// [`WorktreeError`] when the worktree's git dirs are not `repo`'s.
pub(crate) fn trust_git_of(
    program: &mut Command,
    repo: &Path,
    worktree: &Path,
) -> Result<(), WorktreeError> {
    let (own, common) = git_dirs(repo, worktree)?;
    program
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env("GIT_DIR", own)
        .env("GIT_WORK_TREE", worktree)
        .env("GIT_COMMON_DIR", common)
        .env("GIT_CONFIG_COUNT", TRUSTED_CONFIG.len().to_string())
        .env("GIT_NO_REPLACE_OBJECTS", "1");
    for (n, (key, value)) in TRUSTED_CONFIG.iter().enumerate() {
        program
            .env(format!("GIT_CONFIG_KEY_{n}"), key)
            .env(format!("GIT_CONFIG_VALUE_{n}"), value);
    }
    Ok(())
}

// The config every trusted git runs with.
const TRUSTED_CONFIG: [(&str, &str); 2] = [
    // None of the hooks is kelpie's to run.
    ("core.hooksPath", "/dev/null"),
    // `status`, `diff`, `reset`, `merge` and `rebase` start this program.
    ("core.fsmonitor", "false"),
];

// The worktree's own git dir and the repo's common one, once the own dir's
// `commondir` is checked to name the repo's.
fn git_dirs(repo: &Path, worktree: &Path) -> Result<(PathBuf, PathBuf), WorktreeError> {
    let foreign = || WorktreeError::Foreign(worktree.to_owned());
    let common = git(
        repo,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common = canonical(Path::new(&common));
    let own = own_git_dir(&common, worktree).ok_or_else(foreign)?;
    let named = std::fs::read_to_string(own.join("commondir")).map_err(|_| foreign())?;
    if canonical(&own.join(named.trim())) != common {
        return Err(foreign());
    }
    Ok((own, common))
}

// `git` about `worktree` with its git dirs named. The common dir is named
// too, so git never reads `commondir` again after the check. Config passed
// down from kelpie's own environment is dropped.
fn trusted_git(worktree: &Path, (own, common): &(PathBuf, PathBuf)) -> Command {
    let mut git = in_repo(worktree);
    git.env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_CONFIG_COUNT")
        .env("GIT_COMMON_DIR", common)
        .arg("--git-dir")
        .arg(own)
        .arg("--work-tree")
        .arg(worktree);
    for (key, value) in TRUSTED_CONFIG {
        git.arg("-c").arg(format!("{key}={value}"));
    }
    git.arg("--no-replace-objects");
    git
}

// Everything about the worktree is read from the project's repo, never from
// inside the worktree. The worker can rewrite its worktree's `.git` file,
// and git run there would believe it.

/// The branch `git worktree list` shows checked out at `worktree`
fn listed_branch(repo: &Path, worktree: &Path) -> Result<Option<String>, WorktreeError> {
    let list = git(repo, ["worktree", "list", "--porcelain"])?;
    let want = canonical(worktree);
    let mut here = false;
    for line in list.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            here = canonical(Path::new(path)) == want;
        } else if let Some(branch) = line.strip_prefix("branch ")
            && here
        {
            return Ok(Some(branch.to_owned()));
        }
    }
    Ok(None)
}

/// The folder under `<common>/worktrees/` whose `gitdir` names `worktree`
fn own_git_dir(common: &Path, worktree: &Path) -> Option<PathBuf> {
    let want = canonical(&worktree.join(".git"));
    std::fs::read_dir(common.join("worktrees"))
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|dir| {
            std::fs::read_to_string(dir.join("gitdir"))
                .is_ok_and(|named| canonical(Path::new(named.trim())) == want)
        })
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_owned())
}

fn create(path: &Path) -> Result<(), WorktreeError> {
    std::fs::create_dir_all(path).map_err(|e| WorktreeError::Folder {
        path: path.to_owned(),
        kind: e.kind(),
    })
}

/// `git -C cwd`, waiting its turn for a ref another git holds
///
/// The open work items' worktrees share one repo's refs, and their calls
/// run at once, so a ref or `packed-refs` lock one git holds makes another
/// wait for it rather than fail at once.
pub(crate) fn in_repo(cwd: &Path) -> Command {
    let mut git = crate::spawn::command("git");
    git.args(["-c", "core.filesRefLockTimeout=2000"])
        .args(["-c", "core.packedRefsTimeout=5000"])
        .arg("-C")
        .arg(cwd);
    git
}

pub(crate) fn git<I, S>(cwd: &Path, args: I) -> Result<String, WorktreeError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    output(in_repo(cwd), args)
}

// Runs `git` with `args` added, and returns its trimmed stdout.
fn output<I, S>(mut git: Command, args: I) -> Result<String, WorktreeError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<_> = args.into_iter().map(|a| a.as_ref().to_owned()).collect();
    let output = git
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| WorktreeError::Spawn(e.to_string()))?;
    if !output.status.success() {
        let args: Vec<_> = args.iter().map(|a| a.to_string_lossy()).collect();
        return Err(WorktreeError::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).into(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
