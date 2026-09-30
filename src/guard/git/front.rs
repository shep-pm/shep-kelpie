//! Git's own options in front of its command, and which command it is
//!
//! An option the guard does not know could take the word after it, which it
//! would then read as the command, so it is refused. So is a command that is
//! not one of git's own, since an alias could run anything.

// Git's own options, in front of its command.
#[derive(Debug, Default)]
pub(super) struct Front<'a> {
    // The command's place in the words, when there is one to run.
    pub sub: Option<usize>,
    // Where each `-C` moves it, in order.
    pub moves: Vec<&'a str>,
    // Whether `--git-dir`, `--work-tree` or `--bare` points it at another repo.
    pub redirected: bool,
    // Whether `-c`, `--config-env` or `--exec-path` changes its config.
    pub configured: bool,
    // Each `-c` and `--config-env` setting, as `key=value`.
    pub config: Vec<&'a str>,
}

// Git's options in front of its command, and where that command is.
pub(super) fn front(words: &[String]) -> Result<Front<'_>, String> {
    let mut front = Front::default();
    let mut i = 1;
    while let Some(word) = words.get(i) {
        let next = words.get(i + 1).map_or("", String::as_str);
        i += 1;
        match word.as_str() {
            "-C" => {
                front.moves.push(next);
                i += 1;
            }
            "-c" | "--config-env" => {
                front.configured = true;
                front.config.push(next);
                i += 1;
            }
            "--git-dir" | "--work-tree" => {
                front.redirected = true;
                i += 1;
            }
            "--bare" => front.redirected = true,
            "--namespace" | "--super-prefix" | "--attr-source" => i += 1,
            w if w.starts_with("--") && w.contains('=') => {
                let (name, value) = w.split_once('=').unwrap_or((w, ""));
                match name {
                    "--git-dir" | "--work-tree" => front.redirected = true,
                    "--config-env" => {
                        front.configured = true;
                        front.config.push(value);
                    }
                    "--exec-path" => front.configured = true,
                    "--namespace" | "--super-prefix" | "--attr-source" | "--list-cmds" => {}
                    _ => return Err(unknown_option(w)),
                }
            }
            w if GIT_FLAGS.contains(&w) => {}
            // These print and run no command.
            "--version" | "-v" | "--help" | "-h" | "--html-path" | "--man-path" | "--info-path"
            | "--exec-path" => return Ok(front),
            w if w.starts_with('-') => return Err(unknown_option(w)),
            _ => {
                front.sub = Some(i - 1);
                return Ok(front);
            }
        }
    }
    Ok(front)
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
pub(super) const BUILTINS: [&str; 77] = [
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
    "sparse-checkout",
    "stash",
    "status",
    "switch",
    "symbolic-ref",
    "tag",
    "update-ref",
    "var",
    "worktree",
];
