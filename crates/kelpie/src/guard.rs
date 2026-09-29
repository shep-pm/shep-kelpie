//! `kelpie guard`: the PreToolUse hook on every worker's Bash and Agent calls
//!
//! Kelpie adds it to every worker, before any hook a project names. It
//! refuses two things a worker's commit or pull request would carry out:
//! the home folder's path, which names the machine's user and every
//! worktree sits under, and a pull request title that is not a
//! conventional commit. It reads the command's own text and, for a commit
//! or a push in the worker's worktree, the lines it adds or sends. It never
//! echoes what it matched. A command it cannot read is refused.
//!
//! The hook runs outside the sandbox, so it runs git the way kelpie's own
//! worktree steps do, with the worktree's git dirs named and checked. A
//! repo the worker made could name any program in its own config.

mod git;
mod shell;
mod wrap;

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::confine::Verdict;
use wrap::{SHELLS, program, script};

/// Where the worker's own git lives
#[derive(Debug, Clone, Copy)]
pub struct Checkout<'a> {
    /// The project repo's common git dir
    pub git_common_dir: &'a Path,
    /// The work item's worktree
    pub worktree: &'a Path,
}

/// The conventional commit types a pull request title may start with
const TYPES: [&str; 11] = [
    "feat", "fix", "docs", "style", "refactor", "perf", "test", "build", "ci", "chore", "revert",
];

// A message file larger than this is not a message.
const MESSAGE_FILE_MAX: u64 = 1 << 20;

// A shell run inside a shell this many times over is not a worker's command.
const MAX_SHELLS: usize = 4;

// More commands than this in one call are refused rather than each judged:
// a command nested in parentheses is judged once per level around it.
const MAX_COMMANDS: usize = 200;

/// Judges the tool call in `input`, with `home` the home folder whose path stays in
pub fn judge(input: impl Read, home: Option<&Path>, checkout: Checkout<'_>) -> Verdict {
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
        isolation: Option<String>,
    }
    let call: Call = match serde_json::from_reader(input) {
        Ok(call) => call,
        Err(e) => return Verdict::Refuse(format!("kelpie cannot read this tool call: {e}")),
    };
    // Kelpie gives each worker its worktree; a subagent gets no other.
    if matches!(call.tool_name.as_str(), "Agent" | "Task")
        && matches!(
            call.tool_input.isolation.as_deref(),
            Some("worktree" | "remote")
        )
    {
        return Verdict::Refuse(
            "a worker's subagents run in its own worktree: leave out `isolation`.".into(),
        );
    }
    let Some(line) = call.tool_input.command.filter(|_| call.tool_name == "Bash") else {
        return Verdict::Allow;
    };
    let judging = Judging {
        home: home.and_then(Home::new),
        checkout,
    };
    let mut call_state = CallState::default();
    judging.line(&line, Some(call.cwd), 0, &mut call_state);
    if call_state.refusals.is_empty() {
        return Verdict::Allow;
    }
    Verdict::Refuse(call_state.refusals.join("\n\n"))
}

/// One Bash call being judged
struct Judging<'a> {
    home: Option<Home>,
    checkout: Checkout<'a>,
}

/// What judging one call has found and done so far
#[derive(Debug, Default)]
struct CallState {
    refusals: Vec<String>,
    commands: usize,
    reads: git::Reads,
}

impl CallState {
    // Each refusal once, however many commands earn it.
    fn refuse(&mut self, refusal: String) {
        if !self.refusals.contains(&refusal) {
            self.refusals.push(refusal);
        }
    }
}

