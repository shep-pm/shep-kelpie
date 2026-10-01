//! `git commit`, `git tag` and `git push` in a worker's Bash call
//!
//! Messages are read from the command. What a commit adds and a push sends
//! is read from the worktree's own git, once a call, through kelpie's
//! trusted worktree git. A commit or push anywhere else, in a folder the
//! guard cannot follow, or pointed at another repo or config is refused, as
//! is any git command, option or push form it does not know: the guard can
//! only vouch for the worktree as it stands. A push to the base branch is
//! refused wherever it runs, since only the project manager changes it.

mod front;
mod runs;

use std::collections::HashMap;
use std::path::Path;

use super::wrap::Reach;
use super::{Checkout, Home, REWRITE, WRITE, files, moved, values};
use crate::worktree::{self, BASE, WorktreeError};
use front::{BUILTINS, front};
pub(super) use runs::{bisect_run, makes_repo, scripts};

/// The worktree reads one call has made, so none runs twice
#[derive(Debug, Default)]
pub(super) struct Reads(HashMap<Read, Result<Vec<String>, String>>);

/// A git command, its wrappers off
#[derive(Debug, Clone, Copy)]
pub(super) struct Git<'a> {
    /// Its words, `git` first
    pub words: &'a [String],
    /// The heredoc bodies it reads
    pub heredocs: &'a [String],
    /// Whether an assignment in front points it at another repo
    pub redirected: bool,
    /// Whether an assignment gives it config from the call's text
    pub configured: bool,
    /// Whether it surely runs, or may only be named
    pub reach: Reach,
    /// Whether the call made a repo before it
    pub after_new_repo: bool,
}

