//! The skill each step runs
//!
//! Every step kelpie drives an agent through names a skill, by default one
//! from kelpie's vendored copy of mattpocock/skills. Each reaches Claude
//! Code as a plugin folder passed with `--plugin-dir`, so an override is
//! just another folder. A step whose skill cannot load runs kelpie's own
//! prompt instead, and the runner's log and `status` say why.
//!
//! Most steps start their prompt with the skill's slash command. `tests`
//! and `pr` happen inside a worker's turn, so the worker's instructions name
//! their skills instead.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::settings::{SkillChoice, SkillName, StepSkills};

mod vendored;

pub use vendored::{PIN, PLUGIN, UPSTREAM};

/// A step kelpie drives an agent through
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Step {
    /// Triaging an issue
    Triage,
    /// Splitting a plan into work items
    Planning,
    /// Writing a spec
    Spec,
    /// The worker's first turn on an issue
    Implement,
    /// How the worker writes tests
    Tests,
    /// Each Claude review round
    Review,
    /// The worker's turn on a red CI run
    Ci,
    /// How the worker writes its pull request's body
    Pr,
    /// A session reset's handoff
    Reset,
    /// A retro for the lessons file
    Retro,
}

impl Step {
    /// Every step, in the order a work item meets them
    pub const ALL: [Self; 10] = [
        Self::Triage,
        Self::Planning,
        Self::Spec,
        Self::Implement,
        Self::Tests,
        Self::Review,
        Self::Ci,
        Self::Pr,
        Self::Reset,
        Self::Retro,
    ];

    /// The step's key in `[skills]`
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Triage => "triage",
            Self::Planning => "planning",
            Self::Spec => "spec",
            Self::Implement => "implement",
            Self::Tests => "tests",
            Self::Review => "review",
            Self::Ci => "ci",
            Self::Pr => "pr",
            Self::Reset => "reset",
            Self::Retro => "retro",
        }
    }

    /// The step's skill in kelpie's vendored copy
    pub fn default_skill(self) -> &'static str {
        match self {
            Self::Triage => "triage",
            Self::Planning => "to-tickets",
            Self::Spec => "to-spec",
            Self::Implement => "implement",
            Self::Tests => "tdd",
            Self::Review => "code-review",
            Self::Ci => "diagnosing-bugs",
            Self::Pr => "pr",
            Self::Reset => "handoff",
            Self::Retro => "retro",
        }
    }
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One step's skill, as `status` shows it
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepSkill {
    /// The step
    pub step: Step,
    /// The skill's slash command, such as `/mattpocock:tdd`, or none where
    /// kelpie's own prompt runs
    pub skill: Option<String>,
    /// Why the skill the project chose could not load, where it could not
    pub fallback: Option<String>,
    #[serde(skip)]
    plugin: Option<PathBuf>,
}

/// Each step's skill, as the runner loaded it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skills {
    steps: Vec<StepSkill>,
    plugins: Vec<PathBuf>,
}

impl Skills {
    /// Loads each step's skill, writing the plugins it needs under `folder`
    ///
    /// The vendored plugin goes in `folder/mattpocock`, and a step's own
    /// skill folder is copied into a plugin at `folder/<step>`. Nothing here
    /// fails: a skill that cannot load leaves its step on kelpie's prompt.
    pub fn load(chosen: &StepSkills, folder: &Path) -> Self {
        let vendored = folder.join(PLUGIN);
        let written = vendored::write(&vendored).map_err(|e| {
            format!(
                "cannot write kelpie's copy of mattpocock/skills to {}: {e}",
                vendored.display()
            )
        });
        let steps: Vec<StepSkill> = Step::ALL
            .into_iter()
            .map(|step| {
                let loaded = match chosen.choice(step) {
                    None => written
                        .clone()
                        .map(|()| Some(Loaded::new(&vendored, PLUGIN, step.default_skill()))),
                    Some(SkillChoice::None {}) => Ok(None),
                    Some(SkillChoice::Path { path }) => {
                        wrap(path, &folder.join(step.as_str()), step).map(Some)
                    }
                    Some(SkillChoice::Plugin { plugin, skill }) => {
                        in_plugin(plugin, skill).map(Some)
                    }
                };
                match loaded {
                    Ok(Some(loaded)) => StepSkill {
                        step,
                        skill: Some(loaded.command),
                        fallback: None,
                        plugin: Some(loaded.plugin),
                    },
                    Ok(None) => StepSkill {
                        step,
                        skill: None,
                        fallback: None,
                        plugin: None,
                    },
                    Err(reason) => StepSkill {
                        step,
                        skill: None,
                        fallback: Some(reason),
                        plugin: None,
                    },
                }
            })
            .collect();
        let mut plugins: Vec<PathBuf> = steps.iter().filter_map(|s| s.plugin.clone()).collect();
        plugins.sort();
        plugins.dedup();
        Self { steps, plugins }
    }

