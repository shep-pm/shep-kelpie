//! `gh` in a worker's Bash call
//!
//! A pull request's title must be a conventional commit, and no title, body
//! or notes may carry what [`LocalPaths`](crate::local_paths::LocalPaths) finds of this machine's. What only the project manager
//! does is refused whatever the command's shape: merging, marking ready and
//! summoning a review, with `gh api` and `gh auth`, which reach around them.
//! An alias or an extension could stand for any of those, so only gh's own
//! commands run.

use std::path::Path;

use super::{Home, WRITE, files, values};
use crate::coderabbit::LABEL;

/// The conventional commit types a pull request title may start with
const TYPES: [&str; 11] = [
    "feat", "fix", "docs", "style", "refactor", "perf", "test", "build", "ci", "chore", "revert",
];

// gh's own commands and their short names, besides `api`, `auth`, `alias`,
// `co` and `extension`, which are refused by name.
const COMMANDS: [&str; 31] = [
    "agent-task",
    "attestation",
    "browse",
    "cache",
    "codespace",
    "completion",
    "config",
    "copilot",
    "cs",
    "discussion",
    "gist",
    "gpg-key",
    "help",
    "issue",
    "label",
    "licenses",
    "org",
    "pr",
    "preview",
    "project",
    "release",
    "repo",
    "ruleset",
    "run",
    "search",
    "secret",
    "skill",
    "ssh-key",
    "status",
    "variable",
    "workflow",
];

const MANAGER_ONLY: &str = "only the project manager merges a pull request, marks one ready \
                            or summons a review, on the maintainer's ruling: leave your pull \
                            request as a draft.";

/// Judges one gh command run in `cwd`
pub(super) fn judge(
    words: &[String],
    heredocs: &[String],
    cwd: Option<&Path>,
    home: &Home,
) -> Vec<String> {
    // The group and verb are the first two words that are not flags, which
    // may come before them: `gh pr -R owner/repo create`.
    let mut positions = Vec::new();
    let mut i = 1;
    while i < words.len() && positions.len() < 2 {
        match words[i].as_str() {
            "-R" | "--repo" => i += 1,
            w if w.starts_with('-') => {}
            _ => positions.push(i),
        }
        i += 1;
    }
    let Some(&group) = positions.first() else {
        return Vec::new();
    };
    let group = words[group].as_str();
    let verb = positions.get(1).map(|&v| match words[v].as_str() {
        "new" => "create",
        verb => verb,
    });
    let args = &words[i.min(words.len())..];
    match (group, verb) {
        ("api", _) => {
            return vec![
                "kelpie runs no `gh api` for a worker: it reaches around what only the \
                 project manager does. Use gh's own commands."
                    .into(),
            ];
        }
        ("auth", _) => {
            return vec!["`gh auth` handles the maintainer's token, which no worker needs.".into()];
        }
        ("pr", Some("merge" | "ready")) => return vec![MANAGER_ONLY.into()],
        ("alias" | "co" | "extension" | "extensions" | "ext", _) => {
            return vec![
                "kelpie runs no gh alias or extension: run the gh command it stands for.".into(),
            ];
        }
        (group, _) if !COMMANDS.contains(&group) => {
            return vec![format!(
                "kelpie runs only gh's own commands, and `{}` is not one: run the command it \
                 stands for.",
                group.chars().take(40).collect::<String>()
            )];
        }
        _ => {}
    }
    // Labels come comma-separated, and GitHub matches their names in any case.
    let summons = values(args, &["--add-label", "--label"], &['l'])
        .iter()
        .flat_map(|v| v.split(','))
        .any(|label| {
            label
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .eq_ignore_ascii_case(LABEL)
        });
    if summons {
        return vec![MANAGER_ONLY.into()];
    }
    let Some(verb) = verb else {
        return Vec::new();
    };
    let publishes = matches!(
        (group, verb),
        ("pr", "create" | "edit" | "comment" | "review")
            | ("issue", "create" | "edit" | "comment")
            | ("release", "create" | "edit")
    );
    if !publishes {
        return Vec::new();
    }
    let titles = values(args, &["--title"], &['t']);
    let mut out = Vec::new();
    if group == "pr" && verb == "create" && titles.is_empty() {
        out.push(
            "`gh pr create` needs `--title`: without one, GitHub titles a pull request of \
             several commits with the branch's name. Give it a conventional commit subject, \
             such as `fix(parser): keep the last line`."
                .to_owned(),
        );
    }
    // Not echoed: a title can carry a path too.
    if group == "pr" && !titles.iter().all(|t| conventional(t)) {
        out.push(format!(
            "this pull request's title is not a conventional commit. Write it as \
             `type(scope): summary`, the scope optional, with the type one of {}, and \
             `!` after the type or scope for a breaking change.",
            TYPES.join(", ")
        ));
    }
    let texts = titles
        .into_iter()
        .chain(values(args, &["--body", "--notes"], &['b', 'n']))
        .chain(files(args, &["--body-file", "--notes-file"], &['F'], cwd))
        .chain(heredocs.iter().cloned());
    if let Some(leak) = home.find_in_prose(texts) {
        out.push(home.refusal(&format!("this `gh {group} {verb}`"), leak, WRITE));
    }
    out
}

/// Whether `title` is a conventional commit subject
fn conventional(title: &str) -> bool {
    let Some((head, summary)) = title.split_once(": ") else {
        return false;
    };
    let head = head.strip_suffix('!').unwrap_or(head);
    let kind = match head.split_once('(') {
        Some((kind, scope)) => {
            let Some(scope) = scope.strip_suffix(')') else {
                return false;
            };
            if scope.is_empty() || scope.contains(['(', ')', '\n']) {
                return false;
            }
            kind
        }
        None => head,
    };
    TYPES.contains(&kind) && !summary.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every command `gh help` lists in gh 2.96.0, `co` its one alias.
    const GH_HELP: [&str; 35] = [
        "auth",
        "browse",
        "codespace",
        "discussion",
        "gist",
        "issue",
        "org",
        "pr",
        "project",
        "release",
        "repo",
        "skill",
        "cache",
        "run",
        "workflow",
        "co",
        "agent-task",
        "alias",
        "api",
        "attestation",
        "completion",
        "config",
        "copilot",
        "extension",
        "gpg-key",
        "label",
        "licenses",
        "preview",
        "ruleset",
        "search",
        "secret",
        "ssh-key",
        "status",
        "variable",
        "help",
    ];

    // A hook that was given no folders and no names.
    fn nobody() -> Home {
        Home::new(None, crate::local_paths::LocalPaths::default())
    }

    #[test]
    fn every_command_gh_lists_is_known_by_name() {
        for name in GH_HELP {
            let found = judge(
                &["gh", name, "list"].map(str::to_owned),
                &[],
                None,
                &nobody(),
            );
            assert!(
                found.iter().all(|f| !f.contains("not one")),
                "{name}: {found:?}"
            );
        }
        let found = judge(
            &["gh", "workflow", "view", "ci"].map(str::to_owned),
            &[],
            None,
            &nobody(),
        );
        assert!(found.is_empty(), "{found:?}");
    }
}
