//! What a project adds to the repo's plugins, `[app.dogs.kelpie.repo_setup]`
//!
//! Kelpie loads every plugin the repo turns on in `.claude/settings.json`,
//! and the repo's own `.claude` files, into the calls of the worker, the
//! reviewers and the project manager (`crate::skills`). The repo's files say
//! what loads, so a project narrows nothing: `extra` is the only key, and it
//! adds a plugin the repo does not name. A value is a table of strings, which
//! lookout edits.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// The plugins the repo does not turn on, which load too
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(title = "Repo setup")]
pub struct RepoSetup {
    /// Plugins the repo does not turn on, each as `name@marketplace` with
    /// its marketplace's source as one string: `github:owner/name`,
    /// `git:<url>` or `directory:<folder inside the checkout>`, and
    /// `#<branch or tag>` after a `github` or `git` address
    #[serde(default)]
    pub extra: BTreeMap<PluginId, MarketplaceSource>,
}

/// One part of a plugin, as the copy holds it
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// `skills/`
    Skills,
    /// `agents/`
    Agents,
    /// `commands/`
    Commands,
    /// `hooks/`
    Hooks,
    /// The plugin's MCP servers
    Mcp,
}

impl Part {
    /// The name it goes by, which is its folder where it has one
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skills => "skills",
            Self::Agents => "agents",
            Self::Commands => "commands",
            Self::Hooks => "hooks",
            Self::Mcp => "mcp",
        }
    }
}

/// A plugin as `name@marketplace`, each part letters, digits, `-`, `_` and
/// `.`, not starting with `.`
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
#[schemars(extend("pattern" = "^[A-Za-z0-9_-][A-Za-z0-9_.-]*@[A-Za-z0-9_-][A-Za-z0-9_.-]*$"))]
pub struct PluginId(String);

impl PluginId {
    /// The plugin as written
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The marketplace it comes from, after the `@`
    pub fn marketplace(&self) -> &str {
        self.0.split_once('@').map_or("", |(_, market)| market)
    }
}

impl TryFrom<String> for PluginId {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let plain = |part: &str| super::SkillName::try_from(part.to_owned()).is_ok();
        match value.split_once('@') {
            Some((name, market)) if plain(name) && plain(market) => Ok(Self(value)),
            _ => Err("must be name@marketplace: letters, digits, - _ . in each"),
        }
    }
}

/// Where a marketplace is, written as one string: `github:owner/name`,
/// `git:<url>` or `directory:<folder>`, and `#<branch or tag>` after a
/// `github` or `git` address to fetch that ref
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
#[schemars(extend("pattern" = "^(github|git|directory):.+$"))]
pub struct MarketplaceSource {
    kind: String,
    address: String,
    reference: Option<String>,
}

impl TryFrom<String> for MarketplaceSource {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        const WRITTEN: &str = "must be github:owner/name, git:<url> or directory:<folder>";
        let (kind, rest) = value.split_once(':').ok_or(WRITTEN)?;
        if !["github", "git", "directory"].contains(&kind) {
            return Err(WRITTEN);
        }
        let (address, reference) = match rest.rsplit_once('#') {
            Some((address, reference)) if kind != "directory" && !reference.is_empty() => {
                (address, Some(reference.to_owned()))
            }
            _ => (rest, None),
        };
        if address.is_empty() {
            return Err(WRITTEN);
        }
        Ok(Self {
            kind: kind.to_owned(),
            address: address.to_owned(),
            reference,
        })
    }
}

impl MarketplaceSource {
    /// The marketplace as an `extraKnownMarketplaces` entry holds it
    pub fn entry(&self) -> Value {
        let field = match self.kind.as_str() {
            "github" => "repo",
            "git" => "url",
            _ => "path",
        };
        let mut source = Map::new();
        source.insert("source".into(), json!(self.kind));
        source.insert(field.into(), json!(self.address));
        if let Some(reference) = &self.reference {
            source.insert("ref".into(), json!(reference));
        }
        json!({ "source": source })
    }
}