    /// `prompt`, started with `step`'s slash command where it has a skill
    pub fn invoke(&self, step: Step, prompt: &str) -> String {
        match self.command(step) {
            Some(command) => format!("{command} {prompt}"),
            None => prompt.to_owned(),
        }
    }

    /// `step`'s slash command, such as `/mattpocock:tdd`, if it has a skill
    pub fn command(&self, step: Step) -> Option<&str> {
        self.steps
            .iter()
            .find(|s| s.step == step)
            .and_then(|s| s.skill.as_deref())
    }

    /// Every plugin folder a step's skill is in, for `claude --plugin-dir`
    pub fn plugin_dirs(&self) -> &[PathBuf] {
        &self.plugins
    }

    /// Each step's skill, in [`Step::ALL`]'s order
    pub fn status(&self) -> &[StepSkill] {
        &self.steps
    }

    /// A log line for each step whose skill could not load
    pub fn notices(&self) -> impl Iterator<Item = String> + '_ {
        self.steps.iter().filter_map(|s| {
            s.fallback.as_ref().map(|reason| {
                format!(
                    "the {} step's skill cannot load, so it runs kelpie's own prompt: {reason}",
                    s.step
                )
            })
        })
    }
}

/// Splits the slash command that starts `prompt`, if one does, from the rest
///
/// A command only runs from the very start of a prompt, so text put ahead
/// of an invoked prompt has to go after its command.
pub fn split_command(prompt: &str) -> (Option<&str>, &str) {
    match prompt.split_once(' ') {
        Some((command, rest)) if command.starts_with('/') && command.contains(':') => {
            (Some(command), rest)
        }
        _ => (None, prompt),
    }
}

// A skill found in a plugin folder, and the slash command that runs it
struct Loaded {
    plugin: PathBuf,
    command: String,
}

impl Loaded {
    fn new(plugin: &Path, plugin_name: &str, skill: &str) -> Self {
        Self {
            plugin: plugin.to_owned(),
            command: format!("/{plugin_name}:{skill}"),
        }
    }
}

// A loose skill folder becomes a plugin of its own, `kelpie-<step>`, so it
// reaches Claude Code the way every other skill does. Claude Code will not
// load a plugin's skill from outside the plugin's folder, so it is copied.
fn wrap(skill: &Path, into: &Path, step: Step) -> Result<Loaded, String> {
    if !skill.join("SKILL.md").is_file() {
        return Err(format!("{} holds no SKILL.md", skill.display()));
    }
    let name = skill
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| SkillName::try_from(n.to_owned()).ok())
        .ok_or_else(|| format!("{} is not named like a skill", skill.display()))?;
    let plugin_name = format!("kelpie-{step}");
    let written = (|| {
        match fs::remove_dir_all(into) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        fs::create_dir_all(into.join(".claude-plugin"))?;
        let manifest = serde_json::json!({ "name": plugin_name });
        fs::write(
            into.join(".claude-plugin/plugin.json"),
            manifest.to_string(),
        )?;
        copy_folder(skill, &into.join("skills").join(name.as_str()))
    })();
    written.map_err(|e| {
        format!(
            "cannot copy {} into {}: {e}",
            skill.display(),
            into.display()
        )
    })?;
    Ok(Loaded::new(into, &plugin_name, name.as_str()))
}

// A skill in a plugin the project names: in its `skills/` folder, or in a
// folder its manifest's `skills` list names.
fn in_plugin(plugin: &Path, skill: &SkillName) -> Result<Loaded, String> {
    let manifest_path = plugin.join(".claude-plugin/plugin.json");
    let manifest: serde_json::Value = fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .ok_or_else(|| {
            format!(
                "{} is not a readable plugin manifest",
                manifest_path.display()
            )
        })?;
    let name = manifest["name"]
        .as_str()
        .filter(|n| !n.is_empty())
        .ok_or_else(|| format!("{} names no plugin", manifest_path.display()))?;
    let listed = manifest["skills"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.as_str())
        .map(|entry| plugin.join(entry));
    let found = std::iter::once(plugin.join("skills").join(skill.as_str()))
        .chain(listed)
        .any(|folder| {
            folder.file_name().is_some_and(|n| n == skill.as_str())
                && folder.join("SKILL.md").is_file()
        });
    if !found {
        return Err(format!(
            "the plugin in {} has no skill {}",
            plugin.display(),
            skill.as_str()
        ));
    }
    Ok(Loaded::new(plugin, name, skill.as_str()))
}

// Copies `from` into `to`, following links to files and skipping links to
// folders, which could loop.
fn copy_folder(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_folder(&source, &target)?;
        } else if kind.is_file() || source.is_file() {
            fs::copy(&source, &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
