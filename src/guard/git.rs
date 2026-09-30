//! `git commit`, `git tag` and `git push` in a worker's Bash call
//!
//! Messages are read from the command. What a commit adds and a push sends
//! is read from the worktree's own git, once a call, through kelpie's
//! trusted worktree git. A commit or push anywhere else, in a folder the
//! guard cannot follow, or pointed at another repo or config is refused, as
//! is any git command, option or push form it does not know: the guard can
//! only vouch for the worktree as it stands.

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
    home: Option<&Home>,
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
    // shares a built-in's name.
    if !BUILTINS.contains(&sub) {
        if runs {
            out.push(format!(
                "kelpie runs only git's own commands, and `{}` is not one: run the command it \
                 stands for.",
                sub.chars().take(40).collect::<String>()
            ));
        }
        return out;
    }
    let Some(home) = home else {
        return out;
    };
    let dir = front.moves.iter().fold(cwd.map(Path::to_owned), |dir, to| {
        moved(dir.as_deref(), to, Some(home))
    });
    let args = &git.words[at + 1..];
    if sub == "commit" || sub == "tag" {
        let messages = values(args, &["--message"], &['m'])
            .into_iter()
            .chain(files(args, &["--file"], &['F'], dir.as_deref()))
            .chain(git.heredocs.iter().cloned());
        if messages.into_iter().any(|m| home.is_in(&m)) {
            out.push(home.refusal(&format!("this {sub}'s message"), WRITE));
        }
    }
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
        match sources(args) {
            Ok(sources) => std::iter::once(Read::PushConfig)
                .chain(sources.into_iter().map(Read::Push))
                .collect(),
            Err(refusal) => {
                out.push(refusal);
                return out;
            }
        }
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

// What each of a push's refspecs sends from; `HEAD` when it names none.
fn sources(args: &[String]) -> Result<Vec<String>, String> {
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
        let (source, destination) = refspec.split_once(':').unwrap_or((refspec, refspec));
        if source.starts_with('-') {
            return Err("kelpie cannot read a push source that starts with `-`.".into());
        }
        match (source.is_empty(), destination.is_empty()) {
            // `:` alone pushes every branch that matches one on the remote.
            (true, true) => return unknown(refspec),
            // `:branch` deletes, and sends nothing.
            (true, false) => {}
            (false, _) => out.push(source.to_owned()),
        }
    }
    if positional.len() < 2 {
        out.push("HEAD".into());
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

// What a commit adds, or a push sends, that names the home folder.
fn read(
    key: &Read,
    home: &Home,
    run: impl Fn(&[&str]) -> Result<String, WorktreeError>,
) -> Result<Vec<String>, WorktreeError> {
    // `--unified` alone makes `git log` print patches, so only patch reads take these.
    let patches = |args: &[&str]| run(&[args, &PLAIN].concat());
    let mut out = Vec::new();
    match key {
        Read::Push(source) => {
            // A file written and committed in one call is not staged when the
            // commit is judged, so the push reads what it sends. The base is
            // `origin`'s, which the worker cannot move, unlike its own branch's.
            let base = format!("refs/remotes/origin/{BASE}");
            let log = run(&["log", "--format=%B", source, "--not", &base])?;
            if home.is_in(&log) {
                out.push(home.refusal("a message in the commits this push sends", REWRITE));
            }
            let sent = patches(&["log", "-p", "--format=", source, "--not", &base])?;
            for file in home.added(&sent) {
                out.push(home.refusal(&format!("{file} in the commits this push sends"), REWRITE));
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
                         the refs it names, and kelpie checks only those: unset it, then push."
                    ));
                }
            }
        }
        Read::Commit { all } => {
            let range = if *all { "HEAD" } else { "--cached" };
            for file in home.added(&patches(&["diff", range])?) {
                out.push(home.refusal(&format!("this commit's {file}"), WRITE));
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
