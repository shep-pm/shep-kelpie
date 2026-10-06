//! An agent file's frontmatter, and the agent it defines
//!
//! The frontmatter is tagged by its `role`, so a key only a reviewer takes,
//! such as `paths`, is refused on an implementer by name, and the other
//! way round. It is read in two passes, the role and then that role's keys,
//! rather than as a serde-tagged enum, which buffers the keys and so loses
//! the line a refusal names.

use std::num::NonZeroU32;
use std::path::PathBuf;

use serde::Deserialize;

use super::{Agent, Role, Runs};
use crate::forwarder::Upstream;
use crate::review_bot::Bot;

mod bot;

use crate::settings::{
    Account, AgentHarness, ContextSize, Effort, Endpoint, EndpointUrl, Harness, LeaseName, Limit,
    LocalCommand, ModelServer, NonBlank, RoleModel, UsageReader,
};
use bot::at_key;

// An agent file's frontmatter, by its role
#[derive(Debug)]
enum Front {
    Implementer(Implementer),
    Reviewer(Reviewer),
    IssueWriter(OnClaude),
    Pm(OnClaude),
}

// The first pass: the role alone, whatever else is there
#[derive(Debug, Deserialize)]
struct Tag {
    role: Role,
}

// An implementer's keys: a session on a harness
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Implementer {
    #[serde(rename = "role")]
    _role: Role,
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

// The issue writer's and the project manager's keys: a Claude Code
// session, since each is held by a Claude Code hook
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OnClaude {
    #[serde(rename = "role")]
    _role: Role,
    harness: Harness,
    model: NonBlank,
    effort: Effort,
}

// What runs a reviewer: a session on a harness, a command, an endpoint or a
// review bot
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ReviewerHarness {
    ClaudeCode,
    Pi,
    Codex,
    Command,
    Endpoint,
    Bot,
    #[cfg(test)]
    StandIn,
}

// A reviewer's keys, which its harness narrows
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reviewer {
    #[serde(rename = "role")]
    _role: Role,
    harness: ReviewerHarness,
    #[serde(default)]
    model: Option<NonBlank>,
    #[serde(default)]
    effort: Option<Effort>,
    #[serde(default)]
    usage: Option<UsageReader>,
    #[serde(default)]
    lease: Option<LeaseName>,
    #[serde(default)]
    url: Option<EndpointUrl>,
    #[serde(default)]
    context: Option<ContextSize>,
    #[serde(default)]
    command: Option<PathBuf>,
    #[serde(default)]
    ollama: Option<EndpointUrl>,
    #[serde(default)]
    ollama_model: Option<NonBlank>,
    #[serde(default)]
    paths: Vec<NonBlank>,
    #[serde(default)]
    second_look: bool,
    #[serde(default)]
    bot: Option<Bot>,
    #[serde(default)]
    reviews: Option<NonZeroU32>,
    #[serde(default)]
    hours: Option<NonZeroU32>,
    #[serde(default)]
    reviews_on_ready: Option<bool>,
    #[serde(default)]
    rounds: Option<NonZeroU32>,
}

// A session's keys, whichever role runs it
struct SessionKeys<'a> {
    harness: Harness,
    model: &'a NonBlank,
    effort: Effort,
    usage: Option<UsageReader>,
    lease: Option<&'a LeaseName>,
    url: Option<&'a EndpointUrl>,
    context: Option<ContextSize>,
}

/// The agent an agent file's `text` defines, or what is wrong with it
pub(super) fn parse(text: &str) -> Result<Agent, String> {
    let Some((front, body)) = split(text) else {
        return Err("must start with a `---` line, then the YAML frontmatter, \
                    then a `---` line of its own"
            .into());
    };
    // A blank first line stands for the opening `---`, so a message's line is the file's.
    let yaml = format!("\n{front}");
    let refused = |e: serde_saphyr::Error| yaml_error(&yaml, &e);
    let front = match serde_saphyr::from_str::<Tag>(&yaml).map_err(refused)?.role {
        Role::Implementer => Front::Implementer(serde_saphyr::from_str(&yaml).map_err(refused)?),
        Role::Reviewer => Front::Reviewer(serde_saphyr::from_str(&yaml).map_err(refused)?),
        Role::IssueWriter => Front::IssueWriter(serde_saphyr::from_str(&yaml).map_err(refused)?),
        Role::Pm => Front::Pm(serde_saphyr::from_str(&yaml).map_err(refused)?),
    };
    let body = body.trim();
    let prompt = (!body.is_empty()).then(|| body.to_owned());
    match front {
        Front::Implementer(keys) => Ok(Agent {
            role: Role::Implementer,
            runs: session(&SessionKeys {
                harness: keys.harness,
                model: &keys.model,
                effort: keys.effort,
                usage: keys.usage,
                lease: keys.lease.as_ref(),
                url: keys.url.as_ref(),
                context: keys.context,
            })?,
            prompt,
            paths: Vec::new(),
            second_look: false,
        }),
        Front::Reviewer(keys) => {
            let runs = keys.runs(prompt.is_some(), &yaml)?;
            Ok(Agent {
                role: Role::Reviewer,
                runs,
                prompt,
                paths: keys.paths,
                second_look: keys.second_look,
            })
        }
        Front::IssueWriter(keys) => keys.agent(
            Role::IssueWriter,
            prompt,
            &yaml,
            (
                "the issue writer",
                "`--interactive` starts `claude`, and its guard is a Claude Code hook",
            ),
        ),
        Front::Pm(keys) => keys.agent(
            Role::Pm,
            prompt,
            &yaml,
            (
                "the project manager",
                "a Claude Code hook is what holds its writes to its notes",
            ),
        ),
    }
}

