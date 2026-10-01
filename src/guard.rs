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
//! refuses the ways of running a command it knows it cannot read; it is not
//! a shell, and does not find them all.
//!
//! The hook runs outside the sandbox, so it runs git the way kelpie's own
//! worktree steps do, with the worktree's git dirs named and checked. A
//! repo the worker made could name any program in its own config.

mod gh;
mod git;
mod script;
mod shell;
mod wrap;

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::confine::Verdict;
use crate::local_paths::{Leak, LocalPaths, Surface};
use wrap::{SHELLS, program, script};

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

/// One Bash call being judged
struct Judging<'a> {
    home: Home,
    checkout: Checkout<'a>,
}

/// What judging one call has found and done so far
#[derive(Debug, Default)]
struct CallState {
    refusals: Vec<String>,
    commands: usize,
    reads: git::Reads,
    // Whether an earlier command exported a variable that redirects git.
    git_redirected: bool,
    // The text of each line being read, the call's own first.
    texts: Vec<String>,
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
    fn line(&self, line: &str, cwd: Option<PathBuf>, shells: usize, state: &mut CallState) {
        state.texts.push(line.to_owned());
        self.commands(line, cwd, shells, state);
        state.texts.pop();
    }

    // A script file a command runs, read as one more shell's commands.
    fn script(&self, name: &str, cwd: Option<&Path>, shells: usize, state: &mut CallState) {
        match script::read(name, cwd, self.home.path(), &state.texts) {
            Ok(text) => self.line(&text, cwd.map(Path::to_owned), shells + 1, state),
            Err(refusal) => state.refuse(refusal),
        }
    }

    // `cwd` is `None` once a `cd` goes somewhere the guard cannot follow.
    fn commands(&self, line: &str, mut cwd: Option<PathBuf>, shells: usize, state: &mut CallState) {
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
        let home = &self.home;
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
            state.git_redirected |= wrap::sets_git_redirect(&command.words);
            let run = match wrap::unwrap(&command.words) {
                Ok(Some(run)) => run,
                Ok(None) => continue,
                Err(refusal) => {
                    state.refuse(refusal);
                    continue;
                }
            };
            // `builtin export`, `command export` and `A=1 export` export too.
            state.git_redirected |= wrap::sets_git_redirect(run.words);
            if run.moved {
                cwd = None;
            }
            let name = &run.words[0];
            if name.contains(['$', '`']) {
                state.refuse(
                    "kelpie cannot check a command whose program the shell works out when it \
                     runs: name the program."
                        .into(),
                );
                continue;
            }
            if name.contains('/') {
                match script::interpreted(name, cwd.as_deref(), home.path()) {
                    Ok(true) => self.script(name, cwd.as_deref(), shells, state),
                    Ok(false) => {}
                    Err(refusal) => state.refuse(refusal),
                }
            }
            let found = match program(name) {
                "cd" | "pushd" => {
                    cwd = match run.words.get(1) {
                        Some(to) => moved(cwd.as_deref(), to, home.path()),
                        None => home.path().map(Path::to_owned),
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
                        redirected: run.git_redirected || state.git_redirected,
                    };
                    git::judge(git, cwd.as_deref(), home, self.checkout, &mut state.reads)
                }
                "gh" => gh::judge(run.words, &command.heredocs, cwd.as_deref(), home),
                name if SHELLS.contains(&name) => {
                    let runs = match script(run.words) {
                        Some(script) => {
                            self.line(script, cwd.clone(), shells + 1, state);
                            script::Runs::Nothing
                        }
                        None => script::shell(run.words),
                    };
                    let heredoc = !command.heredocs.is_empty();
                    match runs {
                        script::Runs::File(file) => {
                            self.script(file, cwd.as_deref(), shells, state)
                        }
                        script::Runs::Stdin if !heredoc => state.refuse(script::STDIN.into()),
                        script::Runs::Unreadable => state.refuse(script::STDIN.into()),
                        script::Runs::Stdin | script::Runs::Nothing => {}
                    }
                    // A shell reading its commands from a heredoc.
                    for body in &command.heredocs {
                        self.line(body, cwd.clone(), shells + 1, state);
                    }
                    Vec::new()
                }
                "source" | "." => {
                    if let script::Runs::File(file) = script::sourced(run.words) {
                        self.script(file, cwd.as_deref(), shells, state);
                    }
                    Vec::new()
                }
                _ => wrap::hidden(run.words).into_iter().collect(),
            };
            for refusal in found {
                state.refuse(refusal);
            }
        }
    }
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