/// Judges one git command run in `cwd`
pub(super) fn judge(
    git: Git<'_>,
    cwd: Option<&Path>,
    home: &Home,
    checkout: Checkout<'_>,
    reads: &mut Reads,
) -> Vec<String> {
    let runs = git.reach == Reach::Runs;
    // A program kelpie does not know may only name git, as `rg git` does.
    let front = match front(git.words) {
        Ok(front) => front,
        Err(refusal) if runs => return vec![refusal],
        Err(_) => return Vec::new(),
    };
    let Some(at) = front.sub else {
        return Vec::new();
    };
    let sub = git.words[at].as_str();
    let mut out = Vec::new();
    if git.configured {
        out.push(
            "kelpie cannot check git given config in its environment (`GIT_CONFIG_*` or \
             `GIT_ALLOW_PROTOCOL`), which can name a program git runs: run it without."
                .to_owned(),
        );
    }
    if let Some(key) = front
        .config
        .iter()
        .map(|c| runs::key(c))
        .find(|k| !runs::safe(k))
    {
        out.push(format!(
            "kelpie cannot check git run with the config `{}`, which can name a program git \
             runs: run it without.",
            key.chars().take(40).collect::<String>()
        ));
    }
    // An alias or an external command could be anything, and no alias
    // shares a built-in's name. Some built-ins run a command of their own,
    // as `submodule foreach` does, so they are left out too.
    if !BUILTINS.contains(&sub) {
        if runs {
            out.push(format!(
                "kelpie runs only the git commands it knows, and `{}` is not one: run the plain \
                 git command you need, or the one an alias stands for.",
                sub.chars().take(40).collect::<String>()
            ));
        }
        return out;
    }
    let dir = front.moves.iter().fold(cwd.map(Path::to_owned), |dir, to| {
        moved(dir.as_deref(), to, home.path())
    });
    let args = &git.words[at + 1..];
    if sub == "config"
        && let Some(key) = runs::config_set(args)
        && !runs::safe(key)
    {
        out.push(format!(
            "kelpie cannot check git once its config names a program for it to run, and \
             `{}` can: set only keys that run nothing, such as `color.*`, `user.*` or \
             `core.quotepath`.",
            key.chars().take(40).collect::<String>()
        ));
    }
    // Where a push goes is judged wherever it runs from.
    let pushed = if sub == "push" {
        match refspecs(args) {
            Ok(specs) if specs.iter().any(Refspec::to_base) => {
                out.push(to_base());
                return out;
            }
            Ok(specs) => specs,
            Err(refusal) => {
                // Behind a program kelpie does not know, a push is refused below.
                if runs {
                    out.push(refusal);
                    return out;
                }
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    if sub == "commit" || sub == "tag" {
        let messages = values(args, &["--message"], &['m'])
            .into_iter()
            .chain(files(args, &["--file"], &['F'], dir.as_deref()))
            .chain(git.heredocs.iter().cloned());
        if let Some(leak) = home.find_in_prose(messages) {
            out.push(home.refusal(&format!("this {sub}'s message"), leak, WRITE));
        }
    }
    // A commit is read for what names this machine; a push, for its branch too.
    if !matches!(sub, "commit" | "push") {
        return out;
    }
    if !runs {
        out.push(format!(
            "kelpie checks a {sub} only when git is the command's program, or runs behind a \
             wrapper it knows such as `env` or `sudo`: run git directly."
        ));
        return out;
    }
    if git.redirected || front.redirected || front.configured {
        out.push(format!(
            "kelpie checks a {sub} only in this worktree's own git, as its config stands: run \
             it without `--git-dir`, `--work-tree`, `-c`, `--config-env` or a `GIT_` variable."
        ));
        return out;
    }
    if !in_own_repo(checkout.worktree, dir.as_deref()) {
        out.push(format!(
            "this {sub} runs outside this worktree's own repo, which kelpie does not check: \
             run it from the worktree."
        ));
        return out;
    }
    // A folder there before the call may hold a repo the call makes.
    if git.after_new_repo {
        out.push(format!(
            "this {sub} follows a `git init`, `git clone` or `git worktree add` in the same \
             call, which makes a repo kelpie does not check: run it in a call of its own."
        ));
        return out;
    }
    let keys = if sub == "push" {
        let current = pushed.iter().any(Refspec::sends_current_branch);
        let sent = pushed
            .into_iter()
            .filter(|spec| !spec.source.is_empty())
            .map(|spec| Read::Push(spec.source));
        std::iter::once(Read::PushConfig)
            .chain(current.then_some(Read::Branch))
            .chain(sent)
            .collect()
    } else {
        vec![Read::Commit {
            all: all_tracked(args),
        }]
    };
    for key in keys {
        let found = reads
            .0
            .entry(key.clone())
            .or_insert_with(|| {
                worktree::trusted(checkout.git_common_dir, checkout.worktree)
                    .and_then(|run| read(&key, home, run))
                    .map_err(|e| e.to_string())
            })
            .clone();
        match found {
            Ok(found) => out.extend(found),
            // Git that cannot be read is refused, not let through unchecked.
            Err(e) => out.push(format!(
                "kelpie cannot read this worktree's git to check this {sub}: {e}"
            )),
        }
    }
    out
}

/// One read of the worktree's git, keyed so a call makes it once
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Read {
    /// What a commit adds: staged, or every tracked change with `all`
    Commit { all: bool },
    /// What a push of this source sends past `origin`'s base branch
    Push(String),
    /// The branch checked out, where a push of `HEAD` alone goes
    Branch,
    /// Config that has a push send more than the refs it names
    PushConfig,
}

// Push options that take no value. Git takes an abbreviated long option, so
// only these exact spellings pass.
const PUSH_FLAGS: [&str; 30] = [
    "-u",
    "--set-upstream",
    "-q",
    "--quiet",
    "-v",
    "--verbose",
    "--progress",
    "--no-progress",
    "-n",
    "--dry-run",
    "--porcelain",
    "--no-verify",
    "--verify",
    "--atomic",
    "--no-atomic",
    "--thin",
    "--no-thin",
    "--signed",
    "--no-signed",
    "--force-with-lease",
    "--no-force-with-lease",
    "--force-if-includes",
    "--no-force-if-includes",
    "-4",
    "-6",
    "--ipv4",
    "--ipv6",
    "-f",
    "--force",
    "--no-recurse-submodules",
];

// Push options that take the next word, or an `=` value.
const PUSH_VALUED: [&str; 4] = ["-o", "--push-option", "--repo", "--force-with-lease"];

/// One refspec of a push
#[derive(Debug, Clone, PartialEq, Eq)]
struct Refspec {
    /// What it sends from, empty for a delete
    source: String,
    /// The branch it names on the remote, `None` when git takes the source's
    destination: Option<String>,
}

impl Refspec {
    // Git matches `main`, `heads/main` and `refs/heads/main` to the remote's
    // branch. A source alone goes to the branch of its own name.
    fn to_base(&self) -> bool {
        let named = self.destination.as_deref().unwrap_or(&self.source);
        let full = format!("refs/heads/{BASE}");
        [BASE, &full["refs/".len()..], &full].contains(&named)
    }

    // `HEAD` or `@` alone goes to the branch checked out.
    fn sends_current_branch(&self) -> bool {
        self.destination.is_none() && matches!(self.source.as_str(), "HEAD" | "@")
    }
}

/// The refusal for a push to the base branch
fn to_base() -> String {
    format!(
        "only the project manager changes `{BASE}`, on the maintainer's ruling: push your own \
         branch with `git push origin HEAD`."
    )
}

// A push's refspecs; `HEAD` alone when it names none.
fn refspecs(args: &[String]) -> Result<Vec<Refspec>, String> {
    let unknown = |word: &str| {
        Err(format!(
            "kelpie checks a push only in the forms it knows, and not with `{}`: push HEAD \
             to a branch, with no more than `-u`.",
            word.chars().take(40).collect::<String>()
        ))
    };
    let mut positional = Vec::new();
    let mut words = args.iter();
    while let Some(word) = words.next() {
        let word = word.as_str();
        let name = word.split_once('=').map_or(word, |(name, _)| name);
        if word == "--" {
            positional.extend(words.by_ref().cloned());
        } else if PUSH_FLAGS.contains(&word) || word.contains('=') && PUSH_VALUED.contains(&name) {
        } else if PUSH_VALUED.contains(&word) {
            words.next();
        } else if matches!(
            word,
            "--recurse-submodules=no" | "--recurse-submodules=check"
        ) {
        } else if word.starts_with('-') {
            return unknown(word);
        } else {
            positional.push(word.to_owned());
        }
    }
    // The first is the remote.
    let mut out = Vec::new();
    for refspec in positional.iter().skip(1) {
        let refspec = refspec.strip_prefix('+').unwrap_or(refspec);
        let (source, destination) = match refspec.split_once(':') {
            Some((source, destination)) => (source, Some(destination)),
            None => (refspec, None),
        };
        if source.starts_with('-') {
            return Err("kelpie cannot read a push source that starts with `-`.".into());
        }
        // `:` alone pushes every branch that matches one on the remote, and
        // `:branch` deletes.
        if source.is_empty() && destination.is_none_or(str::is_empty) {
            return unknown(refspec);
        }
        out.push(Refspec {
            source: source.to_owned(),
            destination: destination.map(str::to_owned),
        });
    }
    if positional.len() < 2 {
        out.push(Refspec {
            source: "HEAD".into(),
            destination: None,
        });
    }
    Ok(out)
}

// `-a` or `--all`, which commit every tracked change, staged or not.
fn all_tracked(args: &[String]) -> bool {
    args.iter()
        .take_while(|a| *a != "--")
        .any(|a| a == "--all" || a.starts_with('-') && !a.starts_with("--") && a.contains('a'))
}

// Plain patches: no colour, and no program the repo's config names.
const PLAIN: [&str; 4] = [
    "--unified=0",
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
];

// What a commit adds, or a push sends, that names this machine, and a
// push of `HEAD` from the base branch.
fn read(
    key: &Read,
    home: &Home,
    run: impl Fn(&[&str]) -> Result<String, WorktreeError>,
) -> Result<Vec<String>, WorktreeError> {
    // `--unified` alone makes `git log` print patches, so only patch reads take these.
    let patches = |args: &[&str]| run(&[args, &PLAIN].concat());
    let mut out = Vec::new();
    match key {
        Read::Branch => {
            let branch = run(&["rev-parse", "--symbolic-full-name", "HEAD"])?;
            if branch.trim() == format!("refs/heads/{BASE}") {
                out.push(to_base());
            }
        }
        Read::PushConfig => {
            let config = run(&["config", "--list", "-z"])?;
            for entry in config.split('\0') {
                let (key, value) = entry.split_once('\n').unwrap_or((entry, ""));
                let key = key.to_lowercase();
                if runs::pushes_more(&key, &value.to_lowercase()) {
                    out.push(format!(
                        "this repo's git config sets `{key}`, which has a push send more than \
                         the refs it names, and kelpie checks only those: take it back with \
                         `{}`, adding `--global` if it is set there, then push.",
                        runs::push_fix(&key)
                    ));
                }
            }
        }
        Read::Push(source) => {
            // A file written and committed in one call is not staged when the
            // commit is judged, so the push reads what it sends. The base is
            // `origin`'s, which the worker cannot move, unlike its own branch's.
            let base = format!("refs/remotes/origin/{BASE}");
            let log = run(&["log", "--format=%B", source, "--not", &base])?;
            if let Some(leak) = home.find_in_prose([log]) {
                let what = "a message in the commits this push sends";
                out.push(home.refusal(what, leak, REWRITE));
            }
            let sent = patches(&["log", "-p", "--format=", source, "--not", &base])?;
            for (file, leak) in home.added(&sent) {
                let what = format!("{file} in the commits this push sends");
                out.push(home.refusal(&what, leak, REWRITE));
            }
        }
        Read::Commit { all } => {
            let range = if *all { "HEAD" } else { "--cached" };
            for (file, leak) in home.added(&patches(&["diff", range])?) {
                out.push(home.refusal(&format!("this commit's {file}"), leak, WRITE));
            }
        }
    }
    Ok(out)
}

// Whether `dir` is the worker's own repo: in the worktree and in no repo it
// made there, bare ones too. A folder the guard cannot follow (`None`), or
// one that is not there when the hook runs, such as one the call will make, is not.
fn in_own_repo(worktree: &Path, dir: Option<&Path>) -> bool {
    let (Ok(worktree), Some(Ok(dir))) = (worktree.canonicalize(), dir.map(Path::canonicalize))
    else {
        return false;
    };
    dir.starts_with(&worktree)
        && dir.ancestors().take_while(|a| *a != worktree).all(|a| {
            a.join(".git").symlink_metadata().is_err()
                && !["HEAD", "objects", "refs"]
                    .iter()
                    .all(|name| a.join(name).symlink_metadata().is_ok())
        })
}
