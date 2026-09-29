//! `kelpie guard`: the PreToolUse hook on every worker's Bash calls
//!
//! Kelpie adds it to every worker, before any hook a project names. It
//! refuses two things a worker's commit or pull request would carry out:
//! the home folder's path, which names the machine's user and every
//! worktree sits under, and a pull request title that is not a
//! conventional commit. It reads the command's own text and, for a commit,
//! the lines it adds. It never echoes what it matched.

mod shell;

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};

use serde::Deserialize;

use crate::confine::Verdict;
use shell::Command;

/// The conventional commit types a pull request title may start with
const TYPES: [&str; 11] = [
    "feat", "fix", "docs", "style", "refactor", "perf", "test", "build", "ci", "chore", "revert",
];

// A message file larger than this is not a message.
const MESSAGE_FILE_MAX: u64 = 1 << 20;

/// Judges the Bash call in `input`, with `home` the home folder whose path stays in
pub fn judge(input: impl Read, home: Option<&Path>) -> Verdict {
    #[derive(Deserialize)]
    struct Call {
        tool_name: String,
        cwd: PathBuf,
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
    let Some(line) = call.tool_input.command.filter(|_| call.tool_name == "Bash") else {
        return Verdict::Allow;
    };
    let home = home.and_then(Home::new);
    let mut refusals: Vec<String> = Vec::new();
    let mut cwd = call.cwd;
    for command in shell::commands(&line) {
        let found = match command.words.first().map(String::as_str) {
            Some("cd") => {
                if let Some(dir) = command.words.get(1).filter(|d| !d.starts_with(['~', '-'])) {
                    cwd = cwd.join(dir);
                }
                Vec::new()
            }
            Some("git") => git(&command, &cwd, home.as_ref()),
            Some("gh") => gh(&command, &cwd, home.as_ref()),
            _ => Vec::new(),
        };
        for refusal in found {
            if !refusals.contains(&refusal) {
                refusals.push(refusal);
            }
        }
    }
    if refusals.is_empty() {
        return Verdict::Allow;
    }
    Verdict::Refuse(refusals.join("\n\n"))
}

// `git commit` and `git tag`: their messages, and the lines a commit adds.
fn git(command: &Command, cwd: &Path, home: Option<&Home>) -> Vec<String> {
    let mut words = command.words[1..].iter();
    let mut dir = cwd.to_owned();
    let sub = loop {
        match words.next().map(String::as_str) {
            Some("-C") => dir = dir.join(words.next().map_or("", String::as_str)),
            Some("-c") => {
                words.next();
            }
            Some(w) if w.starts_with('-') => {}
            Some(w) => break w,
            None => return Vec::new(),
        }
    };
    let Some(home) = home else {
        return Vec::new();
    };
    let args: Vec<String> = words.cloned().collect();
    let mut out = Vec::new();
    match sub {
        "commit" | "tag" => {
            let messages = values(&args, &["--message"], &['m'])
                .into_iter()
                .chain(files(&args, &["--file"], &['F'], &dir))
                .chain(command.heredocs.iter().cloned());
            if messages.into_iter().any(|m| home.is_in(&m)) {
                out.push(home.refusal(&format!("this {sub}'s message"), WRITE));
            }
        }
        // A file written and committed in one call is not staged when the
        // commit is judged, so the push reads what it sends.
        "push" => {
            let log = run_git(&dir, &["log", "--format=%B", "HEAD", "--not", "--remotes"]);
            if home.is_in(&log) {
                out.push(home.refusal("a message in the commits this push sends", REWRITE));
            }
            let patches = ["log", "-p", "--format=", "HEAD", "--not", "--remotes"];
            for file in home.added(&dir, &patches) {
                out.push(home.refusal(&format!("{file} in the commits this push sends"), REWRITE));
            }
        }
        _ => {}
    }
    if sub == "commit" {
        let all = args
            .iter()
            .take_while(|a| *a != "--")
            .any(|a| a == "--all" || a.starts_with('-') && !a.starts_with("--") && a.contains('a'));
        let range = if all { "HEAD" } else { "--cached" };
        for file in home.added(&dir, &["diff", range]) {
            out.push(home.refusal(&format!("this commit's {file}"), WRITE));
        }
    }
    out
}

// `gh` publishing verbs: their titles, bodies and notes, and a pull request's title.
fn gh(command: &Command, cwd: &Path, home: Option<&Home>) -> Vec<String> {
    let words = &command.words;
    let verb = (
        words.get(1).map_or("", String::as_str),
        words.get(2).map_or("", String::as_str),
    );
    let publishes = matches!(
        verb,
        ("pr", "create" | "edit" | "comment" | "review")
            | ("issue", "create" | "edit" | "comment")
            | ("release", "create" | "edit")
    );
    if !publishes {
        return Vec::new();
    }
    let args = &words[3..];
    let titles = values(args, &["--title"], &['t']);
    let mut out = Vec::new();
    if verb.0 == "pr" && verb.1 == "create" && titles.is_empty() {
        out.push(
            "`gh pr create` needs `--title`: without one, GitHub titles a pull request of \
             several commits with the branch's name. Give it a conventional commit subject, \
             such as `fix(parser): keep the last line`."
                .to_owned(),
        );
    }
    if verb.0 == "pr" {
        for title in titles.iter().filter(|t| !conventional(t)) {
            out.push(format!(
                "the title {title:?} is not a conventional commit. Write it as \
                 `type(scope): summary`, the scope optional, with the type one of {}, and \
                 `!` after the type or scope for a breaking change.",
                TYPES.join(", ")
            ));
        }
    }
    if let Some(home) = home {
        let texts = titles
            .into_iter()
            .chain(values(args, &["--body", "--notes"], &['b', 'n']))
            .chain(files(args, &["--body-file", "--notes-file"], &['F'], cwd))
            .chain(command.heredocs.iter().cloned());
        if texts.into_iter().any(|t| home.is_in(&t)) {
            out.push(home.refusal(&format!("this `gh {} {}`", verb.0, verb.1), WRITE));
        }
    }
    out
}

// The values of a flag, as `--long v`, `--long=v`, `-s v`, `-sv` or `-xs v`.
fn values(args: &[String], long: &[&str], short: &[char]) -> Vec<String> {
    let mut out = Vec::new();
    let mut words = args.iter().take_while(|a| *a != "--");
    while let Some(word) = words.next() {
        if let Some(name) = word.strip_prefix("--") {
            match name.split_once('=') {
                Some((name, value)) if long.contains(&format!("--{name}").as_str()) => {
                    out.push(value.to_owned());
                }
                None if long.contains(&word.as_str()) => {
                    out.extend(words.next().cloned());
                }
                _ => {}
            }
        } else if let Some(cluster) = word.strip_prefix('-')
            && let Some(at) = cluster.find(|c| short.contains(&c))
        {
            let rest = &cluster[at + 1..];
            if rest.is_empty() {
                out.extend(words.next().cloned());
            } else {
                out.push(rest.to_owned());
            }
        }
    }
    out
}

// What the files a flag names hold. `-` is stdin, which a heredoc carries.
fn files(args: &[String], long: &[&str], short: &[char], cwd: &Path) -> Vec<String> {
    values(args, long, short)
        .into_iter()
        .filter(|f| f != "-")
        .filter_map(|f| {
            let path = cwd.join(f);
            let small = fs::metadata(&path).is_ok_and(|m| m.len() <= MESSAGE_FILE_MAX);
            small.then(|| fs::read_to_string(path).ok()).flatten()
        })
        .collect()
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

/// The home folder's path, as text that must not leave the machine
struct Home(String);

impl Home {
    // A path this short would match every absolute path.
    fn new(path: &Path) -> Option<Self> {
        let text = path.to_str()?.trim_end_matches('/').to_lowercase();
        (text.matches('/').count() >= 2).then_some(Self(text))
    }

    // Whether `text` names the folder, or a path under it.
    fn is_in(&self, text: &str) -> bool {
        let text = text.to_lowercase();
        text.match_indices(&self.0).any(|(at, _)| {
            text[at + self.0.len()..]
                .chars()
                .next()
                .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '-'))
        })
    }

    // The files whose added lines, in what `git <args>` prints, name the folder.
    fn added(&self, dir: &Path, args: &[&str]) -> Vec<String> {
        let plain = [
            "--unified=0",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
        ];
        let diff = run_git(dir, &[args, &plain].concat());
        let mut file = "";
        let mut out: Vec<String> = Vec::new();
        for line in diff.lines() {
            if let Some(name) = line.strip_prefix("+++ ") {
                file = name.strip_prefix("b/").unwrap_or(name);
            } else if line.starts_with('+') && self.is_in(line) && !out.iter().any(|f| f == file) {
                out.push(file.to_owned());
            }
        }
        out.into_iter().map(|f| format!("`{f}`")).collect()
    }

    fn refusal(&self, what: &str, fix: &str) -> String {
        format!(
            "{what} carries the home folder's absolute path, which names this machine's user. \
             {fix} Write a path in the repo from its root (`src/lib.rs`), and one outside it \
             with `~` for the home folder."
        )
    }
}

const WRITE: &str = "Take it out, then try again.";

const REWRITE: &str = "Those commits have not left this machine: take it out and rewrite \
                       them, since a new commit on top would still send the old one.";

// What `git <args>` prints in `dir`, or nothing when it cannot run.
fn run_git(dir: &Path, args: &[&str]) -> String {
    Process::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
