//! Agent definition files
//!
//! Each agent is `<name>.md` in the `agents` folder of kelpie's home: YAML
//! frontmatter naming what it is for and what runs it, then a Markdown body.
//! An implementer's body is added to kelpie's own instructions, and a
//! reviewer's is its prompt. Kelpie embeds its defaults, which a file of the
//! same name replaces, and `shep kelpie add` writes out any that are
//! missing. A file that cannot be read or used stops the runner, naming the
//! file and what is wrong with it. A `.md` file whose name is no agent's,
//! such as a `README.md`, is skipped and named in the log. A review bot's
//! file is named for its bot, since the bot's window is its account's.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::review_bot::BotReviewer;
use crate::settings::{
    AgentName, Effort, Endpoint, Limit, LocalCommand, LocalRound, NonBlank, RoleModel,
};

mod front;

/// The folder in kelpie's home that holds the agent files
pub const FOLDER: &str = "agents";

/// The implementer a project lists when it names none
pub const DEFAULT_IMPLEMENTER: &str = "sonnet-high";

/// The reviewer every project lists when it names none
pub const DEFECT_HUNTER: &str = "defect-hunter";

/// The reviewer that runs the maintainer's qwen-review script
pub const QWEN: &str = "qwen";

/// The maintainer's qwen-review script, which the `qwen` reviewer runs
pub const QWEN_REVIEW: &str = "~/.claude/scripts/qwen-review.sh";

// When `add` writes one of kelpie's own agents out.
#[derive(Debug, Clone, Copy)]
enum Written {
    Always,
    // Only where this file, under the home folder, is installed.
    Beside(&'static str),
}

// Kelpie's own agents, by name: what `add` writes out, and what a missing
// file falls back to. A project lists none of the review bots unless told to.
const DEFAULTS: [(&str, &str, Written); 7] = [
    (
        DEFAULT_IMPLEMENTER,
        include_str!("../agents/sonnet-high.md"),
        Written::Always,
    ),
    (
        "opus-high",
        include_str!("../agents/opus-high.md"),
        Written::Always,
    ),
    (
        DEFECT_HUNTER,
        include_str!("../agents/defect-hunter.md"),
        Written::Always,
    ),
    (
        QWEN,
        include_str!("../agents/qwen.md"),
        Written::Beside(QWEN_REVIEW),
    ),
    (
        "coderabbit",
        include_str!("../agents/coderabbit.md"),
        Written::Always,
    ),
    ("cubic", include_str!("../agents/cubic.md"), Written::Always),
    ("codex", include_str!("../agents/codex.md"), Written::Always),
];

/// What an agent is for
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// It builds a work item, once a project lists it in `agents.implementers`
    Implementer,
    /// It reads a pull request once a pass, once a project lists it in
    /// `agents.reviewers`
    Reviewer,
}

impl Role {
    /// The role's name, as an agent file writes it
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Implementer => "implementer",
            Self::Reviewer => "reviewer",
        }
    }
}

/// What runs an agent's calls
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Runs {
    /// A fresh or resumed session on a harness, at a model and effort
    Session {
        /// Its harness, model and effort
        model: RoleModel,
        /// What holds its calls back
        limit: Limit,
    },
    /// A command that keeps the README's contract: a reviewer only
    Command(LocalCommand),
    /// Kelpie's own reviewer over an OpenAI-compatible server: a reviewer only
    Endpoint(Endpoint),
    /// A review bot summoned on the pull request: a reviewer only
    Bot(BotReviewer),
}

impl Runs {
    /// The session's model and limit, for an agent that runs sessions
    pub fn session(&self) -> Option<(&RoleModel, &Limit)> {
        match self {
            Self::Session { model, limit } => Some((model, limit)),
            Self::Command(_) | Self::Endpoint(_) | Self::Bot(_) => None,
        }
    }

    /// The local round, for an agent that runs on its own
    pub fn local(&self) -> Option<LocalRound> {
        match self {
            Self::Session { .. } | Self::Bot(_) => None,
            Self::Command(command) => Some(LocalRound::Command(command.clone())),
            Self::Endpoint(endpoint) => Some(LocalRound::Endpoint(endpoint.clone())),
        }
    }

