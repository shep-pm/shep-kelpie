//! What a project adds to the repo's plugins, `[app.dogs.kelpie.repo_setup]`
//!
//! Kelpie loads every plugin the repo turns on in `.claude/settings.json`,
//! and the repo's own `.claude` files, into the calls of the worker, the
//! reviewers and the project manager (`crate::skills`). The repo's files say
//! what loads, so a project narrows nothing here. It adds a plugin the repo
//! does not name, in `extra`. The other keys still load and nothing reads them.
//! Every key is optional, and a value is a string, a list of strings or a table
//! of strings, which lookout edits.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::ports::Role;

/// The plugins the repo does not turn on, which load too, and the keys that
/// narrowed what the repo sets up
///
/// Every key but `extra` is read by nothing: the repo's `.claude` files decide.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(title = "Repo setup")]
pub struct RepoSetup {
    /// Read by nothing: the plugins `.claude/settings.json` turns on load
    #[serde(default = "yes")]
    pub plugins: bool,
    /// Read by nothing: the repo's own `.claude` files on the default branch load
    #[serde(default = "yes")]
    pub own: bool,
    /// Read by nothing: the worker, the reviewers and the project manager get what the repo sets up
    #[serde(default = "every_role")]
    pub roles: Vec<CallRole>,
    /// Read by nothing: every part a plugin holds loads
    #[serde(default = "every_part")]
    pub parts: Vec<Part>,
    /// Read by nothing: a plugin the repo turns off is not loaded
    #[serde(default)]
    pub skip: Vec<PluginId>,
    /// Plugins the repo does not turn on, each as `name@marketplace` with
    /// its marketplace's source as one string: `github:owner/name`,
    /// `git:<url>` or `directory:<folder inside the checkout>`, and
    /// `#<branch or tag>` after a `github` or `git` address
    #[serde(default)]
    pub extra: BTreeMap<PluginId, MarketplaceSource>,
}

fn yes() -> bool {
    true
}

fn every_part() -> Vec<Part> {
    Part::ALL.to_vec()
}

fn every_role() -> Vec<CallRole> {
    CallRole::ALL.to_vec()
}

impl Default for RepoSetup {
    fn default() -> Self {
        Self {
            plugins: true,
            own: true,
            roles: every_role(),
            parts: every_part(),
            skip: Vec::new(),
            extra: BTreeMap::new(),
        }
    }
}

/// One part of a plugin, written as the folder or file that holds it
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
// schemars describes a `try_from` type by its source, so the bound goes here.
#[schemars(extend("pattern" = "^(skills|agents|commands|hooks|mcp)$"))]
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
    const ALL: [Self; 5] = [
        Self::Skills,
        Self::Agents,
        Self::Commands,
        Self::Hooks,
        Self::Mcp,
    ];

    /// The name the setting writes it as, which is its folder where it has one
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

impl TryFrom<String> for Part {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::ALL
            .into_iter()
            .find(|part| part.as_str() == value)
            .ok_or("must be skills, agents, commands, hooks or mcp")
    }
}

/// A call's role, written as the labels of a lamb carry it
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
// schemars describes a `try_from` type by its source, so the bound goes here.
#[schemars(extend("pattern" = "^(worker|reviewer|pm)$"))]
pub struct CallRole(#[schemars(with = "String")] Role);

impl CallRole {
    const ALL: [Self; 3] = [Self(Role::Worker), Self(Role::Reviewer), Self(Role::Pm)];
}

impl TryFrom<String> for CallRole {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value == "issue-writer" {
            return Err(
                "`issue-writer` went with the issue writer: delete it, or run \
                        `shep kelpie settings migrate`, which does",
            );
        }
        Self::ALL
            .into_iter()
            .find(|role| role.0.as_str() == value)
            .ok_or(
                "must be worker, reviewer or pm: `advisor` and `researcher` \
                 went with the ruling panel, so delete them",
            )
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
