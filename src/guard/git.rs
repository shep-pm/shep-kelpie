//! `git commit`, `git tag` and `git push` in a worker's Bash call
//!
//! Messages are read from the command. What a commit adds and a push sends
//! is read from the worktree's own git, once a call, through kelpie's
//! trusted worktree git. A commit or push anywhere else, in a folder the
//! guard cannot follow, or pointed at another repo or config is refused, as
//! is any git command, option or push form it does not know: the guard can
//! only vouch for the worktree as it stands. A push to the base branch is
//! refused wherever it runs, since only the project manager changes it.

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
            Some("--git-dir" | "--work-tree") => {
                redirected = true;
                rest.next();
            }
            Some("--bare") => redirected = true,
            Some("--namespace" | "--super-prefix" | "--attr-source") => {
                rest.next();
            }
            Some(w) if w.starts_with("--") && w.contains('=') => {
                match w.split_once('=').map_or(w, |(name, _)| name) {
                    "--git-dir" | "--work-tree" => redirected = true,
                    "--config-env" | "--exec-path" => configured = true,
                    "--namespace" | "--super-prefix" | "--attr-source" | "--list-cmds" => {}
                    _ => return vec![unknown_option(w)],
                }
            }
            Some(w) if GIT_FLAGS.contains(&w) => {}
            // These print and run no command.
            Some(
                "--version" | "-v" | "--help" | "-h" | "--html-path" | "--man-path" | "--info-path"
                | "--exec-path",
            ) => return Vec::new(),
            Some(w) if w.starts_with('-') => return vec![unknown_option(w)],
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
    let args: Vec<String> = rest.map(str::to_owned).collect();
    // Where a push goes is judged wherever it runs from.
    let pushed = if sub == "push" {
        match refspecs(&args) {
            Ok(specs) if specs.iter().any(Refspec::to_base) => return vec![to_base()],
            Ok(specs) => specs,
            Err(refusal) => return vec![refusal],
        }
    } else {
        Vec::new()
    };
    let mut out = Vec::new();
    if let Some(home) = home
        && (sub == "commit" || sub == "tag")
    {
        let messages = values(&args, &["--message"], &['m'])
            .into_iter()
            .chain(files(&args, &["--file"], &['F'], dir.as_deref()))
            .chain(git.heredocs.iter().cloned());
        if messages.into_iter().any(|m| home.is_in(&m)) {
            out.push(home.refusal(&format!("this {sub}'s message"), WRITE));
        }
    }
    // A commit is read only for the home folder; a push, for its branch too.
    if !(sub == "push" || sub == "commit" && home.is_some()) {
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
        let current = pushed.iter().any(Refspec::sends_current_branch);
        let sent = pushed
            .into_iter()
            .filter(|spec| home.is_some() && !spec.source.is_empty())
            .map(|spec| Read::Push(spec.source));
        current
            .then_some(Read::Branch)
            .into_iter()
            .chain(sent)
            .collect()
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

// Git's options before its command that take no value.
const GIT_FLAGS: [&str; 12] = [
    "-p",
    "--paginate",
    "-P",
    "--no-pager",
    "--no-replace-objects",
    "--no-lazy-fetch",
    "--no-optional-locks",
    "--no-advice",
    "--literal-pathspecs",
    "--glob-pathspecs",
    "--noglob-pathspecs",
    "--icase-pathspecs",
];

// An option the guard does not know could take the word after it, which it
// would then read as the command.
fn unknown_option(option: &str) -> String {
    format!(
        "kelpie cannot read the git option `{}`: run git without it.",
        option.chars().take(40).collect::<String>()
    )
}

// Git's own commands a worker may run. `--version` and `--help` come as options.
const BUILTINS: [&str; 75] = [
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
    /// The branch checked out, where a push of `HEAD` alone goes
    Branch,
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

// What a commit adds, or a push sends, that names the home folder, and a
// push of `HEAD` from the base branch.
fn read(
    key: &Read,
    home: Option<&Home>,
    run: impl Fn(&[&str]) -> Result<String, WorktreeError>,
) -> Result<Vec<String>, WorktreeError> {
    // `--unified` alone makes `git log` print patches, so only patch reads take these.
    let patches = |args: &[&str]| run(&[args, &PLAIN].concat());
    let mut out = Vec::new();
    match (key, home) {
        (Read::Branch, _) => {
            let branch = run(&["rev-parse", "--symbolic-full-name", "HEAD"])?;
            if branch.trim() == format!("refs/heads/{BASE}") {
                out.push(to_base());
            }
        }
        (_, None) => {}
        (Read::Push(source), Some(home)) => {
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
        (Read::Commit { all }, Some(home)) => {
            let range = if *all { "HEAD" } else { "--cached" };
            for file in home.added(&patches(&["diff", range])?) {
                out.push(home.refusal(&format!("this commit's {file}"), WRITE));
            }
        }
    }
    Ok(out)
}

// Whether `dir` is the worker's own repo: in the worktree and in no repo it
// made there. A folder the guard cannot follow (`None`), or one that is not
// there when the hook runs, such as one the call will make, is not.
fn in_own_repo(worktree: &Path, dir: Option<&Path>) -> bool {
    let (Ok(worktree), Some(Ok(dir))) = (worktree.canonicalize(), dir.map(Path::canonicalize))
    else {
        return false;
    };
    dir.starts_with(&worktree)
        && dir
            .ancestors()
            .take_while(|a| *a != worktree)
            .all(|a| a.join(".git").symlink_metadata().is_err())
}
