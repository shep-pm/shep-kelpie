//! `git commit`, `git tag` and `git push` in a worker's Bash call
//!
//! Messages are read from the command. What a commit adds and a push sends
//! is read from the worktree's own git, once a call, through kelpie's
//! trusted worktree git. A commit or push anywhere else, or pointed at
//! another repo, is refused: the guard can only vouch for the worktree.

use std::collections::HashMap;
use std::path::Path;

use super::{Checkout, Home, REWRITE, WRITE, files, moved, values};
use crate::worktree::{self, BASE, WorktreeError};

/// The worktree reads one call has made, so none runs twice
#[derive(Debug, Default)]
pub(super) struct Reads(HashMap<(String, bool), Result<Vec<String>, String>>);

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
    let mut aliases: Vec<(&str, &str)> = Vec::new();
    let sub = loop {
        match rest.next() {
            Some("-C") => dir = moved(dir.as_deref(), rest.next().unwrap_or_default(), home),
            Some("-c") => {
                let set = rest.next().unwrap_or_default();
                if let Some((name, target)) =
                    set.strip_prefix("alias.").and_then(|a| a.split_once('='))
                {
                    aliases.push((name, target));
                }
            }
            Some("--git-dir" | "--work-tree") => {
                redirected = true;
                rest.next();
            }
            Some(w) if w.starts_with("--git-dir=") || w.starts_with("--work-tree=") => {
                redirected = true
            }
            Some("--namespace" | "--super-prefix" | "--config-env") => {
                if rest.next().is_some_and(|v| v.starts_with("alias.")) {
                    return vec![ALIAS.into()];
                }
            }
            Some(w) if w.starts_with("--config-env=alias.") => return vec![ALIAS.into()],
            Some(w) if w.starts_with('-') => {}
            Some(w) => break w,
            None => return Vec::new(),
        }
    };
    let mut args: Vec<&str> = rest.collect();
    let sub = match aliases.iter().rev().find(|(name, _)| *name == sub) {
        Some((_, target)) if target.starts_with('!') => return vec![ALIAS.into()],
        Some((_, target)) => {
            let mut words = target.split_whitespace();
            let sub = words.next().unwrap_or_default();
            args.splice(0..0, words);
            sub
        }
        None => sub,
    };
    let Some(home) = home else {
        return Vec::new();
    };
    let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
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
    if redirected {
        out.push(format!(
            "kelpie checks a {sub} only in this worktree's own git: run it without \
             `--git-dir`, `--work-tree` or a `GIT_DIR`-like variable."
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
    let all = sub == "commit" && all_tracked(&args);
    let found = reads
        .0
        .entry((sub.to_owned(), all))
        .or_insert_with(|| {
            worktree::trusted(checkout.git_common_dir, checkout.worktree)
                .and_then(|run| read(sub, all, home, run))
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
    out
}

const ALIAS: &str = "kelpie cannot check a git alias set on the command line: run the git \
                     command itself.";

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
    sub: &str,
    all: bool,
    home: &Home,
    run: impl Fn(&[&str]) -> Result<String, WorktreeError>,
) -> Result<Vec<String>, WorktreeError> {
    // `--unified` alone makes `git log` print patches, so only patch reads take these.
    let patches = |args: &[&str]| run(&[args, &PLAIN].concat());
    let mut out = Vec::new();
    if sub == "push" {
        // A file written and committed in one call is not staged when the
        // commit is judged, so the push reads what it sends. The base is
        // `origin`'s, which the worker cannot move, unlike its own branch's.
        let base = format!("refs/remotes/origin/{BASE}");
        let log = run(&["log", "--format=%B", "HEAD", "--not", &base])?;
        if home.is_in(&log) {
            out.push(home.refusal("a message in the commits this push sends", REWRITE));
        }
        let sent = patches(&["log", "-p", "--format=", "HEAD", "--not", &base])?;
        for file in home.added(&sent) {
            out.push(home.refusal(&format!("{file} in the commits this push sends"), REWRITE));
        }
        return Ok(out);
    }
    let range = if all { "HEAD" } else { "--cached" };
    for file in home.added(&patches(&["diff", range])?) {
        out.push(home.refusal(&format!("this commit's {file}"), WRITE));
    }
    Ok(out)
}

// Whether `dir` is the worker's own repo: in the worktree and in no repo it
// made there. A folder the guard cannot follow or see is judged as the worktree.
fn in_own_repo(worktree: &Path, dir: Option<&Path>) -> bool {
    let Ok(worktree) = worktree.canonicalize() else {
        return false;
    };
    let Some(Ok(dir)) = dir.map(Path::canonicalize) else {
        return true;
    };
    dir.starts_with(&worktree)
        && dir
            .ancestors()
            .take_while(|a| *a != worktree)
            .all(|a| a.join(".git").symlink_metadata().is_err())
}
