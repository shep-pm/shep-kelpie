//! The model id each `worker:` label name runs

use schemars::JsonSchema;
use serde::Deserialize;

use super::NonBlank;

/// A model name a `worker:<model>-<effort>` label may use
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelName {
    /// `opus`
    Opus,
    /// `sonnet`
    Sonnet,
    /// `haiku`
    Haiku,
    /// `fable`
    Fable,
}

impl LabelName {
    /// Every name, in the order a message lists them
    pub const ALL: [Self; 4] = [Self::Opus, Self::Sonnet, Self::Haiku, Self::Fable];

    /// The name as a label writes it, or `None` for any other word
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|n| n.as_str() == name)
    }

    /// The name as a label writes it
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Opus => "opus",
            Self::Sonnet => "sonnet",
            Self::Haiku => "haiku",
            Self::Fable => "fable",
        }
    }
}

/// The model id each label name runs, so a new model needs no new build
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabelModels {
    /// What `worker:opus-*` runs. `claude-opus-5-5` when absent.
    #[serde(default = "opus")]
    pub opus: NonBlank,
    /// What `worker:sonnet-*` runs. `claude-sonnet-5-5` when absent.
    #[serde(default = "sonnet")]
    pub sonnet: NonBlank,
    /// What `worker:haiku-*` runs. `claude-haiku-4-5-20251001` when absent.
    #[serde(default = "haiku")]
    pub haiku: NonBlank,
    /// What `worker:fable-*` runs. `claude-fable-5-1` when absent.
    #[serde(default = "fable")]
    pub fable: NonBlank,
}

impl LabelModels {
    /// The model id `name` runs
    #[must_use]
    pub fn id(&self, name: LabelName) -> &str {
        match name {
            LabelName::Opus => self.opus.as_str(),
            LabelName::Sonnet => self.sonnet.as_str(),
            LabelName::Haiku => self.haiku.as_str(),
            LabelName::Fable => self.fable.as_str(),
        }
    }
}

impl Default for LabelModels {
    fn default() -> Self {
        Self {
            opus: opus(),
            sonnet: sonnet(),
            haiku: haiku(),
            fable: fable(),
        }
    }
}

fn opus() -> NonBlank {
    NonBlank("claude-opus-5-5".to_owned())
}

fn sonnet() -> NonBlank {
    NonBlank("claude-sonnet-5-5".to_owned())
}

fn haiku() -> NonBlank {
    NonBlank("claude-haiku-4-5-20251001".to_owned())
}

fn fable() -> NonBlank {
    NonBlank("claude-fable-5-1".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_reads_back_from_how_a_label_writes_it() {
        for name in LabelName::ALL {
            assert_eq!(LabelName::parse(name.as_str()), Some(name));
        }
        assert_eq!(LabelName::parse("Sonnet"), None);
        assert_eq!(LabelName::parse("gpt"), None);
    }

    #[test]
    fn a_table_that_names_one_model_keeps_the_others_at_their_defaults() {
        let labels: LabelModels = toml::from_str("sonnet = \"claude-sonnet-6\"").unwrap();
        assert_eq!(labels.id(LabelName::Sonnet), "claude-sonnet-6");
        assert_eq!(labels.id(LabelName::Opus), "claude-opus-5-5");
        let defaults = LabelModels::default();
        assert_eq!(defaults.id(LabelName::Sonnet), "claude-sonnet-5-5");
        assert_eq!(defaults.id(LabelName::Haiku), "claude-haiku-4-5-20251001");
        assert_eq!(defaults.id(LabelName::Fable), "claude-fable-5-1");
    }

    #[test]
    fn a_blank_or_unknown_entry_is_refused() {
        assert!(toml::from_str::<LabelModels>("sonnet = \" \"").is_err());
        assert!(toml::from_str::<LabelModels>("gpt = \"gpt-6\"").is_err());
    }
}
