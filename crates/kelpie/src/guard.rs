//! `kelpie guard`: the PreToolUse hook on every worker's Bash calls
//!
//! Kelpie adds it to every worker, before any hook a project names. It
//! refuses two things a worker's commit or pull request would carry out:
//! the home folder's path, which names the machine's user and every
//! worktree sits under, and a pull request title that is not a
//! conventional commit. It reads the command's own text and, for a commit
//! or a push in the worker's worktree, the lines it adds or sends. It never
//! echoes what it matched.
//!
//! The hook runs outside the sandbox, so it runs git the way kelpie's own
//! worktree steps do, with the worktree's git dirs named and checked. A
//! repo the worker made could name any program in its own config.

mod shell;

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::confine::Verdict;
use crate::worktree::{self, WorktreeError};
use shell::Command;

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

/// Judges the Bash call in `input`, with `home` the home folder whose path stays in
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
    }
    let call: Call = match serde_json::from_reader(input) {
        Ok(call) => call,
        Err(e) => return Verdict::Refuse(format!("kelpie cannot read this tool call: {e}")),
    };
    let Some(line) = call.tool_input.command.filter(|_| call.tool_name == "Bash") else {
        return Verdict::Allow;
    };
    let judging = Judging {
        home: home.and_then(Home::new),
        checkout,
    };
    let mut refusals = Vec::new();
    judging.line(&line, Some(call.cwd), 0, &mut refusals);
    if refusals.is_empty() {
        return Verdict::Allow;
    }
    Verdict::Refuse(refusals.join("\n\n"))
}

// A shell run inside a shell this many times over is not a worker's command.
const MAX_SHELLS: usize = 4;

// The shells whose `-c` script the guard reads as a command line of its own.
const SHELLS: [&str; 4] = ["sh", "bash", "zsh", "dash"];

/// One Bash call being judged
struct Judging<'a> {
    home: Option<Home>,
    checkout: Checkout<'a>,
}

impl Judging<'_> {
    // `cwd` is `None` once a `cd` goes somewhere the guard cannot follow.
    fn line(&self, line: &str, mut cwd: Option<PathBuf>, shells: usize, out: &mut Vec<String>) {
        let commands = match shell::commands(line) {
            Ok(commands) if shells <= MAX_SHELLS => commands,
            Ok(_) => return refuse(out, "kelpie cannot check this command: it runs a shell inside a shell too many times over. Run it as plain commands.".into()),
            Err(e) => return refuse(out, format!("kelpie cannot check this command: {e}. Split it into plain commands.")),
        };
        let home = self.home.as_ref();
        for command in commands {
            let found = match program(&command.words[0]) {
                "cd" => {
                    cwd = match command.words.get(1) {
                        Some(to) => moved(cwd.as_deref(), to, home),
                        None => home.map(|h| h.path.clone()),
                    };
                    Vec::new()
                }
                "git" => git(&command, cwd.as_deref(), home, self.checkout),
                "gh" => gh(&command, cwd.as_deref(), home),
                name if SHELLS.contains(&name) => {
                    if let Some(script) = script(&command.words) {
                        self.line(script, cwd.clone(), shells + 1, out);
                    }
                    Vec::new()
                }
                _ => Vec::new(),
            };
            for refusal in found {
                refuse(out, refusal);
            }
        }
    }
}

// Each refusal once, however many commands earn it.
fn refuse(out: &mut Vec<String>, refusal: String) {
    if !out.contains(&refusal) {
        out.push(refusal);
    }
}