impl OnClaude {
    /// The agent of `role` these keys and `prompt` make, or why not: `who`
    /// names it and why it runs on Claude Code alone
    fn agent(
        &self,
        role: Role,
        prompt: Option<String>,
        yaml: &str,
        (who, why): (&str, &str),
    ) -> Result<Agent, String> {
        if self.harness != Harness::ClaudeCode {
            let name = self.harness.as_str();
            return Err(at_key(
                yaml,
                "harness",
                &format!("{who} runs on claude-code, not {name}: {why}"),
            ));
        }
        if prompt.is_none() {
            return Err(format!(
                "{who}'s prompt is the file's body: write it below the closing `---`"
            ));
        }
        Ok(Agent {
            role,
            runs: session(&SessionKeys {
                harness: self.harness,
                model: &self.model,
                effort: self.effort,
                usage: None,
                lease: None,
                url: None,
                context: None,
            })?,
            prompt,
            paths: Vec::new(),
            second_look: false,
        })
    }
}

impl Reviewer {
    /// What runs it, or why its keys cannot: `prompted` says whether the
    /// file has a body
    fn runs(&self, prompted: bool, yaml: &str) -> Result<Runs, String> {
        let harness = match self.harness {
            ReviewerHarness::ClaudeCode => Harness::ClaudeCode,
            ReviewerHarness::Pi => Harness::Pi,
            ReviewerHarness::Codex => Harness::Codex,
            #[cfg(test)]
            ReviewerHarness::StandIn => Harness::StandIn,
            ReviewerHarness::Command => {
                self.no_bot_keys("command", yaml)?;
                return self.command(prompted).map(Runs::Command);
            }
            ReviewerHarness::Endpoint => {
                self.no_bot_keys("endpoint", yaml)?;
                return self.endpoint(prompted).map(Runs::Endpoint);
            }
            ReviewerHarness::Bot => {
                let key = if self.bot.is_some() { "bot" } else { "harness" };
                return self
                    .bot(prompted)
                    .map(Runs::Bot)
                    .map_err(|m| at_key(yaml, key, &m));
            }
        };
        let name = harness.as_str();
        self.no_bot_keys(name, yaml)?;
        if self.command.is_some() || self.ollama.is_some() || self.ollama_model.is_some() {
            return Err(format!(
                "runs a session on {name}, which takes no `command`, `ollama` or \
                 `ollama_model`"
            ));
        }
        let (Some(model), Some(effort)) = (&self.model, self.effort) else {
            return Err(format!(
                "runs a session on {name}, which needs `model` and `effort`"
            ));
        };
        if !prompted {
            return Err(format!(
                "runs a session on {name}, whose prompt is the file's body: write it \
                 below the closing `---`"
            ));
        }
        session(&SessionKeys {
            harness,
            model,
            effort,
            usage: self.usage,
            lease: self.lease.as_ref(),
            url: self.url.as_ref(),
            context: self.context,
        })
    }

    // A command keeps the README's contract, which gives it no model, prompt
    // or findings to look again at.
    fn command(&self, prompted: bool) -> Result<LocalCommand, String> {
        self.on_its_own("command", prompted)?;
        let session_keys = self.model.is_some()
            || self.effort.is_some()
            || self.url.is_some()
            || self.context.is_some();
        if session_keys {
            return Err(
                "runs a command, which takes no `model`, `effort`, `url` or \
                        `context`"
                    .into(),
            );
        }
        let Some(command) = &self.command else {
            return Err("runs a command, which needs its path as `command`".into());
        };
        if !(command.is_absolute() || command.starts_with("~")) {
            return Err("`command` must start with `/` or `~/`".into());
        }
        match (&self.ollama, &self.ollama_model, &self.lease) {
            (Some(_), _, None) => Err("`ollama` needs a lease, such as `lease: gpu`: \
                                       without it kelpie reads `/api/ps` while another \
                                       round may hold the model, and would rule on that \
                                       round's model"
                .into()),
            (None, Some(_), _) => Err("`ollama_model` needs `ollama`".into()),
            _ => Ok(LocalCommand {
                command: command.clone(),
                lease: self.lease.clone(),
                ollama: self.ollama.clone(),
                ollama_model: self.ollama_model.clone(),
            }),
        }
    }

