//! `kelpie guard`: the PreToolUse hook on every worker's Bash and Agent calls
//!
//! Kelpie adds it to every worker, before any hook a project names. It
//! refuses two things a worker's commit or pull request would carry out:
//! whatever [`LocalPaths`] finds of this machine's (the home folder's path,
//! which every worktree sits under, and the rest the shared check knows),
//! and a pull request title that is not a conventional commit. It also
//! refuses what only the project manager does: a merge, marking ready, a
//! summons, and a push to the base branch. It reads the command's own text,
//! the shell scripts it runs and, for a commit or a push in the worker's
//! worktree, the lines it adds or sends. It never echoes what it matched. It
//! reads git and gh wherever the call's text runs them: behind a wrapper, in
//! a shell's `-c`, `<<<`, heredoc or script file, or in text git runs as a
//! command. It refuses a shell reading a pipe, and a program named by a
//! variable. It does not read `python -c` or a file a config names.
//!
//! The hook runs outside the sandbox, so it runs git the way kelpie's own
//! worktree steps do, with the worktree's git dirs named and checked. A
//! repo the worker made could name any program in its own config.

mod gh;
mod git;
mod judging;
mod script;
mod shell;
mod wrap;

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::confine::Verdict;
use crate::local_paths::{Leak, LocalPaths, Surface};
use judging::{CallState, Judging};

/// Where the worker's own git lives
#[derive(Debug, Clone, Copy)]
pub struct Checkout<'a> {
    /// The project repo's common git dir
    pub git_common_dir: &'a Path,
    /// The work item's worktree
    pub worktree: &'a Path,
}

// A message file larger than this is not a message.
const MESSAGE_FILE_MAX: u64 = 1 << 20;

// A shell run inside a shell this many times over is not a worker's command.
const MAX_SHELLS: usize = 4;

// More commands than this in one call are refused rather than each judged:
// a command nested in parentheses is judged once per level around it.
const MAX_COMMANDS: usize = 200;

/// How the hook's command line names one more folder to keep off the forge
pub const FOLDER_FLAG: &str = "--folder=";

/// How the hook's command line names one private word
pub const NAME_FLAG: &str = "--name=";

/// How the hook's command line asks for an allowed command to be handed
/// back pinned to the folder it was judged in
///
/// Codex runs a command in a `workdir` of the model's choosing and never
/// tells the hook which, so the hook judges the command in the turn's own
/// folder and answers with it prefixed by a check of where it runs.
pub const PIN_FLAG: &str = "--pin-folder";

/// How the pinned command asks `kelpie guard` to judge the call on its
/// stdin again in the folder it runs in, when that is not the one the hook
/// judged it in
pub const HERE_FLAG: &str = "--judge-here";

/// Whether `args` asks for [`PIN_FLAG`], and the rest of them
pub fn pin_flag(args: &[String]) -> (bool, Vec<String>) {
    take_flag(args, PIN_FLAG)
}

/// Whether `args` asks for [`HERE_FLAG`], and the rest of them
pub fn here_flag(args: &[String]) -> (bool, Vec<String>) {
    take_flag(args, HERE_FLAG)
}

fn take_flag(args: &[String], flag: &str) -> (bool, Vec<String>) {
    (
        args.iter().any(|a| a == flag),
        args.iter().filter(|a| *a != flag).cloned().collect(),
    )
}

/// The tool call in `input`, as though it ran in `folder`
///
/// # Errors
///
/// What to tell the worker, when `input` is not a tool call.
pub fn here(input: &[u8], folder: &Path) -> Result<Vec<u8>, String> {
    let unreadable = |e: serde_json::Error| format!("kelpie cannot read this tool call: {e}");
    let mut call: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(input).map_err(unreadable)?;
    call.insert("cwd".into(), folder.to_string_lossy().into());
    serde_json::to_vec(&call).map_err(unreadable)
}