// A command's program, by name: `/usr/bin/git` is `git`.
fn program(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

// The script a shell runs with `-c`, `-lc` and the like.
fn script(words: &[String]) -> Option<&str> {
    let at = words[1..]
        .iter()
        .position(|w| w.starts_with('-') && !w.starts_with("--") && w.contains('c'))?;
    words.get(at + 2).map(String::as_str)
}

// `git commit`, `git tag` and `git push`: their messages, and the lines they add or send.
fn git(
    command: &Command,
    cwd: Option<&Path>,
    home: Option<&Home>,
    checkout: Checkout<'_>,
) -> Vec<String> {
    let mut words = command.words[1..].iter();
    let mut dir = cwd.map(Path::to_owned);
    let sub = loop {
        match words.next().map(String::as_str) {
            Some("-C") => {
                let to = words.next().map_or("", String::as_str);
                dir = moved(dir.as_deref(), to, home);
            }
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
    if sub == "commit" || sub == "tag" {
        let messages = values(&args, &["--message"], &['m'])
            .into_iter()
            .chain(files(&args, &["--file"], &['F'], dir.as_deref()))
            .chain(command.heredocs.iter().cloned());
        if messages.into_iter().any(|m| home.is_in(&m)) {
            out.push(home.refusal(&format!("this {sub}'s message"), WRITE));
        }
    }
    if !matches!(sub, "commit" | "push") || !in_own_repo(checkout.worktree, dir.as_deref()) {
        return out;
    }
    let found = worktree::trusted(checkout.git_common_dir, checkout.worktree)
        .and_then(|run| read_git(sub, &args, home, run));
    match found {
        Ok(found) => out.extend(found),
        // Git that cannot be read is refused, not let through unchecked.
        Err(e) => out.push(format!(
            "kelpie cannot read this worktree's git to check this {sub}: {e}"
        )),
    }
    out
}

// What a commit adds, or a push sends, that names the home folder.
fn read_git(
    sub: &str,
    args: &[String],
    home: &Home,
    run: impl Fn(&[&str]) -> Result<String, WorktreeError>,
) -> Result<Vec<String>, WorktreeError> {
    // `--unified` alone makes `git log` print patches, so only patch reads take these.
    let patches = |args: &[&str]| run(&[args, &PLAIN].concat());
    let mut out = Vec::new();
    if sub == "push" {
        // A file written and committed in one call is not staged when the
        // commit is judged, so the push reads what it sends.
        let log = run(&["log", "--format=%B", "HEAD", "--not", "--remotes"])?;
        if home.is_in(&log) {
            out.push(home.refusal("a message in the commits this push sends", REWRITE));
        }
        let sent = patches(&["log", "-p", "--format=", "HEAD", "--not", "--remotes"])?;
        for file in home.added(&sent) {
            out.push(home.refusal(&format!("{file} in the commits this push sends"), REWRITE));
        }
        return Ok(out);
    }
    let all = args
        .iter()
        .take_while(|a| *a != "--")
        .any(|a| a == "--all" || a.starts_with('-') && !a.starts_with("--") && a.contains('a'));
    let range = if all { "HEAD" } else { "--cached" };
    for file in home.added(&patches(&["diff", range])?) {
        out.push(home.refusal(&format!("this commit's {file}"), WRITE));
    }
    Ok(out)
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

// Plain patches: no colour, and no program the repo's config names.
const PLAIN: [&str; 4] = [
    "--unified=0",
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
];

// Whether `dir` is the worker's own repo: in the worktree and in no repo it
// made there. A folder the guard cannot follow or see is judged as the worktree.
fn in_own_repo(worktree: &Path, dir: Option<&Path>) -> bool {
    let Ok(worktree) = worktree.canonicalize() else {
        return false;
    };
    let Some(Ok(dir)) = dir.map(Path::canonicalize) else {
        return true;
    };
    dir.starts_with(&worktree)
        && dir
            .ancestors()
            .take_while(|a| *a != worktree)
            .all(|a| a.join(".git").symlink_metadata().is_err())
}

// `gh` publishing verbs: their titles, bodies and notes, and a pull request's title.
fn gh(command: &Command, cwd: Option<&Path>, home: Option<&Home>) -> Vec<String> {
    let words = &command.words;
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
