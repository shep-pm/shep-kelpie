//! One Bash call's commands, judged in order
//!
//! A command's program is judged in full: `cd` moves where the rest run,
//! git and gh are checked, and a shell's script, in the call's text or in a
//! file, is read as commands of its own. Every git, gh or shell among the
//! words of a program the guard does not know is judged too, as far as a
//! command that may only name it can be.

use std::path::{Path, PathBuf};

use super::wrap::{
    self, BUILT, FUNCTION, OTHER_SHELLS, Reach, SHELLS, STDIN, Shell, Unwrapped, configures_git,
    program, redirects_git,
};
use super::{Checkout, Home, MAX_COMMANDS, MAX_SHELLS, gh, git, moved, script, shell};

// What a command's stdin holds, as the call's text shows it.
#[derive(Debug, Clone, Copy)]
struct Input<'a> {
    // The heredoc bodies it reads.
    heredocs: &'a [String],
    // Whether a `|` feeds it another command's output.
    piped: bool,
}

/// One Bash call being judged
pub(super) struct Judging<'a> {
    pub home: Option<Home>,
    pub checkout: Checkout<'a>,
}

/// What judging one call has found and done so far
#[derive(Debug, Default)]
pub(super) struct CallState {
    pub refusals: Vec<String>,
    commands: usize,
    reads: git::Reads,
    // Whether an earlier command exported a variable that redirects git.
    git_redirected: bool,
    // Whether an earlier command exported config for git to read.
    git_configured: bool,
    // Whether an earlier command made a repo.
    made_repo: bool,
    // The text of each line being read, the call's own first.
    texts: Vec<String>,
}

// Programs that only read or print text, and run no word they are given.
const READERS: [&str; 16] = [
    "grep", "egrep", "fgrep", "rg", "ag", "ack", "cat", "less", "more", "head", "tail", "wc",
    "sort", "uniq", "man", "which",
];

impl CallState {
    // Each refusal once, however many commands earn it.
    fn refuse(&mut self, refusal: String) {
        if !self.refusals.contains(&refusal) {
            self.refusals.push(refusal);
        }
    }

    // Counts one more command to judge: `false`, and a refusal, past the cap.
    fn count(&mut self) -> bool {
        self.commands += 1;
        if self.commands > MAX_COMMANDS {
            self.refuse(
                "kelpie cannot check this command: it runs too many commands at once. Split it \
                 into plain commands."
                    .into(),
            );
        }
        self.commands <= MAX_COMMANDS
    }
}

