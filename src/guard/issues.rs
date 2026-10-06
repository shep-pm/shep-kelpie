//! `kelpie guard --issues=<label>`: the issue writer's commands, from a short list
//!
//! A worker's guard refuses what it knows to be wrong. The issue writer
//! only reads the repo and files issues, so its guard runs only what it
//! knows to be right: `gh issue create`, `gh issue edit` on labels, `gh
//! issue view` and `list` on this repo, and `gh api` on an issue's id and
//! its sub-issue and blocked-by links, each as plain words. It runs no git:
//! the session reads the repo with Read, Grep and Glob. Every issue it
//! files carries the status label kelpie names, exactly one `agent:` label
//! naming a listed implementer, and an acceptance criteria section, and no
//! title or body names this machine. It edits and links only the issues
//! the session filed, as its ledger records them.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::shell::{self, Command};
use super::wrap::FUNCTION;
use super::{Home, MAX_COMMANDS};
use crate::confine::Verdict;
use crate::local_paths::LocalPaths;
use gh::Gh;
use ledger::Ledger;

mod gh;
mod ledger;

pub use ledger::record_issues;

/// How the hook's command line names the status label the issue writer files with
pub const ISSUES_FLAG: &str = "--issues=";

/// How the hook's command line names one implementer an `agent:` label may name
pub const AGENT_FLAG: &str = "--agent=";

/// How the hook's command line names the session's ledger
pub const LEDGER_FLAG: &str = "--ledger=";

/// How the PostToolUse hook's command line asks for a finished command to be
/// recorded in the ledger, rather than judged
pub const RECORD_FLAG: &str = "--record";

/// What the issue writer may file
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRules {
    /// The status label every issue it files carries
    pub status: String,
    /// The implementers an `agent:` label may name
    pub agents: Vec<String>,
    /// The file that records what the session filed and read. Without one
    /// no edit or link is allowed.
    pub ledger: Option<PathBuf>,
}

impl IssueRules {
    /// The hook's command-line flags that carry these rules
    pub fn flags(&self) -> Vec<String> {
        let agents = self.agents.iter().map(|a| format!("{AGENT_FLAG}{a}"));
        let ledger = (self.ledger.iter()).map(|l| format!("{LEDGER_FLAG}{}", l.display()));
        [format!("{ISSUES_FLAG}{}", self.status)]
            .into_iter()
            .chain(agents)
            .chain(ledger)
            .collect()
    }
}

/// The issue writer's rules, when `args` name them, and the rest of `args`
///
/// `--agent=` or `--ledger=` with no `--issues=` is not taken, so the rest
/// refuses it.
pub fn issue_rules(args: &[String]) -> (Option<IssueRules>, Vec<String>) {
    let status = args.iter().find_map(|a| a.strip_prefix(ISSUES_FLAG));
    let Some(status) = status else {
        return (None, args.to_vec());
    };
    let agents = args.iter().filter_map(|a| a.strip_prefix(AGENT_FLAG));
    let rules = IssueRules {
        status: status.to_owned(),
        agents: agents.map(str::to_owned).collect(),
        ledger: (args.iter())
            .find_map(|a| a.strip_prefix(LEDGER_FLAG))
            .map(PathBuf::from),
    };
    let ours = |a: &str| {
        [ISSUES_FLAG, AGENT_FLAG, LEDGER_FLAG]
            .iter()
            .any(|f| a.starts_with(f))
    };
    let rest = args.iter().filter(|a| !ours(a)).cloned().collect();
    (Some(rules), rest)
}

const ONLY: &str = "the issue writer runs only these commands, each as plain words: \
    `gh issue create`, `gh issue edit` with `--add-label` or `--remove-label`, \
    `gh issue view <n>`, `gh issue list`, and `gh api` on `repos/{owner}/{repo}/issues/<n>`, \
    its `sub_issues` and its `dependencies/blocked_by`. It runs no git: read the repo with \
    Read, Grep and Glob";

const PLAIN: &str = "kelpie runs the issue writer's commands only as plain words: no `$`, \
    backticks, pipes, redirects or variables. Write each value out, and give a body as \
    `--body-file - <<'EOF'`";

