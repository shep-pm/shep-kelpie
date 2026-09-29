//! `git commit`, `git tag` and `git push` in a worker's Bash call
//!
//! Messages are read from the command. What a commit adds and a push sends
//! is read from the worktree's own git, once a call, through kelpie's
//! trusted worktree git. A commit or push anywhere else, pointed at another
//! repo or config, or a git command that is not one of git's own, is
//! refused: the guard can only vouch for the worktree as it stands.

use std::collections::HashMap;
use std::path::Path;

use super::{Checkout, Home, REWRITE, WRITE, files, moved, values};
use crate::worktree::{self, BASE, WorktreeError};

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
}

/// Judges one git command run in `cwd`
pub(super) fn judge(
    git: Git<'_>,
    cwd: Option<&Path>,
    home: Option<&Home>,
    checkout: Checkout<'_>,
    reads: &mut Reads,
) -> Vec<String> {
    let mut rest = git.words[1..].iter().map(String::as_str);
    let mut dir = cwd.map(Path::to_owned);
    let mut redirected = git.redirected;
    let mut configured = false;
    let sub = loop {
        match rest.next() {
            Some("-C") => dir = moved(dir.as_deref(), rest.next().unwrap_or_default(), home),
            Some("-c" | "--config-env") => {
                configured = true;
                rest.next();
            }
            Some(w) if w.starts_with("--config-env=") => configured = true,
            Some("--git-dir" | "--work-tree") => {
                redirected = true;
                rest.next();
            }
            Some(w) if w.starts_with("--git-dir=") || w.starts_with("--work-tree=") => {
                redirected = true;
            }
            Some("--namespace" | "--super-prefix") => {
                rest.next();
            }
            Some(w) if w.starts_with('-') => {}
            Some(w) => break w,
            None => return Vec::new(),
        }
    };
    // An alias or an external command could be anything, and no alias
    // shares a built-in's name.
    if !BUILTINS.contains(&sub) {
        return vec![format!(
            "kelpie runs only git's own commands, and `{}` is not one: run the command it \
             stands for.",
            sub.chars().take(40).collect::<String>()
        )];
    }
    let Some(home) = home else {
        return Vec::new();
    };
    let args: Vec<String> = rest.map(str::to_owned).collect();
    let mut out = Vec::new();
    if sub == "commit" || sub == "tag" {
        let messages = values(&args, &["--message"], &['m'])
            .into_iter()
            .chain(files(&args, &["--file"], &['F'], dir.as_deref()))
            .chain(git.heredocs.iter().cloned());
        if messages.into_iter().any(|m| home.is_in(&m)) {
            out.push(home.refusal(&format!("this {sub}'s message"), WRITE));
        }
    }
    if !matches!(sub, "commit" | "push") {
        return out;
    }
    if redirected || configured {
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
    let keys = if sub == "push" {
        match sources(&args) {
            Ok(sources) => sources.into_iter().map(Read::Push).collect(),
            Err(refusal) => {
                out.push(refusal);
                return out;
            }
        }
    } else {
        vec![Read::Commit {
            all: all_tracked(&args),
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

// Git's own commands a worker may run. `--version` and `--help` come as options.
const BUILTINS: [&str; 76] = [
    "add",
    "am",
    "annotate",
    "apply",
    "archive",
    "bisect",
    "blame",
    "branch",
    "bundle",
    "cat-file",
    "check-attr",
    "check-ignore",
    "check-ref-format",
    "checkout",
    "cherry",
    "cherry-pick",
    "clean",
    "clone",
    "commit",
    "commit-graph",
    "commit-tree",
    "config",
    "count-objects",
    "describe",
    "diff",
    "diff-files",
    "diff-index",
    "diff-tree",
    "difftool",
    "fetch",
    "for-each-ref",
    "format-patch",
    "fsck",
    "gc",
    "grep",
    "hash-object",
    "help",
    "init",
    "log",
    "ls-files",
    "ls-remote",
    "ls-tree",
    "merge",
    "merge-base",
    "merge-file",
    "merge-tree",
    "mergetool",
    "mv",
    "name-rev",
    "notes",
    "prune",
    "pull",
    "push",
    "range-diff",
    "read-tree",
    "rebase",
    "reflog",
    "remote",
    "repack",
    "replace",
    "reset",
    "restore",
    "rev-list",
    "rev-parse",
    "revert",
    "rm",
    "shortlog",
    "show",
    "show-ref",
    "stash",
    "status",
    "switch",
    "symbolic-ref",
    "tag",
    "update-ref",
    "worktree",
];

/// One read of the worktree's git, keyed so a call makes it once
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Read {
    /// What a commit adds: staged, or every tracked change with `all`
    Commit { all: bool },
    /// What a push of this source sends past `origin`'s base branch
    Push(String),
}

// What each of a push's refspecs sends from; `HEAD` when it names none.
fn sources(args: &[String]) -> Result<Vec<String>, String> {
    let mut positional = Vec::new();
    let mut words = args.iter();
    while let Some(word) = words.next() {
        match word.as_str() {
            "--" => positional.extend(words.by_ref().cloned()),
            "--tags" | "--follow-tags" | "--all" | "--mirror" => {
                return Err(format!(
                    "kelpie checks a push of commits only: push without `{word}`."
                ));
            }
            "-o" | "--push-option" | "--repo" | "--receive-pack" | "--exec" => {
                words.next();
            }
            w if w.starts_with('-') => {}
            w => positional.push(w.to_owned()),
        }
    }
    // The first is the remote.
    let mut out = Vec::new();
    for refspec in positional.iter().skip(1) {
        let refspec = refspec.strip_prefix('+').unwrap_or(refspec);
        let source = refspec.split_once(':').map_or(refspec, |(from, _)| from);
        if source.starts_with('-') {
            return Err("kelpie cannot read a push source that starts with `-`.".into());
        }
        // `:branch` deletes, and sends nothing.
        if !source.is_empty() {
            out.push(source.to_owned());
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
// made there. A folder the guard cannot follow is judged as the worktree; one
// that is not there when the hook runs, such as one the call will make, is not.
fn in_own_repo(worktree: &Path, dir: Option<&Path>) -> bool {
    let Ok(worktree) = worktree.canonicalize() else {
        return false;
    };
    let Some(dir) = dir else {
        return true;
    };
    let Ok(dir) = dir.canonicalize() else {
        return false;
    };
    dir.starts_with(&worktree)
        && dir
            .ancestors()
            .take_while(|a| *a != worktree)
            .all(|a| a.join(".git").symlink_metadata().is_err())
}