    /// The bot, for an agent that is a review bot
    pub fn bot(&self) -> Option<BotReviewer> {
        match self {
            Self::Bot(bot) => Some(*bot),
            Self::Session { .. } | Self::Command(_) | Self::Endpoint(_) => None,
        }
    }
}

/// One agent, as a call on it takes it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    /// What it is for
    pub role: Role,
    /// What runs its calls
    pub runs: Runs,
    /// Its file's body: an implementer's addition to kelpie's own
    /// instructions, or a reviewer's prompt. None when blank.
    pub prompt: Option<String>,
    /// Globs of the files a pull request must change for this reviewer to
    /// run. Every pull request when empty, and always empty for an implementer.
    pub paths: Vec<NonBlank>,
    /// Whether this reviewer reads twice, the second time shown what it found
    /// the first and asked only for what it missed. Never for an implementer.
    pub second_look: bool,
}

/// Every agent kelpie knows: its defaults, and the files that replace or add to them
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agents {
    agents: BTreeMap<AgentName, Agent>,
    skipped: Vec<PathBuf>,
}

impl Agents {
    /// Kelpie's own agents, with no file read
    pub fn embedded() -> Self {
        // A test pins that every default parses, so none is dropped here.
        let parsed = DEFAULTS.iter().filter_map(|(name, text, _)| {
            let name = AgentName::try_from((*name).to_owned()).ok()?;
            Some((name, front::parse(text).ok()?))
        });
        Self {
            agents: parsed.collect(),
            skipped: Vec::new(),
        }
    }

    /// Kelpie's own agents, then each `<name>.md` in `folder` over them
    ///
    /// A folder that is not there holds no files, and a `.md` file whose
    /// name is no agent's is skipped.
    ///
    /// # Errors
    ///
    /// [`AgentsError`] naming the folder or the first file, by name, that
    /// cannot be read or used.
    pub fn load(folder: &Path) -> Result<Self, AgentsError> {
        let mut agents = Self::embedded();
        let entries = match fs::read_dir(folder) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(agents),
            Err(e) => return Err(failed(folder, e)),
        };
        let mut files = Vec::new();
        for entry in entries {
            let path = entry.map_err(|e| failed(folder, e))?.path();
            if path.extension().is_some_and(|e| e == "md") && path.is_file() {
                files.push(path);
            }
        }
        files.sort();
        for path in files {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let Ok(name) = AgentName::try_from(stem.to_owned()) else {
                agents.skipped.push(path);
                continue;
            };
            let text = fs::read_to_string(&path).map_err(|e| failed(&path, e))?;
            let agent =
                front::parse_named(name.as_str(), &text).map_err(|message| AgentsError::File {
                    path: path.clone(),
                    message,
                })?;
            agents.agents.insert(name, agent);
        }
        Ok(agents)
    }

    /// The agent `name`, if kelpie has one
    pub fn get(&self, name: &AgentName) -> Option<&Agent> {
        self.agents.get(name)
    }

    /// A line for each `.md` file skipped because its name is no agent's
    pub fn skipped(&self) -> impl Iterator<Item = String> + '_ {
        self.skipped.iter().map(|path| {
            format!(
                "agent file {} is skipped: an agent's name is lowercase letters, digits \
                 and `-`",
                path.display()
            )
        })
    }

    /// `name` defined by `text`, over any agent of that name, for a test
    ///
    /// # Panics
    ///
    /// When `name` or `text` is not a usable agent, naming why.
    #[cfg(test)]
    #[track_caller]
    pub(crate) fn with(mut self, name: &str, text: &str) -> Self {
        let named: AgentName = name.to_owned().try_into().unwrap();
        let agent = front::parse_named(name, text).unwrap_or_else(|e| panic!("agent {name}: {e}"));
        self.agents.insert(named, agent);
        self
    }
}