    fn endpoint(&self, prompted: bool) -> Result<Endpoint, String> {
        self.on_its_own("endpoint", prompted)?;
        let other = self.effort.is_some()
            || self.command.is_some()
            || self.ollama.is_some()
            || self.ollama_model.is_some();
        if other {
            return Err("runs on an endpoint, which takes no `effort`, `command`, \
                        `ollama` or `ollama_model`: its url is the Ollama host"
                .into());
        }
        let (Some(url), Some(model), Some(context)) = (&self.url, &self.model, self.context) else {
            return Err("runs on an endpoint, which needs the server as `url`, its \
                        `model` and the model's context size as `context`"
                .into());
        };
        Ok(Endpoint {
            url: url.clone(),
            model: model.clone(),
            context,
            lease: self.lease.clone(),
        })
    }

    // What a reviewer that runs on its own, not in a session, cannot take.
    fn on_its_own(&self, harness: &str, prompted: bool) -> Result<(), String> {
        if self.usage.is_some() {
            return Err(format!(
                "runs on {harness}, which spends no account, so it takes no `usage`"
            ));
        }
        if self.second_look {
            return Err(format!(
                "runs on {harness}, which cannot be shown its own findings, so it \
                 takes no `second_look`"
            ));
        }
        if prompted {
            return Err(format!(
                "runs on {harness}, which writes its own prompt: leave the body empty, \
                 and keep notes as `#` lines in the frontmatter"
            ));
        }
        Ok(())
    }
}

/// A session's model and limit, or why its keys cannot reach that model
fn session(keys: &SessionKeys<'_>) -> Result<Runs, String> {
    Ok(Runs::Session {
        model: role_model(keys)?,
        limit: limit(keys)?,
    })
}

fn role_model(keys: &SessionKeys<'_>) -> Result<RoleModel, String> {
    let harness = match (keys.harness, keys.url, keys.context) {
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
        model: keys.model.clone(),
        effort: keys.effort,
        harness,
    })
}

/// What holds a session's calls back, or why its keys do not say
///
/// The reader must be the harness's own: an agent on Claude Code spends
/// the Claude account whatever its `usage` says.
fn limit(keys: &SessionKeys<'_>) -> Result<Limit, String> {
    let own = match keys.harness {
        Harness::ClaudeCode => UsageReader::Claude,
        Harness::Pi => UsageReader::None,
        Harness::Codex => UsageReader::Codex,
        #[cfg(test)]
        Harness::StandIn => keys.usage.unwrap_or(UsageReader::Claude),
    };
    let usage = keys.usage.unwrap_or(own);
    if usage != own {
        return Err(format!(
            "runs on {}, whose usage is read with `{}`, so it cannot set \
             `usage: {}`: leave `usage` out",
            keys.harness.as_str(),
            own.as_str(),
            usage.as_str()
        ));
    }
    match (usage, keys.lease) {
        (UsageReader::None, lease) => {
            Ok(Limit::Lease(lease.cloned().unwrap_or_else(LeaseName::gpu)))
        }
        (_, Some(_)) => Err("sets `lease`, which only an agent with `usage: none` takes".into()),
        (UsageReader::Claude, None) => Ok(Limit::Account(Account::Claude)),
        (UsageReader::Codex, None) => Ok(Limit::Account(Account::Codex)),
    }
}

/// The agent `name`'s file `text` defines, or what is wrong with it
///
/// A review bot's file is named for its bot: the dog books each bot's
/// window from that file, so a second file could not hold a window of its own.
pub(super) fn parse_named(name: &str, text: &str) -> Result<Agent, String> {
    let agent = parse(text)?;
    match agent.runs.bot() {
        Some(bot) if name != bot.bot.as_str() => {
            let yaml = split(text)
                .map(|(front, _)| format!("\n{front}"))
                .unwrap_or_default();
            Err(at_key(
                &yaml,
                "bot",
                &format!(
                    "runs {}, so it must be named `{}.md`: each bot has one file, which \
                     holds its account's window",
                    bot.bot, bot.bot
                ),
            ))
        }
        _ => Ok(agent),
    }
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