impl Judging<'_> {
    pub(super) fn line(
        &self,
        line: &str,
        cwd: Option<PathBuf>,
        shells: usize,
        state: &mut CallState,
    ) {
        state.texts.push(line.to_owned());
        self.commands(line, cwd, shells, state);
        state.texts.pop();
    }

    // A script file a command runs, read as one more shell's commands.
    fn script(&self, name: &str, cwd: Option<&Path>, shells: usize, state: &mut CallState) {
        match script::read(name, cwd, self.home.as_ref(), &state.texts) {
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
        for command in &commands {
            if !state.count() {
                return;
            }
            if command.defines_function {
                state.refuse(FUNCTION.into());
                continue;
            }
            // A value git or gh runs as a command, for this command or later ones.
            for script in wrap::commands_assigned(&command.words) {
                self.line(script, cwd.clone(), shells + 1, state);
            }
            state.git_redirected |= wrap::sets(&command.words, redirects_git);
            state.git_configured |= wrap::sets(&command.words, configures_git);
            let input = Input {
                heredocs: &command.heredocs,
                piped: command.piped,
            };
            self.command(&command.words, input, &mut cwd, shells, state);
        }
    }

    // One command's words, its wrappers off, and each git, gh or shell among them.
    fn command(
        &self,
        words: &[String],
        input: Input<'_>,
        cwd: &mut Option<PathBuf>,
        shells: usize,
        state: &mut CallState,
    ) {
        let run = match wrap::unwrap(words) {
            Ok(Some(run)) => run,
            // `xargs sh -c '...'` runs no git itself, but its words may.
            Ok(None) => {
                let run = Unwrapped {
                    words,
                    moved: false,
                    git_redirected: false,
                    git_configured: false,
                };
                return self.named(&run, input, cwd, shells, state);
            }
            Err(refusal) => return state.refuse(refusal),
        };
        // `builtin export`, `command export` and `A=1 export` export too.
        state.git_redirected |= wrap::sets(run.words, redirects_git);
        state.git_configured |= wrap::sets(run.words, configures_git);
        if run.moved {
            *cwd = None;
        }
        let name = &run.words[0];
        if wrap::built(name) {
            return state.refuse(BUILT.into());
        }
        if name.contains('/') {
            match script::interpreted(name, cwd.as_deref(), self.home.as_ref()) {
                Ok(true) => self.script(name, cwd.as_deref(), shells, state),
                Ok(false) => {}
                Err(refusal) => state.refuse(refusal),
            }
        }
        self.program(&run, input, cwd, Reach::Runs, shells, state);
        self.named(&run, input, cwd, shells, state);
    }

    // Each git, gh or shell among the words after `run`'s program.
    fn named(
        &self,
        run: &Unwrapped<'_>,
        input: Input<'_>,
        cwd: &mut Option<PathBuf>,
        shells: usize,
        state: &mut CallState,
    ) {
        // `ps | grep bash` searches for a shell, it does not run one.
        let reader = READERS.contains(&program(&run.words[0]));
        for at in 1..run.words.len() {
            let name = program(&run.words[at]);
            let shell = SHELLS.contains(&name) || OTHER_SHELLS.contains(&name);
            let judged = matches!(name, "git" | "gh") || shell && !reader;
            if !judged {
                continue;
            }
            if !state.count() {
                return;
            }
            let named = Unwrapped {
                words: &run.words[at..],
                ..run.clone()
            };
            self.program(&named, input, cwd, Reach::Named, shells, state);
        }
    }

    // The program at the front of `run`, which `reach` says how surely runs.
    fn program(
        &self,
        run: &Unwrapped<'_>,
        input: Input<'_>,
        cwd: &mut Option<PathBuf>,
        reach: Reach,
        shells: usize,
        state: &mut CallState,
    ) {
        let home = self.home.as_ref();
        let found = match program(&run.words[0]) {
            "cd" | "pushd" => {
                *cwd = match run.words.get(1) {
                    Some(to) => moved(cwd.as_deref(), to, home),
                    None => home.map(|h| h.path.clone()),
                };
                Vec::new()
            }
            // Where `popd` returns to is not in the command.
            "popd" => {
                *cwd = None;
                Vec::new()
            }
            "git" => {
                let words = run.words;
                for script in git::scripts(words) {
                    self.line(&script, cwd.clone(), shells + 1, state);
                }
                if let Some(command) = git::bisect_run(words) {
                    self.command(command, input, cwd, shells, state);
                }
                let git = git::Git {
                    words,
                    heredocs: input.heredocs,
                    redirected: run.git_redirected || state.git_redirected,
                    configured: run.git_configured || state.git_configured,
                    reach,
                    after_new_repo: state.made_repo,
                };
                let found = git::judge(git, cwd.as_deref(), home, self.checkout, &mut state.reads);
                state.made_repo |= git::makes_repo(words);
                found
            }
            "gh" => gh::judge(run.words, input.heredocs, cwd.as_deref(), home),
            name if SHELLS.contains(&name) => {
                // A variable set in front of a shell reaches its script.
                state.git_redirected |= run.git_redirected;
                state.git_configured |= run.git_configured;
                let scripts = match wrap::shell(run.words) {
                    Shell::Text(scripts) => scripts,
                    Shell::File(file) if reach == Reach::Runs => {
                        self.script(file, cwd.as_deref(), shells, state);
                        Vec::new()
                    }
                    // Behind a program kelpie does not know, only a shell that is
                    // fed stdin, or asks for it, is surely reading commands there.
                    Shell::Stdin { asked }
                        if input.heredocs.is_empty()
                            && (reach == Reach::Runs || asked || input.piped) =>
                    {
                        return state.refuse(STDIN.into());
                    }
                    // A file a program kelpie does not know names may be any text.
                    Shell::File(_) | Shell::Stdin { .. } | Shell::Nothing => Vec::new(),
                };
                // A heredoc is read as commands, whether or not it is the script.
                let heredocs = input.heredocs.iter().map(String::as_str);
                for script in scripts.into_iter().chain(heredocs) {
                    self.line(script, cwd.clone(), shells + 1, state);
                }
                Vec::new()
            }
            "source" | "." if reach == Reach::Runs => {
                if let Some(file) = script::sourced(run.words) {
                    self.script(file, cwd.as_deref(), shells, state);
                }
                Vec::new()
            }
            name if OTHER_SHELLS.contains(&name)
                && matches!(wrap::shell(run.words), Shell::Text(_)) =>
            {
                vec![wrap::other_shell(name)]
            }
            _ if reach == Reach::Runs => wrap::hidden(run.words).into_iter().collect(),
            _ => Vec::new(),
        };
        for refusal in found {
            state.refuse(refusal);
        }
    }
}