impl Judging<'_> {
    // `cwd` is `None` once a `cd` goes somewhere the guard cannot follow.
    fn line(&self, line: &str, mut cwd: Option<PathBuf>, shells: usize, state: &mut CallState) {
        let commands = match shell::commands(line) {
            Ok(commands) if shells <= MAX_SHELLS => commands,
            Ok(_) => {
                return state.refuse(
                    "kelpie cannot check this command: it runs a shell inside a shell too \
                     many times over. Run it as plain commands."
                        .into(),
                );
            }
            Err(e) => {
                return state.refuse(format!(
                    "kelpie cannot check this command: {e}. Split it into plain commands."
                ));
            }
        };
        let home = self.home.as_ref();
        for command in &commands {
            state.commands += 1;
            if state.commands > MAX_COMMANDS {
                return state.refuse(
                    "kelpie cannot check this command: it runs too many commands at once. \
                     Split it into plain commands."
                        .into(),
                );
            }
            if command.defines_function {
                state.refuse(wrap::FUNCTION.into());
                continue;
            }
            let run = match wrap::unwrap(&command.words) {
                Ok(Some(run)) => run,
                Ok(None) => continue,
                Err(refusal) => {
                    state.refuse(refusal);
                    continue;
                }
            };
            if run.moved {
                cwd = None;
            }
            let found = match program(&run.words[0]) {
                "cd" | "pushd" => {
                    cwd = match run.words.get(1) {
                        Some(to) => moved(cwd.as_deref(), to, home),
                        None => home.map(|h| h.path.clone()),
                    };
                    Vec::new()
                }
                // Where `popd` returns to is not in the command.
                "popd" => {
                    cwd = None;
                    Vec::new()
                }
                "git" => {
                    let git = git::Git {
                        words: run.words,
                        heredocs: &command.heredocs,
                        redirected: run.git_redirected,
                    };
                    git::judge(git, cwd.as_deref(), home, self.checkout, &mut state.reads)
                }
                "gh" => gh(run.words, &command.heredocs, cwd.as_deref(), home),
                name if SHELLS.contains(&name) => {
                    if let Some(script) = script(run.words) {
                        self.line(script, cwd.clone(), shells + 1, state);
                    }
                    // A shell reading its commands from a heredoc.
                    for body in &command.heredocs {
                        self.line(body, cwd.clone(), shells + 1, state);
                    }
                    Vec::new()
                }
                _ => Vec::new(),
            };
            for refusal in found {
                state.refuse(refusal);
            }
        }
    }
}

// Where `cd` or `git -C` moves from `cwd`: `None` when it cannot be told.
fn moved(cwd: Option<&Path>, to: &str, home: Option<&Home>) -> Option<PathBuf> {
    match to.strip_prefix('~') {
        Some("") => Some(home?.path.clone()),
        Some(rest) => Some(home?.path.join(rest.strip_prefix('/')?)),
        None if to == "-" => None,
        None if Path::new(to).is_absolute() => Some(PathBuf::from(to)),
        None => Some(cwd?.join(to)),
    }
}

// `gh` publishing verbs: their titles, bodies and notes, and a pull request's title.
fn gh(
    words: &[String],
    heredocs: &[String],
    cwd: Option<&Path>,
    home: Option<&Home>,
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
    let [group, verb] = positions[..] else {
        return Vec::new();
    };
    let verb = match (words[group].as_str(), words[verb].as_str()) {
        (group, "new") => (group, "create"),
        pair => pair,
    };
    let publishes = matches!(
        verb,
        ("pr", "create" | "edit" | "comment" | "review")
            | ("issue", "create" | "edit" | "comment")
            | ("release", "create" | "edit")
    );
    if !publishes {
        return Vec::new();
    }
    let args = &words[i..];
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
        // Not echoed: a title can carry the home folder's path too.
        if !titles.iter().all(|t| conventional(t)) {
            out.push(format!(
                "this pull request's title is not a conventional commit. Write it as \
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
            .chain(heredocs.iter().cloned());
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
fn files(args: &[String], long: &[&str], short: &[char], cwd: Option<&Path>) -> Vec<String> {
    values(args, long, short)
        .into_iter()
        .filter(|f| f != "-")
        .filter_map(|f| {
            // A relative file under a folder the guard could not follow is not read.
            let path = moved(cwd, &f, None)?;
            // A pipe would hold the hook open.
            let small =
                fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() <= MESSAGE_FILE_MAX);
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
struct Home {
    path: PathBuf,
    text: String,
}

impl Home {
    // `/` alone would match every absolute path.
    fn new(path: &Path) -> Option<Self> {
        let text = path.to_str()?.trim_end_matches('/').to_lowercase();
        (text.len() > 1 && text.starts_with('/')).then(|| Self {
            path: path.to_owned(),
            text,
        })
    }

    // Whether `written` names the folder, or a path under it.
    fn is_in(&self, written: &str) -> bool {
        let written = written.to_lowercase();
        written.match_indices(&self.text).any(|(at, _)| {
            written[at + self.text.len()..]
                .chars()
                .next()
                .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '-'))
        })
    }

    // The files whose added lines in `patches` name the folder.
    fn added(&self, patches: &str) -> Vec<String> {
        let mut file = "";
        let mut out: Vec<String> = Vec::new();
        for line in patches.lines() {
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

#[cfg(test)]
mod tests;
