//! Baselines: what a role does, with no binding to run it
//!
//! A baseline is a shipped agent file that says what a role does, its `role`
//! and a body. A binding, the `harness`, `model` and `effort` and the keys
//! around them, says what runs it. A file reaches a baseline with `extends:
//! kelpie:<name>`, and one whose merged keys bind nothing is unbound: the book
//! leaves it out and names it, and `shep kelpie agents new` binds it. A baseline
//! that still carries its binding keys runs with no file.

use std::path::{Path, PathBuf};

use super::{
    Agent, DEFAULTS, DEFECT_HUNTER, PM, REPRESENTATIVE, Role, SECURITY_REVIEWER, TRIAGER, extends,
    front,
};
use crate::settings::AgentName;

/// One of kelpie's own role baselines, embedded from `agents/`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Baseline {
    /// Its name, which `extends: kelpie:<name>` and `--baseline` give
    pub name: &'static str,
    /// Its file as shipped
    pub text: &'static str,
}

impl Baseline {
    /// The role its file names, if the file parses far enough to say
    pub fn role(&self) -> Option<Role> {
        front::role(self.text)
    }
}

/// Every baseline kelpie ships
pub const ALL: [Baseline; 7] = [
    Baseline {
        name: DEFECT_HUNTER,
        text: include_str!("../../../../agents/defect-hunter.md"),
    },
    Baseline {
        name: "standards",
        text: include_str!("../../../../agents/standards.md"),
    },
    Baseline {
        name: "spec",
        text: include_str!("../../../../agents/spec.md"),
    },
    Baseline {
        name: SECURITY_REVIEWER,
        text: include_str!("../../../../agents/security-reviewer.md"),
    },
    Baseline {
        name: PM,
        text: include_str!("../../../../agents/pm.md"),
    },
    Baseline {
        name: REPRESENTATIVE,
        text: include_str!("../../../../agents/representative.md"),
    },
    Baseline {
        name: TRIAGER,
        text: include_str!("../../../../agents/triager.md"),
    },
];

/// The baseline called `name`, if kelpie ships one
pub fn named(name: &str) -> Option<&'static Baseline> {
    ALL.iter().find(|b| b.name == name)
}

/// Every baseline for `role`, in the order kelpie ships them
pub fn of_role(role: Role) -> Vec<&'static Baseline> {
    ALL.iter().filter(|b| b.role() == Some(role)).collect()
}

/// The command that writes the agent file `name`, for `role`
///
/// `baseline` is the one the file builds on, when known. It is named only
/// for a role with several baselines, and `--name` only where the file's name
/// is not the one `agents new` would choose.
pub fn new_command(role: Role, baseline: Option<&str>, name: &str) -> String {
    let own = of_role(role);
    let mut command = format!("shep kelpie agents new {}", role.as_str());
    if let (Some(baseline), true) = (baseline, own.len() > 1) {
        command = format!("{command} --baseline {baseline}");
    }
    let default = baseline.or(match own.as_slice() {
        [only] => Some(only.name),
        _ => None,
    });
    if default != Some(name) {
        command = format!("{command} --name {name}");
    }
    command
}

// The shipped file `shipped` that `extends` (`kelpie:<shipped>`) names, among
// the whole agents and `baselines`.
pub(super) fn shipped_text(
    extends: &str,
    shipped: &str,
    baselines: &[Baseline],
) -> Result<extends::ParentText<'static>, String> {
    (DEFAULTS.iter())
        .find(|(default, _)| *default == shipped)
        .map(|(_, text)| *text)
        .or_else(|| baselines.iter().find(|b| b.name == shipped).map(|b| b.text))
        .map(extends::ParentText::Default)
        .ok_or_else(|| {
            if let Some(why) = front::gone(shipped) {
                return format!("`extends: {extends}`: {why}");
            }
            let own: Vec<&str> = (DEFAULTS.iter().map(|(name, _)| *name))
                .chain(baselines.iter().map(|b| b.name))
                .collect();
            format!(
                "`extends: {extends}` names no shipped agent: kelpie's own are {}",
                own.join(", ")
            )
        })
}

/// The agent the file `name` holding `text` would be, as the loader reads it
///
/// The file may extend only kelpie's own agents, since it is not yet beside
/// any other.
///
/// # Errors
///
/// The message the loader gives for the file.
pub fn check(name: &str, text: &str) -> Result<Agent, String> {
    let parent = |parent: &str| match parent.strip_prefix(extends::SHIPPED) {
        Some(shipped) => shipped_text(parent, shipped, &ALL),
        None => Err(format!(
            "`extends: {parent}` names no shipped agent: write `{}{parent}`",
            extends::SHIPPED
        )),
    };
    let merged = extends::merged(name, text, &parent)?;
    front::parse_named(name, &merged.text).map_err(|message| merged.place(message))
}

/// An agent file that binds nothing, so the book leaves it out
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unbound {
    /// The file's name
    pub name: AgentName,
    /// The file
    pub path: PathBuf,
    /// The role its merged keys name
    pub role: Role,
    // The baseline the file extends directly, if it extends one.
    baseline: Option<String>,
}

impl Unbound {
    pub(super) fn new(name: AgentName, path: &Path, role: Role, baseline: Option<String>) -> Self {
        Self {
            name,
            path: path.to_owned(),
            role,
            baseline,
        }
    }

    /// The command that binds it
    pub fn command(&self) -> String {
        new_command(self.role, self.baseline.as_deref(), self.name.as_str())
    }
}
