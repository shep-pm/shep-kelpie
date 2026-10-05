//! Agent definition files
//!
//! Each agent is `<name>.md` in the `agents` folder of kelpie's home: YAML
//! frontmatter naming what it is for, its harness, model and effort, then a
//! Markdown body added to kelpie's own instructions. Kelpie embeds its
//! defaults, which a file of the same name replaces, and `shep kelpie add`
//! writes out any that are missing. A file that cannot be read or used stops
//! the runner, naming the file and what is wrong with it. A `.md` file whose
//! name is no agent's, such as a `README.md`, is skipped and named in the log.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::forwarder::Upstream;
use crate::settings::{
    Account, AgentHarness, AgentName, ContextSize, Effort, EndpointUrl, Harness, LeaseName, Limit,
    ModelServer, NonBlank, RoleModel, UsageReader,
};

/// The folder in kelpie's home that holds the agent files
pub const FOLDER: &str = "agents";

/// The implementer a project lists when it names none
pub const DEFAULT_IMPLEMENTER: &str = "sonnet-high";

// Kelpie's own agents, by name: what `add` writes out, and what a missing file falls back to.
const DEFAULTS: [(&str, &str); 2] = [
    (
        DEFAULT_IMPLEMENTER,
        include_str!("../agents/sonnet-high.md"),
    ),
    ("opus-high", include_str!("../agents/opus-high.md")),
];

/// What an agent is for
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// It builds a work item, once a project lists it in `agents.implementers`
    Implementer,
}

