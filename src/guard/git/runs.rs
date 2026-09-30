//! What git runs besides itself, as a git command's text or config gives it
//!
//! Some options hand git a shell command: `rebase --exec`, `difftool -x`,
//! `--upload-pack`. The guard reads each as a script. Config set in the
//! command is read from a list of keys that run nothing, and config that has
//! a push send more than the refs it names is refused.

use super::front::front;
use crate::guard::values;

/// The shell commands a git command's own options give it to run
pub(in crate::guard) fn scripts(words: &[String]) -> Vec<String> {
    let Ok(Some(at)) = front(words).map(|f| f.sub) else {
        return Vec::new();
    };
    let args = &words[at + 1..];
    let (long, short): (&[&str], &[char]) = match words[at].as_str() {
        "grep" => return pager(args),
        "rebase" => (&["--exec"], &['x']),
        "difftool" => (&["--extcmd"], &['x']),
        "clone" => (&["--upload-pack"], &['u']),
        "fetch" | "pull" => (&["--upload-pack"], &[]),
        "ls-remote" | "archive" => (&["--upload-pack", "--exec"], &[]),
        _ => return Vec::new(),
    };
    // Git takes any start of a long option that names only one.
    let long: Vec<&str> = long
        .iter()
        .flat_map(|name| (3..=name.len()).map(|n| &name[..n]))
        .collect();
    values(args, &long, short)
}

// `git grep -O<pager>`: its value is optional, so only one joined to the option.
fn pager(args: &[String]) -> Vec<String> {
    let long = "--open-files-in-pager";
    args.iter()
        .take_while(|a| *a != "--")
        .filter_map(|a| match a.split_once('=') {
            Some((name, value)) if name.len() >= 3 && long.starts_with(name) => {
                Some(value.to_owned())
            }
            _ if a.starts_with('-') && !a.starts_with("--") => {
                let (_, value) = a.split_once('O')?;
                (!value.is_empty()).then(|| value.to_owned())
            }
            _ => None,
        })
        .collect()
}

/// The command `git bisect run` runs
pub(in crate::guard) fn bisect_run(words: &[String]) -> Option<&[String]> {
    let at = front(words).ok()?.sub?;
    (words[at] == "bisect" && words.get(at + 1)? == "run").then(|| &words[at + 2..])
}

/// Whether a git command makes a repo: `init`, `clone` or `worktree add`
pub(in crate::guard) fn makes_repo(words: &[String]) -> bool {
    let Ok(Some(at)) = front(words).map(|f| f.sub) else {
        return false;
    };
    match words[at].as_str() {
        "init" | "clone" => true,
        "worktree" => words[at + 1..].iter().any(|w| w == "add"),
        _ => false,
    }
}

// Config sections, and keys, that name no program for git to run.
const SAFE_SECTIONS: [&str; 15] = [
    "advice", "blame", "branch", "checkout", "color", "column", "fetch", "grep", "i18n", "log",
    "pull", "rebase", "safe", "status", "user",
];
const SAFE_KEYS: [&str; 14] = [
    "core.abbrev",
    "core.autocrlf",
    "core.filemode",
    "core.ignorecase",
    "core.precomposeunicode",
    "core.quotepath",
    "core.safecrlf",
    "diff.algorithm",
    "diff.colormoved",
    "diff.mnemonicprefix",
    "diff.noprefix",
    "diff.renames",
    "init.defaultbranch",
    "merge.conflictstyle",
];

/// A `-c` or `--config-env` setting's key
pub(super) fn key(setting: &str) -> &str {
    setting.split_once('=').map_or(setting, |(key, _)| key)
}

/// Whether setting `key` leaves git running only itself
pub(super) fn safe(key: &str) -> bool {
    let key = key.to_lowercase();
    let section = key.split('.').next().unwrap_or_default();
    key.contains('.') && SAFE_SECTIONS.contains(&section) || SAFE_KEYS.contains(&key.as_str())
}

/// Whether config `key`, set to `value`, has a push send more than the refs it names
pub(super) fn pushes_more(key: &str, value: &str) -> bool {
    let on = !matches!(value, "false" | "no" | "off" | "0");
    match key {
        "push.default" => value == "matching",
        "push.followtags" => on,
        "push.recursesubmodules" => on && value != "check",
        _ => {
            key.starts_with("remote.") && (key.ends_with(".push") || key.ends_with(".mirror") && on)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(words: &str) -> Vec<String> {
        words.split(' ').map(str::to_owned).collect()
    }

    #[test]
    fn options_that_run_a_command_are_read_in_every_spelling() {
        for line in [
            "git rebase --exec X main",
            "git rebase --exec=X main",
            "git rebase --exe X main",
            "git rebase -x X main",
            "git rebase -ixX main",
            "git -C d difftool -y -x X",
            "git clone -u X a b",
            "git fetch --upload-pack=X",
            "git archive --remote=. --exec X",
            "git grep -OX a",
            "git grep -iOX a",
            "git grep --open-files-in-pager=X a",
            "git grep --open=X a",
        ] {
            assert_eq!(scripts(&w(line)), ["X"], "{line}");
        }
        assert!(scripts(&w("git rebase main -- --exec X")).is_empty());
        assert!(scripts(&w("git log --exec X")).is_empty());
        assert!(scripts(&w("git grep -O a")).is_empty());
    }

    #[test]
    fn config_keys_that_run_nothing_are_known() {
        for key in [
            "color.ui",
            "Core.QuotePath",
            "advice.detachedHead",
            "user.name",
        ] {
            assert!(safe(key), "{key}");
        }
        for key in [
            "core.fsmonitor",
            "core.hooksPath",
            "alias.p",
            "include.path",
            "color",
            "",
        ] {
            assert!(!safe(key), "{key}");
        }
    }
}
