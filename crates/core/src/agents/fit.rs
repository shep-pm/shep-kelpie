//! What an agent file says the project manager and the board choose it by

use serde::Deserialize;

use crate::settings::NonBlank;

/// How much work an agent is trusted with: a work item names the least it needs
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tier {
    /// Mechanical work
    Light,
    /// Ordinary work, which an agent that names no tier reads as
    #[default]
    Standard,
    /// Work that needs the strongest agent
    Heavy,
}

/// The kinds of work an agent takes: lowercase words, at least one, none twice
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "Vec<String>")]
pub struct Kinds(Vec<String>);

impl Kinds {
    /// The kinds, in the order the file lists them
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }
}

impl TryFrom<Vec<String>> for Kinds {
    type Error = String;

    fn try_from(kinds: Vec<String>) -> Result<Self, Self::Error> {
        if kinds.is_empty() {
            return Err("must name at least one kind: leave `kinds` out to take every kind".into());
        }
        for (at, kind) in kinds.iter().enumerate() {
            let word = !kind.is_empty()
                && kind
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
            if !word {
                return Err(format!(
                    "`{kind}` is not a kind: use lowercase letters, digits and `-`"
                ));
            }
            if kinds[..at].contains(kind) {
                return Err(format!("`{kind}` is listed twice"));
            }
        }
        Ok(Self(kinds))
    }
}

/// The keys an implementer or reviewer file is chosen by, each absent when the file sets none
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fit {
    /// What it is good at, in the maintainer's words, for the project manager
    pub description: Option<NonBlank>,
    /// The kinds of work it takes. None takes every kind.
    pub kinds: Option<Kinds>,
    /// The least a work item may need of it. See [`Fit::tier`].
    pub tier: Option<Tier>,
}

impl Fit {
    /// The agent's tier, [`Tier::Standard`] when its file names none
    pub fn tier(&self) -> Tier {
        self.tier.unwrap_or_default()
    }
}
