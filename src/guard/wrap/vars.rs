//! What a variable the call sets does to git
//!
//! Some point git at another repo or config, some give it config in the
//! call's own text, and some hold a shell command git or gh runs. A name
//! the shell works out when it runs could be any of them.

use super::program;

// `GIT_` variables that change only how git shows or signs what it does.
// Any other can point git at another repo, index or config.
const GIT_HARMLESS: [&str; 12] = [
    "GIT_PAGER",
    "GIT_EDITOR",
    "GIT_SEQUENCE_EDITOR",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_COMMITTER_DATE",
    "GIT_TERMINAL_PROMPT",
    "GIT_MERGE_AUTOEDIT",
    "GIT_OPTIONAL_LOCKS",
];

// Variables whose value git or gh runs as a shell command.
const COMMAND_VARS: [&str; 16] = [
    "GIT_EDITOR",
    "GIT_SEQUENCE_EDITOR",
    "GIT_PAGER",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_PROXY_COMMAND",
    "GIT_ASKPASS",
    "GIT_EXTERNAL_DIFF",
    "SSH_ASKPASS",
    "EDITOR",
    "VISUAL",
    "PAGER",
    "GH_EDITOR",
    "GH_PAGER",
    "GH_BROWSER",
    "BROWSER",
];

/// Whether setting `name` can point git at another repo or config
pub(in crate::guard) fn redirects_git(name: &str) -> bool {
    name.starts_with("GIT_") && !GIT_HARMLESS.contains(&name)
        || matches!(name, "HOME" | "XDG_CONFIG_HOME")
}

/// Whether setting `name` gives every git command config from the command's own text
pub(in crate::guard) fn configures_git(name: &str) -> bool {
    matches!(
        name,
        "GIT_CONFIG_PARAMETERS" | "GIT_CONFIG_COUNT" | "GIT_ALLOW_PROTOCOL"
    ) || name.starts_with("GIT_CONFIG_KEY_")
        || name.starts_with("GIT_CONFIG_VALUE_")
}

/// Whether `words` set, for the commands after them, a variable `named`
/// picks out: an `export`, a `declare -x`, or an assignment alone, which
/// `set -a` would export
pub(in crate::guard) fn sets(words: &[String], named: fn(&str) -> bool) -> bool {
    // A name the shell works out when it runs could be any of them.
    let names = |words: &[String]| {
        words.iter().filter(|w| !w.starts_with('-')).any(|w| {
            let name = w.split_once('=').map_or(w.as_str(), |(n, _)| n);
            !is_name(name) || named(name)
        })
    };
    match words.first().map(|w| program(w)) {
        Some("export" | "declare" | "typeset" | "local" | "readonly") => names(&words[1..]),
        _ => words.iter().all(|w| assignment(w).is_some()) && names(words),
    }
}

/// The values `words` assign to a variable git or gh runs as a command
pub(in crate::guard) fn commands_assigned(words: &[String]) -> impl Iterator<Item = &str> {
    words
        .iter()
        .filter_map(|w| assignment(w))
        .filter(|(name, _)| COMMAND_VARS.contains(name))
        .map(|(_, value)| value)
}

pub(super) fn assignment(word: &str) -> Option<(&str, &str)> {
    let (name, value) = word.split_once('=')?;
    is_name(name).then_some((name, value))
}

// A shell variable's name, as written.
fn is_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}