/// Writes each of kelpie's own agents that `folder` lacks, never over a file
///
/// The `qwen` reviewer is written only where the maintainer's qwen-review
/// script is installed under `home`. Returns the names written, in order.
///
/// # Errors
///
/// [`AgentsError::Read`] naming the folder or file that could not be written.
pub fn write_defaults(folder: &Path, home: &Path) -> Result<Vec<&'static str>, AgentsError> {
    fs::create_dir_all(folder).map_err(|e| failed(folder, e))?;
    let mut written = Vec::new();
    for (name, text, when) in DEFAULTS {
        let wanted = match when {
            Written::Always => true,
            Written::Beside(file) => installed(file, home),
        };
        if wanted && write_new(&folder.join(format!("{name}.md")), text)? {
            written.push(name);
        }
    }
    Ok(written)
}

/// Whether `file`, written with a leading `~/`, is a file under `home`
pub fn installed(file: &str, home: &Path) -> bool {
    file.strip_prefix("~/")
        .is_some_and(|rest| home.join(rest).is_file())
}

// Writes `text` to `path` when nothing is there, and says whether it did.
fn write_new(path: &Path, text: &str) -> Result<bool, AgentsError> {
    match fs::File::create_new(path) {
        Ok(mut file) => {
            file.write_all(text.as_bytes())
                .map_err(|e| failed(path, e))?;
            Ok(true)
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(failed(path, e)),
    }
}

fn failed(path: &Path, e: io::Error) -> AgentsError {
    AgentsError::Read {
        path: path.to_owned(),
        kind: e.kind(),
    }
}

/// What `folder`'s `.md` files hold, by file name, so a change to any of
/// them can be seen without loading them
///
/// A folder or file that cannot be read shows as its error, so it reads as
/// a change once it can be.
pub fn snapshot(folder: &Path) -> Vec<(String, String)> {
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => return vec![(String::new(), e.kind().to_string())],
    };
    let mut files: Vec<(String, String)> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "md"))
        .map(|path| {
            let text = fs::read_to_string(&path).unwrap_or_else(|e| e.kind().to_string());
            (path.display().to_string(), text)
        })
        .collect();
    files.sort();
    files
}

/// Writes `name`'s file in `folder` for a work item that opened before agent
/// files, on Claude Code with the `model` and `effort` its worker ran, never
/// over a file
///
/// Returns whether it wrote one: not when the file is there, or those would
/// not make a usable agent.
///
/// # Errors
///
/// [`AgentsError::Read`] naming the folder or file that could not be written.
pub fn write_kept(
    folder: &Path,
    name: &AgentName,
    model: &str,
    effort: Effort,
) -> Result<bool, AgentsError> {
    let text = format!(
        "---\n# Written by kelpie for a work item that opened before agent files,\n\
         # on the model and effort its worker ran.\n\
         role: implementer\nharness: claude-code\nmodel: \"{model}\"\neffort: {}\n---\n",
        effort.as_str()
    );
    let usable = !model.contains(['"', '\\', '\n']) && front::parse(&text).is_ok();
    if !usable {
        return Ok(false);
    }
    fs::create_dir_all(folder).map_err(|e| failed(folder, e))?;
    write_new(&folder.join(format!("{name}.md")), &text)
}

/// Why kelpie's agents cannot be used
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentsError {
    /// The folder, or a file in it, could not be read or written
    Read {
        /// The folder or file
        path: PathBuf,
        /// What reading or writing it failed with
        kind: io::ErrorKind,
    },
    /// A file has no frontmatter, a key missing, unknown or malformed, or
    /// keys its harness cannot work with
    File {
        /// The file
        path: PathBuf,
        /// What is wrong, naming the key
        message: String,
    },
}

impl fmt::Display for AgentsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, kind } => write!(f, "agent files: {}: {kind}", path.display()),
            Self::File { path, message } => {
                write!(f, "agent file {}: {message}", path.display())
            }
        }
    }
}

impl core::error::Error for AgentsError {}

#[cfg(test)]
mod tests;