/// Codex's hook answer that lets the command in `input` run where Codex
/// runs it, once that folder has been judged
///
/// The command runs as it is in the folder the hook judged it in. In
/// another folder of `worktree`, the model's `workdir`, it runs once
/// `again`, `kelpie guard` with [`HERE_FLAG`], allows it there too.
/// Anywhere else it is refused with what to do instead.
///
/// # Errors
///
/// What to tell the worker, when `input` names no folder or command.
pub fn pinned(input: &[u8], worktree: &Path, again: &[String]) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Call {
        cwd: PathBuf,
        tool_input: Command,
    }
    #[derive(Deserialize)]
    struct Command {
        command: String,
    }
    let call: Call = serde_json::from_slice(input)
        .map_err(|e| format!("kelpie cannot read this tool call: {e}"))?;
    let quote = |text: &str| format!("'{}'", text.replace('\'', r"'\''"));
    // `pwd -P` prints the folder with every link resolved.
    let folder = |path: &Path| {
        let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        quote(&path.to_string_lossy())
    };
    let (judged, tree) = (folder(&call.cwd), folder(worktree));
    // One line of JSON, printed by a builtin: a heredoc would need a
    // temporary file, which zsh makes where the sandbox refuses it.
    let tool_call = serde_json::json!({
        "tool_name": "Bash",
        "cwd": call.cwd,
        "tool_input": { "command": call.tool_input.command },
    });
    let again: Vec<String> = again.iter().map(|a| quote(a)).collect();
    let elsewhere = quote(&format!(
        "kelpie runs a command only inside the worktree {}: leave `workdir` out, \
         and start the command with `cd <folder> &&` to run it in another folder.",
        worktree.display()
    ));
    let command = format!(
        "case \"$(pwd -P)\" in\n\
         {judged}) ;;\n\
         {tree} | {tree}/*) printf '%s\\n' {tool_call} | {again} >/dev/null || exit 1 ;;\n\
         *) echo {elsewhere} >&2; exit 1 ;;\n\
         esac\n{}",
        call.tool_input.command,
        tool_call = quote(&tool_call.to_string()),
        again = again.join(" "),
    );
    let answer = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": { "command": command },
        }
    });
    Ok(answer.to_string())
}

/// What the hook keeps off the forge: `home`, then the folders and names `args` give
///
/// # Errors
///
/// What to tell the worker, when `args` holds one the hook does not know.
pub fn local_paths(home: Option<&Path>, args: &[String]) -> Result<LocalPaths, String> {
    let mut folders: Vec<&Path> = home.into_iter().collect();
    let mut names = Vec::new();
    for arg in args {
        if let Some(folder) = arg.strip_prefix(FOLDER_FLAG) {
            folders.push(Path::new(folder));
        } else if let Some(name) = arg.strip_prefix(NAME_FLAG) {
            names.push(name);
        } else {
            let shown: String = arg.chars().take(40).collect();
            return Err(format!("kelpie guard does not take `{shown}`"));
        }
    }
    Ok(LocalPaths::new(folders, names))
}

/// Judges the tool call in `input`, with `home` the home folder `~` names and
/// `local` what the project keeps off the forge
pub fn judge(
    input: impl Read,
    home: Option<&Path>,
    local: LocalPaths,
    checkout: Checkout<'_>,
) -> Verdict {
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
        subagent_type: Option<String>,
    }
    let call: Call = match serde_json::from_reader(input) {
        Ok(call) => call,
        Err(e) => return Verdict::Refuse(format!("kelpie cannot read this tool call: {e}")),
    };
    // Kelpie gives each worker its worktree; a subagent gets no other.
    let isolated = |isolation: Option<&str>| matches!(isolation, Some("worktree" | "remote"));
    if matches!(call.tool_name.as_str(), "Agent" | "Task")
        && (isolated(call.tool_input.isolation.as_deref())
            || call
                .tool_input
                .subagent_type
                .as_deref()
                .and_then(|name| agent_isolation(checkout.worktree, name))
                .is_some_and(|i| isolated(Some(&i))))
    {
        return Verdict::Refuse(
            "a worker's subagents run in its own worktree: leave out `isolation`.".into(),
        );
    }
    let Some(line) = call.tool_input.command.filter(|_| call.tool_name == "Bash") else {
        return Verdict::Allow;
    };
    let judging = Judging {
        home: Home::new(home, local),
        checkout,
    };
    let mut call_state = CallState::default();
    judging.line(&line, Some(call.cwd), 0, &mut call_state);
    if call_state.refusals.is_empty() {
        return Verdict::Allow;
    }
    Verdict::Refuse(call_state.refusals.join("\n\n"))
}

