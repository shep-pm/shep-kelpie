//! Each step's skill, `[app.dogs.kelpie.skills]`
//!
//! A step left out runs its default from kelpie's vendored copy of
//! mattpocock/skills. A project points a step at a skill folder of its own,
//! at a skill in a Claude Code plugin, or at none, which runs kelpie's own
//! prompt. `crate::skills` says which step is which.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;

use crate::skills::Step;

/// The skill a project chose for each step, its default where left out
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepSkills {
    /// Triaging an issue, `triage` by default
    #[serde(default)]
    pub triage: Option<SkillChoice>,
    /// Writing a spec, `to-spec` by default
    #[serde(default)]
    pub spec: Option<SkillChoice>,
    /// The worker's first turn on an issue, `implement` by default
    #[serde(default)]
    pub implement: Option<SkillChoice>,
    /// How the worker writes tests, `tdd` by default
    #[serde(default)]
    pub tests: Option<SkillChoice>,
    /// The worker's turn on a red CI run, `diagnosing-bugs` by default
    #[serde(default)]
    pub ci_fix: Option<SkillChoice>,
    /// How the worker writes its pull request's body, `pr` by default
    #[serde(default)]
    pub pr: Option<SkillChoice>,
    /// A session reset's handoff, `handoff` by default
    #[serde(default)]
    pub reset: Option<SkillChoice>,
    /// A finished work item's retro, `retro` by default
    #[serde(default)]
    pub retro: Option<SkillChoice>,
}

impl StepSkills {
    /// What the project chose for `step`, or nothing for the default
    pub fn choice(&self, step: Step) -> Option<&SkillChoice> {
        match step {
            Step::Triage => self.triage.as_ref(),
            Step::Spec => self.spec.as_ref(),
            Step::Implement => self.implement.as_ref(),
            Step::Tests => self.tests.as_ref(),
            Step::CiFix => self.ci_fix.as_ref(),
            Step::Pr => self.pr.as_ref(),
            Step::Reset => self.reset.as_ref(),
            Step::Retro => self.retro.as_ref(),
        }
    }

    // Every folder the choices name, which expand `~/` and are taken from
    // the settings file's folder.
    // Destructured without `..`, so a new step fails to compile until it is here.
    pub(super) fn paths_mut(&mut self) -> impl Iterator<Item = &mut PathBuf> {
        let Self {
            triage,
            spec,
            implement,
            tests,
            ci_fix,
            pr,
            reset,
            retro,
        } = self;
        [triage, spec, implement, tests, ci_fix, pr, reset, retro]
            .into_iter()
            .filter_map(|choice| match choice {
                Some(SkillChoice::Path { path }) => Some(path),
                Some(SkillChoice::Plugin { plugin, .. }) => Some(plugin),
                Some(SkillChoice::None {}) | None => None,
            })
    }
}

/// What one step runs in place of its default skill
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SkillChoice {
    /// No skill: kelpie's own prompt
    None {},
    /// A skill folder holding a `SKILL.md`. A leading `~/` is the home
    /// folder, and a relative path is taken from the settings file's folder.
    Path {
        /// The folder
        path: PathBuf,
    },
    /// A skill in a Claude Code plugin's folder, the one `claude
    /// --plugin-dir` takes. Its path reads as `path` does.
    Plugin {
        /// The plugin's folder, holding `.claude-plugin/plugin.json`
        plugin: PathBuf,
        /// The skill's name within the plugin
        skill: SkillName,
    },
}

/// A skill's name: letters, digits, `-`, `_` and `.`, not starting with `.`
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
// schemars describes a `try_from` type by its source, so the bound goes here.
#[schemars(extend("pattern" = "^[A-Za-z0-9_-][A-Za-z0-9_.-]*$"))]
pub struct SkillName(String);

impl SkillName {
    /// The name as written
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SkillName {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
        if value.is_empty() || value.starts_with('.') || !value.chars().all(allowed) {
            return Err("must be a skill's name: letters, digits, - _ .");
        }
        Ok(Self(value))
    }
}
