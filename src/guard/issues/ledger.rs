//! What the issue writer's session has filed and read, so it edits and links
//! only its own issues
//!
//! A PostToolUse hook, `kelpie guard ... --record`, writes it after each of
//! the session's commands: the issues a `gh issue create` printed, and the
//! id a `gh api 'repos/{owner}/{repo}/issues/<n>' --jq .id` printed. The
//! PreToolUse guard reads it. A ledger that is missing or cannot be read
//! holds nothing, so an edit or a link is refused rather than allowed.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{IssueRules, cats_feed_bodies, shell};

/// The issues one session filed, and the ids it read for this repo's issues
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Ledger {
    /// Each issue's number, as `gh issue create` printed it
    filed: Vec<u64>,
    /// Each issue's database id, by its number, as `gh api ... --jq .id` printed it
    ids: BTreeMap<u64, u64>,
}

impl Ledger {
    /// The ledger at `path`, empty when there is none or it cannot be read
    pub(super) fn read(path: Option<&Path>) -> Self {
        path.and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Whether this session filed issue `number`
    pub(super) fn filed(&self, number: u64) -> bool {
        self.filed.contains(&number)
    }

    /// Whether `id` is the id of an issue this session filed
    pub(super) fn filed_id(&self, id: u64) -> bool {
        self.ids
            .iter()
            .any(|(number, read)| *read == id && self.filed(*number))
    }

    /// Whether `id` is one this session read for an issue of this repo
    pub(super) fn read_id(&self, id: u64) -> bool {
        self.ids.values().any(|read| *read == id)
    }
}

/// Records what the issue writer's finished command in `input`, a
/// PostToolUse hook's input, filed or read, in the ledger `rules` name
///
/// Only a line that is one lone `gh issue create`, with at most the `cat`
/// its `--body` reads, adds an issue, and only when it printed one URL and
/// nothing else. Only one lone `gh api` read of an issue with `--jq .id`
/// adds the id it printed. Anything else changes nothing.
pub fn record_issues(input: impl Read, rules: &IssueRules) {
    let Some(path) = &rules.ledger else {
        return;
    };
    let Ok(call) = serde_json::from_reader::<_, Value>(input) else {
        return;
    };
    let (Some("Bash"), Some(line)) = (
        call["tool_name"].as_str(),
        call["tool_input"]["command"].as_str(),
    ) else {
        return;
    };
    let response = &call["tool_response"];
    let Some(stdout) = response["stdout"].as_str().or(response.as_str()) else {
        return;
    };
    let Ok(commands) = shell::commands(line) else {
        return;
    };
    // One lone `gh` command, and no `cat` but the one its body reads.
    if !cats_feed_bodies(&commands) {
        return;
    }
    let (cats, others): (Vec<&shell::Command>, Vec<&shell::Command>) =
        commands.iter().partition(|c| c.words[0] == "cat");
    let [gh] = others.as_slice() else {
        return;
    };
    let Some(words) = (gh.words[0] == "gh").then(|| &gh.words[1..]) else {
        return;
    };
    let printed: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut ledger = Ledger::read(Some(path));
    let creates = matches!(words, [i, c, ..] if i == "issue" && (c == "create" || c == "new"));
    match (creates, printed.as_slice()) {
        // gh prints the new issue's URL alone; anything more is not trusted.
        (true, [line]) => match issue_url(line) {
            Some(number) => ledger.filed.push(number),
            None => return,
        },
        (false, [line]) if cats.is_empty() => {
            match (id_read(words), line.trim().parse::<u64>().ok()) {
                (Some(number), Some(id)) => {
                    ledger.ids.insert(number, id);
                }
                _ => return,
            }
        }
        _ => return,
    }
    // Best effort: a ledger that is not written refuses the edit or link that needs it.
    if let Ok(text) = serde_json::to_vec(&ledger) {
        let _ = std::fs::write(path, text);
    }
}

// The number in a line that is an issue's URL on GitHub, and nothing else.
fn issue_url(line: &str) -> Option<u64> {
    let rest = line.trim().strip_prefix("https://github.com/")?;
    let mut parts = rest.split('/');
    let (_owner, _repo, issues, number) =
        (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    (issues == "issues" && parts.next().is_none()).then_some(())?;
    number.parse().ok()
}

// The issue whose id `gh api 'repos/{owner}/{repo}/issues/<n>' --jq .id` reads.
fn id_read(words: &[String]) -> Option<u64> {
    let [api, endpoint, jq, filter] = words else {
        return None;
    };
    let number = (endpoint.trim_start_matches('/'))
        .strip_prefix("repos/{owner}/{repo}/issues/")?
        .parse()
        .ok()?;
    (api == "api" && (jq == "--jq" || jq == "-q") && filter == ".id").then_some(number)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn rules(ledger: &Path) -> IssueRules {
        IssueRules {
            status: "ready-for-human".into(),
            agents: vec!["sonnet-high".into()],
            ledger: Some(ledger.to_owned()),
        }
    }

    fn ran(rules: &IssueRules, command: &str, stdout: &str) {
        let call = json!({
            "tool_name": "Bash",
            "tool_input": { "command": command },
            "tool_response": { "stdout": stdout, "stderr": "", "interrupted": false },
        });
        record_issues(call.to_string().as_bytes(), rules);
    }

    #[test]
    fn it_records_the_issues_a_create_printed_and_the_ids_a_read_printed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.json");
        let rules = rules(&path);
        ran(
            &rules,
            "gh issue create --title x --body-file - <<'EOF'\nbody\nEOF",
            "https://github.com/acme/shep/issues/41\n",
        );
        ran(
            &rules,
            "gh api 'repos/{owner}/{repo}/issues/41' --jq .id",
            "9041\n",
        );
        ran(
            &rules,
            "gh api 'repos/{owner}/{repo}/issues/7' --jq .id",
            "9007\n",
        );
        // An issue a read printed, a read of another field, and a URL a
        // view printed are none of the session's.
        ran(
            &rules,
            "gh issue view 7",
            "see https://github.com/acme/shep/issues/7\n",
        );
        ran(
            &rules,
            "gh issue view 8",
            "https://github.com/acme/shep/issues/8\n",
        );
        ran(
            &rules,
            "gh api 'repos/{owner}/{repo}/issues/8' --jq .number",
            "8\n",
        );
        let ledger = Ledger::read(Some(&path));
        assert!(ledger.filed(41));
        assert!(!ledger.filed(7) && !ledger.filed(8));
        assert!(ledger.filed_id(9041) && !ledger.filed_id(9007));
        assert!(ledger.read_id(9007) && !ledger.read_id(8));
    }

    #[test]
    fn only_one_lone_create_printing_one_url_is_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.json");
        let rules = rules(&path);
        let create = "gh issue create --title x --body-file - <<'EOF'\nbody\nEOF";
        let url = |n: u64| format!("https://github.com/acme/shep/issues/{n}");
        // Forged: a `cat` of its own, a second create, or more than one URL.
        let cat = format!("{create}\ncat <<'EOF'\n{}\nEOF", url(7));
        ran(&rules, &cat, &format!("{}\n{}\n", url(41), url(7)));
        ran(
            &rules,
            &format!("{create}\n{create}"),
            &format!("{}\n{}\n", url(42), url(8)),
        );
        ran(&rules, create, &format!("{}\n{}\n", url(43), url(9)));
        ran(&rules, create, &format!("see {}\n", url(10)));
        let read = "cat <<'EOF'\n9007\nEOF\ngh api 'repos/{owner}/{repo}/issues/7' --jq .id";
        ran(&rules, read, "9007\n");
        assert_eq!(Ledger::read(Some(&path)), Ledger::default());
        // The body's own `cat` is the one a create may carry.
        let body = "gh issue create --title x --body \"$(cat <<'EOF'\nbody\nEOF\n)\"";
        ran(&rules, body, &format!("{}\n", url(44)));
        assert!(Ledger::read(Some(&path)).filed(44));
    }

    #[test]
    fn a_ledger_that_is_not_there_holds_nothing() {
        let ledger = Ledger::read(Some(Path::new("/nowhere/ledger.json")));
        assert_eq!(ledger, Ledger::default());
        assert_eq!(Ledger::read(None), Ledger::default());
    }
}