// The `isolation` a project subagent's definition in the worktree sets.
fn agent_isolation(worktree: &Path, name: &str) -> Option<String> {
    let plain = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !plain {
        return None;
    }
    let text =
        fs::read_to_string(worktree.join(".claude/agents").join(format!("{name}.md"))).ok()?;
    let front = text.strip_prefix("---")?.split("\n---").next()?;
    front.lines().find_map(|line| {
        let value = line.trim().strip_prefix("isolation:")?;
        Some(value.trim().trim_matches(['"', '\'']).to_owned())
    })
}

// Where `cd` or `git -C` moves from `cwd`: `None` when it cannot be told.
fn moved(cwd: Option<&Path>, to: &str, home: Option<&Path>) -> Option<PathBuf> {
    // A folder the shell works out when it runs.
    if to.contains(['$', '`', '*', '?', '[']) {
        return None;
    }
    match to.strip_prefix('~') {
        Some("") => Some(home?.to_owned()),
        Some(rest) => Some(home?.join(rest.strip_prefix('/')?)),
        None if to == "-" => None,
        None if Path::new(to).is_absolute() => Some(PathBuf::from(to)),
        None => Some(cwd?.join(to)),
    }
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

/// The home folder, and what else of this machine's a text must not name
struct Home {
    path: Option<PathBuf>,
    local: LocalPaths,
}

impl Home {
    // `/` alone would match every absolute path, and a relative one none.
    fn new(path: Option<&Path>, local: LocalPaths) -> Self {
        let path = path
            .filter(|p| {
                let text = p.to_str().unwrap_or_default().trim_end_matches('/');
                text.len() > 1 && text.starts_with('/')
            })
            .map(Path::to_owned);
        Self { path, local }
    }

    fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    // What the first of `texts` to name this machine names.
    fn find_in_prose(&self, texts: impl IntoIterator<Item = String>) -> Option<Leak> {
        texts
            .into_iter()
            .find_map(|t| self.local.find(&t, Surface::Prose))
    }

    // Each file whose added lines in `patches` name this machine, and what they name.
    fn added(&self, patches: &str) -> Vec<(String, Leak)> {
        let mut file = "";
        let mut out: Vec<(String, Leak)> = Vec::new();
        for line in patches.lines() {
            if let Some(name) = line.strip_prefix("+++ ") {
                file = name.strip_prefix("b/").unwrap_or(name);
            } else if line.starts_with('+')
                && !out.iter().any(|(f, _)| f == file)
                && let Some(leak) = self.local.find(line, Surface::Code)
            {
                out.push((file.to_owned(), leak));
            }
        }
        out.into_iter()
            .map(|(f, leak)| (format!("`{f}`"), leak))
            .collect()
    }

    // A name or an address is taken out; a path is written from the repo's root.
    fn refusal(&self, what: &str, leak: Leak, fix: &str) -> String {
        let advice = match leak {
            Leak::Path | Leak::Tilde => {
                " Write a path in the repo from its root (`src/lib.rs`), and leave out one \
                 outside it."
            }
            Leak::Name | Leak::Lan => "",
        };
        format!("{what} carries {leak}. {fix}{advice}")
    }
}

const WRITE: &str = "Take it out, then try again.";

const REWRITE: &str = "Those commits have not left this machine: take it out and rewrite \
                       them, since a new commit on top would still send the old one.";

#[cfg(test)]
mod machine_tests;
#[cfg(test)]
mod tests;