/// Judges the issue writer's tool call in `input`, with `home` the home
/// folder and `local` what the project keeps off the forge
pub fn judge_issues(
    input: impl Read,
    home: Option<&Path>,
    local: LocalPaths,
    rules: &IssueRules,
) -> Verdict {
    #[derive(Deserialize)]
    struct Call {
        tool_name: String,
        #[serde(default)]
        tool_input: ToolInput,
    }
    #[derive(Deserialize, Default)]
    struct ToolInput {
        command: Option<String>,
    }
    let call: Call = match serde_json::from_reader(input) {
        Ok(call) => call,
        Err(e) => return Verdict::Refuse(format!("kelpie cannot read this tool call: {e}")),
    };
    match call.tool_name.as_str() {
        "Agent" | "Task" => {
            return Verdict::Refuse(
                "the issue writer runs no sub-agent: research it yourself".into(),
            );
        }
        "Bash" => {}
        _ => return Verdict::Allow,
    }
    let line = call.tool_input.command.unwrap_or_default();
    if unquoted_heredoc(&line) {
        return Verdict::Refuse(
            "quote a heredoc's delimiter, as `<<'EOF'`, so the shell leaves its body as \
             written."
                .into(),
        );
    }
    let commands = match shell::commands(&line) {
        Ok(commands) if commands.len() <= MAX_COMMANDS => commands,
        Ok(_) => return Verdict::Refuse(format!("{ONLY}.")),
        Err(e) => return Verdict::Refuse(format!("kelpie cannot check this command: {e}.")),
    };
    if !cats_feed_bodies(&commands) {
        return Verdict::Refuse(format!(
            "{ONLY}. A `cat` runs only as the body of `gh issue create --body \"$(cat \
             <<'EOF' ...)\"`."
        ));
    }
    let home = Home::new(home, local);
    let ledger = Ledger::read(rules.ledger.as_deref());
    let mut refusals: Vec<String> = Vec::new();
    for command in &commands {
        let gh = Gh {
            heredocs: &command.heredocs,
            rules,
            home: &home,
            ledger: &ledger,
        };
        if let Err(why) = judge(command, &gh)
            && !refusals.contains(&why)
        {
            refusals.push(why);
        }
    }
    match refusals.is_empty() {
        true => Verdict::Allow,
        false => Verdict::Refuse(refusals.join("\n\n")),
    }
}

fn judge(command: &Command, gh: &Gh<'_>) -> Result<(), String> {
    if command.defines_function {
        return Err(FUNCTION.into());
    }
    let words = &command.words;
    if command.piped || words.iter().any(|w| redirects(w)) {
        return Err(format!("{PLAIN}."));
    }
    match words[0].as_str() {
        // `--body "$(cat <<'EOF' ... EOF)"` reads its heredoc this way.
        "cat" if is_heredoc(&words[1..]) && !command.heredocs.is_empty() => Ok(()),
        "gh" => gh.judge(&words[1..]),
        _ => Err(format!("{ONLY}.")),
    }
}

// A word that sends output to a file or reads one, as the shell splits it.
fn redirects(word: &str) -> bool {
    let bare = word.trim_start_matches(|c: char| c.is_ascii_digit() || c == '&');
    !matches!(word, "<<" | "<<-") && (bare.starts_with(['>', '<']) || bare.ends_with('>'))
}

// A heredoc whose delimiter is bare, whose body the shell expands: `$(...)`
// there would run, and `$VAR` would put a variable's value in the issue.
// A `<<` in other text is taken as one too, which errs toward refusing.
fn unquoted_heredoc(line: &str) -> bool {
    let mut rest = line;
    while let Some(at) = rest.find("<<") {
        let after = &rest[at + 2..];
        // `<<<` is a here-string, which is refused as a redirect.
        if let Some(string) = after.strip_prefix('<') {
            rest = string;
            continue;
        }
        let delimiter = after.trim_start_matches('-').trim_start();
        if !delimiter.starts_with(['\'', '"', '\\']) {
            return true;
        }
        rest = after;
    }
    false
}

// Whether each `cat` among `commands` is the one a `gh` command's `--body
// "$(cat <<'EOF' ...)"` reads: one such body for each. A `cat` of its own
// prints whatever its heredoc says, such as a forged issue URL.
fn cats_feed_bodies(commands: &[Command]) -> bool {
    let cats = commands.iter().filter(|c| c.words[0] == "cat").count();
    let bodies: usize = (commands.iter())
        .filter(|c| c.words[0] == "gh")
        .map(|c| {
            let words = &c.words;
            (0..words.len())
                .filter(|&at| {
                    let word = words[at].as_str();
                    let next = words.get(at + 1).map(String::as_str);
                    match word.strip_prefix("--body=") {
                        Some(value) => heredoc_body(value),
                        None => matches!(word, "--body" | "-b") && next.is_some_and(heredoc_body),
                    }
                })
                .count()
        })
        .sum();
    cats <= bodies
}

fn is_heredoc(words: &[String]) -> bool {
    matches!(words, [w] if w == "<<" || w == "<<-")
}

// A word the shell passes as written.
fn plain(word: &str) -> bool {
    !word.contains(['$', '`'])
}

// The word `--body "$(cat <<'EOF' ... EOF)"` leaves once its heredoc is lifted.
fn heredoc_body(word: &str) -> bool {
    let squeezed: String = word.split_whitespace().collect();
    squeezed == "$(cat<<)" || squeezed == "$(cat<<-)"
}

#[cfg(test)]
mod tests;