/// One agent, as a call on it takes it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    /// What it is for
    pub role: Role,
    /// Its harness, model and effort
    pub model: RoleModel,
    /// What holds its calls back
    pub limit: Limit,
    /// Its file's body, added to kelpie's own instructions. None when blank.
    pub prompt: Option<String>,
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
        let parsed = DEFAULTS.iter().filter_map(|(name, text)| {
            let name = AgentName::try_from((*name).to_owned()).ok()?;
            Some((name, parse(text).ok()?))
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
        let unread = |path: &Path, e: io::Error| AgentsError::Read {
            path: path.to_owned(),
            kind: e.kind(),
        };
        let entries = match fs::read_dir(folder) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(agents),
            Err(e) => return Err(unread(folder, e)),
        };
        let mut files = Vec::new();
        for entry in entries {
            let path = entry.map_err(|e| unread(folder, e))?.path();
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
            let text = fs::read_to_string(&path).map_err(|e| unread(&path, e))?;
            let agent = parse(&text).map_err(|message| AgentsError::File {
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
        let agent = parse(text).unwrap_or_else(|e| panic!("agent {name}: {e}"));
        self.agents
            .insert(name.to_owned().try_into().unwrap(), agent);
        self
    }
}

/// Writes each of kelpie's own agents that `folder` lacks, never over a file
///
/// Returns the names written, in order.
///
/// # Errors
///
/// [`AgentsError::Read`] naming the folder or file that could not be written.
pub fn write_defaults(folder: &Path) -> Result<Vec<&'static str>, AgentsError> {
    let failed = |path: &Path, e: io::Error| AgentsError::Read {
        path: path.to_owned(),
        kind: e.kind(),
    };
    fs::create_dir_all(folder).map_err(|e| failed(folder, e))?;
    let mut written = Vec::new();
    for (name, text) in DEFAULTS {
        let path = folder.join(format!("{name}.md"));
        match fs::File::create_new(&path) {
            Ok(mut file) => {
                file.write_all(text.as_bytes())
                    .map_err(|e| failed(&path, e))?;
                written.push(name);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(failed(&path, e)),
        }
    }
    Ok(written)
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
    let usable = !model.contains(['"', '\\', '\n']) && parse(&text).is_ok();
    if !usable {
        return Ok(false);
    }
    let failed = |path: &Path, e: io::Error| AgentsError::Read {
        path: path.to_owned(),
        kind: e.kind(),
    };
    fs::create_dir_all(folder).map_err(|e| failed(folder, e))?;
    let path = folder.join(format!("{name}.md"));
    match fs::File::create_new(&path) {
        Ok(mut file) => {
            file.write_all(text.as_bytes())
                .map_err(|e| failed(&path, e))?;
            Ok(true)
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(failed(&path, e)),
    }
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

// An agent file's frontmatter
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Front {
    role: Role,
    harness: Harness,
    model: NonBlank,
    effort: Effort,
    #[serde(default)]
    usage: Option<UsageReader>,
    #[serde(default)]
    lease: Option<LeaseName>,
    #[serde(default)]
    url: Option<EndpointUrl>,
    #[serde(default)]
    context: Option<ContextSize>,
}

/// The agent an agent file's `text` defines, or what is wrong with it
fn parse(text: &str) -> Result<Agent, String> {
    let Some((front, body)) = split(text) else {
        return Err("must start with a `---` line, then the YAML frontmatter, \
                    then a `---` line of its own"
            .into());
    };
    // A blank first line stands for the opening `---`, so a message's line is the file's.
    let yaml = format!("\n{front}");
    let front: Front = serde_saphyr::from_str(&yaml).map_err(|e| yaml_error(&yaml, &e))?;
    let model = front.role_model()?;
    let limit = front.limit()?;
    let body = body.trim();
    Ok(Agent {
        role: front.role,
        model,
        limit,
        prompt: (!body.is_empty()).then(|| body.to_owned()),
    })
}

// The parser's message, led by the key whose value it refuses. A missing or
// unknown key is already named, and placed at the start of a line.
fn yaml_error(yaml: &str, error: &serde_saphyr::Error) -> String {
    let message = error.without_snippet().to_string();
    let key = error
        .location()
        .filter(|at| at.column() > 1)
        .and_then(|at| {
            let line = yaml
                .lines()
                .nth(usize::try_from(at.line()).ok()?.checked_sub(1)?)?;
            let (key, _) = line.split_once(':')?;
            Some(key.trim()).filter(|k| !k.is_empty() && !k.starts_with('#'))
        });
    match key {
        Some(key) => format!("`{key}`: {message}"),
        None => message,
    }
}

// The frontmatter between the opening and closing `---` lines, and the body after.
fn split(text: &str) -> Option<(&str, &str)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    let mut at = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\n', '\r']) == "---" {
            return Some((&rest[..at], &rest[at + line.len()..]));
        }
        at += line.len();
    }
    None
}

impl Front {
    /// Its model and effort, as a call on its harness takes them, or why its
    /// keys cannot reach that model
    fn role_model(&self) -> Result<RoleModel, String> {
        let harness = match (self.harness, &self.url, self.context) {
            (Harness::ClaudeCode, None, None) => AgentHarness::ClaudeCode,
            (Harness::ClaudeCode, ..) => {
                return Err("runs on claude-code, which takes no `url` or `context`".into());
            }
            (Harness::Pi, Some(url), Some(context)) => {
                if let Err(e) = Upstream::new(url) {
                    return Err(format!(
                        "runs on pi, with a `url` kelpie cannot forward to: {e}"
                    ));
                }
                AgentHarness::Pi(ModelServer {
                    url: url.clone(),
                    context,
                })
            }
            (Harness::Pi, ..) => {
                return Err("runs on pi, which needs the model's server as `url` \
                            and its context size as `context`"
                    .into());
            }
            (Harness::Codex, None, None) => AgentHarness::Codex,
            (Harness::Codex, ..) => {
                return Err("runs on codex, which takes no `url` or `context`".into());
            }
            #[cfg(test)]
            (Harness::StandIn, ..) => AgentHarness::StandIn,
        };
        Ok(RoleModel {
            model: self.model.clone(),
            effort: self.effort,
            harness,
        })
    }

    /// What holds its calls back, or why its keys do not say
    ///
    /// The reader must be the harness's own: an agent on Claude Code spends
    /// the Claude account whatever its `usage` says.
    fn limit(&self) -> Result<Limit, String> {
        let own = match self.harness {
            Harness::ClaudeCode => UsageReader::Claude,
            Harness::Pi => UsageReader::None,
            Harness::Codex => UsageReader::Codex,
            #[cfg(test)]
            Harness::StandIn => self.usage.unwrap_or(UsageReader::Claude),
        };
        let usage = self.usage.unwrap_or(own);
        if usage != own {
            return Err(format!(
                "runs on {}, whose usage is read with `{}`, so it cannot set \
                 `usage: {}`: leave `usage` out",
                self.harness.as_str(),
                own.as_str(),
                usage.as_str()
            ));
        }
        match (usage, &self.lease) {
            (UsageReader::None, lease) => {
                Ok(Limit::Lease(lease.clone().unwrap_or_else(LeaseName::gpu)))
            }
            (_, Some(_)) => {
                Err("sets `lease`, which only an agent with `usage: none` takes".into())
            }
            (UsageReader::Claude, None) => Ok(Limit::Account(Account::Claude)),
            (UsageReader::Codex, None) => Ok(Limit::Account(Account::Codex)),
        }
    }
}

#[cfg(test)]
mod tests;
