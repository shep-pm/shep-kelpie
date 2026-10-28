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
use serde::de::IgnoredAny;

use super::{Agent, Fit, Kinds, Need, Role, Runs, Tier};
use crate::forwarder::Upstream;
use crate::review_bot::Bot;

mod bot;
#[cfg(test)]
mod tests;

use crate::settings::{
    Account, AgentHarness, ContextSize, Effort, Endpoint, EndpointUrl, GatewayName, Harness, Limit,
    LocalCommand, ModelHost, ModelServer, NonBlank, RoleModel, UsageReader,
};
use bot::at_key;

// An agent file's frontmatter, by its role
#[derive(Debug)]
enum Front {
    Implementer(Implementer),
    Reviewer(Reviewer),
    Pm(OnClaude),
    Representative(Expert),
    Triager(Expert),
}

// The first pass: the role alone, whatever else is there
#[derive(Debug, Deserialize)]
struct Tag {
    role: Role,
}

// The `role` key as plain text, so a role no longer in [`Role`] can be named
#[derive(Debug, Deserialize)]
struct Written {
    role: String,
}

/// The roles an agent file may not name, each with the reason its refusal gives
const GONE: [(&str, &str); 3] = [
    ("advisor", "went with the ruling panel"),
    ("researcher", "went with the ruling panel"),
    (
        "issue-writer",
        "went with the issue writer, and an issue is written with kelpie's `issue` skill",
    ),
];

// An implementer's keys: a session on a harness
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Implementer {
    #[serde(rename = "role")]
    _role: Role,
    #[serde(default)]
    needs: Option<Vec<Need>>,
    harness: Harness,
    model: NonBlank,
    effort: Effort,
    #[serde(default)]
    usage: Option<UsageReader>,
    #[serde(default)]
    lease: Option<IgnoredAny>,
    #[serde(default)]
    url: Option<EndpointUrl>,
    #[serde(default)]
    gateway: Option<GatewayName>,
    #[serde(default)]
    context: Option<ContextSize>,
    #[serde(default)]
    tools: Option<NonBlank>,
    #[serde(default)]
    description: Option<NonBlank>,
    #[serde(default)]
    kinds: Option<Kinds>,
    #[serde(default)]
    tier: Option<Tier>,
}

// The project manager's keys: a Claude Code session, since it is held by a
// Claude Code hook
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OnClaude {
    #[serde(rename = "role")]
    _role: Role,
    #[serde(default)]
    needs: Option<Vec<Need>>,
    harness: Harness,
    model: NonBlank,
    effort: Effort,
}

// The representative's and the triager's keys: a session on any harness.
// Neither takes `tools`: each is held to its reads by the kind of call kelpie
// makes.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Expert {
    #[serde(rename = "role")]
    _role: Role,
    #[serde(default)]
    needs: Option<Vec<Need>>,
    harness: Harness,
    model: NonBlank,
    effort: Effort,
    #[serde(default)]
    usage: Option<UsageReader>,
    #[serde(default)]
    lease: Option<IgnoredAny>,
    #[serde(default)]
    url: Option<EndpointUrl>,
    #[serde(default)]
    gateway: Option<GatewayName>,
    #[serde(default)]
    context: Option<ContextSize>,
}

// What runs a reviewer: a session on a harness, a command, an endpoint or a
// review bot
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ReviewerHarness {
    ClaudeCode,
    Pi,
    Codex,
    Goose,
    Command,
    Endpoint,
    Bot,
    #[cfg(any(test, feature = "test-support"))]
    StandIn,
}

// A reviewer's keys, which its harness narrows
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reviewer {
    #[serde(rename = "role")]
    _role: Role,
    #[serde(default)]
    needs: Option<Vec<Need>>,
    harness: ReviewerHarness,
    #[serde(default)]
    model: Option<NonBlank>,
    #[serde(default)]
    effort: Option<Effort>,
    #[serde(default)]
    usage: Option<UsageReader>,
    #[serde(default)]
    lease: Option<IgnoredAny>,
    #[serde(default)]
    url: Option<EndpointUrl>,
    #[serde(default)]
    gateway: Option<GatewayName>,
    #[serde(default)]
    context: Option<ContextSize>,
    #[serde(default)]
    command: Option<PathBuf>,
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
    #[serde(default)]
    review_wait: Option<NonZeroU32>,
    #[serde(default)]
    tools: Option<NonBlank>,
    #[serde(default)]
    description: Option<NonBlank>,
    #[serde(default)]
    kinds: Option<Kinds>,
    #[serde(default)]
    tier: Option<Tier>,
}

// A session's keys, whichever role runs it
struct SessionKeys<'a> {
    harness: Harness,
    model: &'a NonBlank,
    effort: Effort,
    usage: Option<UsageReader>,
    host: Option<ModelHost>,
    context: Option<ContextSize>,
    tools: Option<&'a NonBlank>,
}

// The role alone, and whether the file names a harness
#[derive(Debug, Deserialize)]
struct Binding {
    role: Role,
    #[serde(default)]
    harness: Option<IgnoredAny>,
}

/// The role `text` names, if its frontmatter reads that far
pub(super) fn role(text: &str) -> Option<Role> {
    let (front, _) = split(text)?;
    serde_saphyr::from_str::<Tag>(&format!("\n{front}"))
        .ok()
        .map(|tag| tag.role)
}

/// The role of a file whose frontmatter names a `role` and no `harness`: it
/// binds nothing, so it is unbound and not malformed
pub(super) fn unbound_role(text: &str) -> Option<Role> {
    let (front, _) = split(text)?;
    let keys = serde_saphyr::from_str::<Binding>(&format!("\n{front}")).ok()?;
    keys.harness.is_none().then_some(keys.role)
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
    let tag = serde_saphyr::from_str::<Tag>(&yaml)
        .map_err(|e| gone_at_role(&yaml).unwrap_or_else(|| refused(e)))?;
    let front = match tag.role {
        Role::Implementer => Front::Implementer(serde_saphyr::from_str(&yaml).map_err(refused)?),
        Role::Reviewer => Front::Reviewer(serde_saphyr::from_str(&yaml).map_err(refused)?),
        Role::Pm => Front::Pm(serde_saphyr::from_str(&yaml).map_err(refused)?),
        Role::Representative => {
            Front::Representative(serde_saphyr::from_str(&yaml).map_err(refused)?)
        }
        Role::Triager => Front::Triager(serde_saphyr::from_str(&yaml).map_err(refused)?),
    };
    let leased = match &front {
        Front::Implementer(keys) => keys.lease.is_some(),
        Front::Reviewer(keys) => keys.lease.is_some(),
        Front::Representative(keys) | Front::Triager(keys) => keys.lease.is_some(),
        Front::Pm(_) => false,
    };
    if leased {
        return Err(at_key(&yaml, "lease", LEASE_GONE));
    }
    let body = body.trim();
    let prompt = (!body.is_empty()).then(|| body.to_owned());
    let agent = match front {
        Front::Implementer(keys) => {
            check_needs(
                Role::Implementer,
                keys.harness,
                keys.needs.as_deref(),
                &yaml,
            )?;
            Ok(Agent {
                role: Role::Implementer,
                runs: session(&SessionKeys {
                    harness: keys.harness,
                    model: &keys.model,
                    effort: keys.effort,
                    usage: keys.usage,
                    host: host(keys.url.as_ref(), keys.gateway.as_ref())?,
                    context: keys.context,
                    tools: keys.tools.as_ref(),
                })?,
                prompt,
                paths: Vec::new(),
                second_look: false,
                fit: Fit {
                    description: keys.description,
                    kinds: keys.kinds,
                    tier: keys.tier,
                },
            })
        }
        Front::Reviewer(keys) => {
            let runs = keys.runs(prompt.as_deref(), &yaml)?;
            Ok(Agent {
                role: Role::Reviewer,
                runs,
                prompt,
                paths: keys.paths,
                second_look: keys.second_look,
                fit: Fit {
                    description: keys.description,
                    kinds: keys.kinds,
                    tier: keys.tier,
                },
            })
        }
        Front::Pm(keys) => keys.agent(Role::Pm, prompt, &yaml, "the project manager"),
        Front::Representative(keys) => {
            keys.agent(Role::Representative, prompt, &yaml, "the representative")
        }
        Front::Triager(keys) => keys.agent(Role::Triager, prompt, &yaml, "the triager"),
    }?;
    Ok(agent)
}

/// The refusal of a file whose role is `role`, if that is one of [`GONE`]
pub(super) fn gone(role: &str) -> Option<String> {
    went(role).map(|went| format!("the `{role}` role {went}, so delete the file"))
}

/// What the role `role` went with, if it is one of [`GONE`]
pub(super) fn went(role: &str) -> Option<&'static str> {
    GONE.iter()
        .find(|(gone, _)| *gone == role)
        .map(|(_, went)| *went)
}

/// The refusal of the agent file `text`, if its own `role` is one of [`GONE`]
pub(super) fn gone_role_of(text: &str) -> Option<String> {
    let (front, _) = split(text)?;
    gone(&serde_saphyr::from_str::<Written>(front).ok()?.role)
}

// The refusal at the `role` key, if `yaml` names one of `GONE`.
fn gone_at_role(yaml: &str) -> Option<String> {
    let role = serde_saphyr::from_str::<Written>(yaml).ok()?.role;
    gone(&role).map(|why| at_key(yaml, "role", &why))
}

impl Expert {
    /// The agent of `role` these keys and `prompt` make, or why not: `who`
    /// names it
    fn agent(
        &self,
        role: Role,
        prompt: Option<String>,
        yaml: &str,
        who: &str,
    ) -> Result<Agent, String> {
        check_needs(role, self.harness, self.needs.as_deref(), yaml)?;
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
                usage: self.usage,
                host: host(self.url.as_ref(), self.gateway.as_ref())?,
                context: self.context,
                tools: None,
            })?,
            prompt,
            paths: Vec::new(),
            second_look: false,
            fit: Fit::default(),
        })
    }
}

// Session capabilities are independent of a role's model and prompt keys.
fn check_needs(
    role: Role,
    harness: Harness,
    explicit: Option<&[Need]>,
    yaml: &str,
) -> Result<(), String> {
    let guard = (role == Role::Pm).then_some(&Need::RoleGuard);
    let needs = explicit.unwrap_or_else(|| role.needs()).iter().chain(guard);
    // parity: harness.pm
    let Some(need) = needs.clone().find(|need| !need.met_by(harness)) else {
        return Ok(());
    };
    let name = harness.as_str();
    let message = match (role, need) {
        (Role::Pm, Need::RoleGuard) => format!(
            "the project manager runs on claude-code, not {name}: \
             a Claude Code hook is what holds its writes to its notes"
        ),
        _ => {
            let serving = Harness::ALL
                .into_iter()
                .filter(|harness| needs.clone().all(|need| need.met_by(*harness)))
                .map(Harness::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "`{}` needs {}, which {name} lacks: harnesses that can serve it: {serving}",
                role.as_str(),
                need.description()
            )
        }
    };
    Err(at_key(yaml, "harness", &message))
}

impl OnClaude {
    /// The agent of `role` these keys and `prompt` make, or why not: `who`
    /// names it and why it runs on Claude Code alone
    fn agent(
        &self,
        role: Role,
        prompt: Option<String>,
        yaml: &str,
        who: &str,
    ) -> Result<Agent, String> {
        check_needs(role, self.harness, self.needs.as_deref(), yaml)?;
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
                host: None,
                context: None,
                tools: None,
            })?,
            prompt,
            paths: Vec::new(),
            second_look: false,
            fit: Fit::default(),
        })
    }
}

impl Reviewer {
    /// What runs it, or why its keys cannot: `prompt` is the file's body, if
    /// it has one
    fn runs(&self, prompt: Option<&str>, yaml: &str) -> Result<Runs, String> {
        let harness = match self.harness {
            ReviewerHarness::ClaudeCode => Harness::ClaudeCode,
            ReviewerHarness::Pi => Harness::Pi,
            ReviewerHarness::Codex => Harness::Codex,
            ReviewerHarness::Goose => Harness::Goose,
            #[cfg(any(test, feature = "test-support"))]
            ReviewerHarness::StandIn => Harness::StandIn,
            ReviewerHarness::Command => {
                self.no_bot_keys("command", yaml)?;
                return self.command(prompt.is_some()).map(Runs::Command);
            }
            ReviewerHarness::Endpoint => {
                self.no_bot_keys("endpoint", yaml)?;
                return self.endpoint(prompt).map(Runs::Endpoint);
            }
            ReviewerHarness::Bot => {
                let key = if self.bot.is_some() { "bot" } else { "harness" };
                return self
                    .bot(prompt.is_some())
                    .map(Runs::Bot)
                    .map_err(|m| at_key(yaml, key, &m));
            }
        };
        check_needs(Role::Reviewer, harness, self.needs.as_deref(), yaml)?;
        let name = harness.as_str();
        self.no_bot_keys(name, yaml)?;
        if self.command.is_some() {
            return Err(format!(
                "runs a session on {name}, which takes no `command`"
            ));
        }
        let (Some(model), Some(effort)) = (&self.model, self.effort) else {
            return Err(format!(
                "runs a session on {name}, which needs `model` and `effort`"
            ));
        };
        if prompt.is_none() {
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
            host: host(self.url.as_ref(), self.gateway.as_ref())?,
            context: self.context,
            tools: self.tools.as_ref(),
        })
    }

    // A command keeps the README's contract, which gives it no model, prompt
    // or findings to look again at.
    fn command(&self, prompted: bool) -> Result<LocalCommand, String> {
        self.on_its_own("command", prompted)?;
        let session_keys = self.model.is_some()
            || self.effort.is_some()
            || self.url.is_some()
            || self.gateway.is_some()
            || self.context.is_some()
            || self.tools.is_some();
        if session_keys {
            return Err(
                "runs a command, which takes no `model`, `effort`, `url`, `gateway`, \
                        `context` or `tools`"
                    .into(),
            );
        }
        let Some(command) = &self.command else {
            return Err("runs a command, which needs its path as `command`".into());
        };
        if !(command.is_absolute() || command.starts_with("~")) {
            return Err("`command` must start with `/` or `~/`".into());
        }
        Ok(LocalCommand {
            command: command.clone(),
        })
    }

    // An endpoint takes a body for what to report. Kelpie sends the diff in
    // chunks, so the body cannot place the whole diff or its base.
    fn endpoint(&self, prompt: Option<&str>) -> Result<Endpoint, String> {
        self.on_its_own("endpoint", false)?;
        for placeholder in ["{{BASE}}", "{{DIFF}}"] {
            if prompt.is_some_and(|body| body.contains(placeholder)) {
                return Err(format!(
                    "runs on endpoint, which sends the diff in chunks of its own, so its \
                     body cannot name {placeholder}: leave it out"
                ));
            }
        }
        let other = self.effort.is_some() || self.command.is_some() || self.tools.is_some();
        if other {
            return Err(
                "runs on an endpoint, which takes no `effort`, `command` or `tools`".into(),
            );
        }
        let host = host(self.url.as_ref(), self.gateway.as_ref())?;
        let (Some(host), Some(model), Some(context)) = (host, &self.model, self.context) else {
            return Err("runs on an endpoint, which needs the server as `url` or a \
                        `gateway`, its `model` and the model's context size as `context`"
                .into());
        };
        Ok(Endpoint {
            host,
            model: model.clone(),
            context,
            prompt: prompt.map(str::to_owned),
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

// A server's URL or the gateway in front of it, whichever the keys name.
fn host(
    url: Option<&EndpointUrl>,
    gateway: Option<&GatewayName>,
) -> Result<Option<ModelHost>, String> {
    match (url, gateway) {
        (Some(_), Some(_)) => Err("names both `url` and `gateway`: keep the one its \
                                   requests go to"
            .into()),
        (Some(url), None) => Ok(Some(ModelHost::Url(url.clone()))),
        (None, Some(gateway)) => Ok(Some(ModelHost::Gateway(gateway.clone()))),
        (None, None) => Ok(None),
    }
}

/// Why a `lease` is refused: kelpie holds no lock of its own on a model
const LEASE_GONE: &str = "kelpie holds no lock of its own on a GPU now, so `lease` is gone: \
     name a `gateway` such as paddock in place of `url`, which queues the model's calls, \
     and leave `lease` out";

fn role_model(keys: &SessionKeys<'_>) -> Result<RoleModel, String> {
    let facts = keys.harness.facts();
    // parity: harness.tool-allow-list
    if keys.tools.is_some() && !facts.takes_tools {
        return Err(format!(
            "runs on {}, which takes no `tools`: only claude-code has an allow-list to name",
            facts.name
        ));
    }
    let server = match (facts.needs_model_server, &keys.host, keys.context) {
        (true, Some(host), Some(context)) => Some(model_server(facts.name, host, context)?),
        (true, ..) => {
            return Err(format!(
                "runs on {}, which needs the model's server as `url` or a \
                 `gateway`, and its context size as `context`",
                facts.name
            ));
        }
        (false, None, None) => None,
        (false, ..) => {
            return Err(format!(
                "runs on {}, which takes no `url`, `gateway` or `context`",
                facts.name
            ));
        }
    };
    let harness = AgentHarness::on(keys.harness, server)
        .ok_or_else(|| format!("runs on {}, which cannot take that server", facts.name))?;
    Ok(RoleModel {
        model: keys.model.clone(),
        effort: keys.effort,
        harness,
        tools: keys.tools.cloned(),
    })
}

// The server a pi or goose agent named `harness` runs its model on
fn model_server(
    harness: &str,
    host: &ModelHost,
    context: ContextSize,
) -> Result<ModelServer, String> {
    if let ModelHost::Url(url) = host
        && let Err(e) = Upstream::new(url)
    {
        return Err(format!(
            "runs on {harness}, with a `url` kelpie cannot forward to: {e}"
        ));
    }
    Ok(ModelServer {
        host: host.clone(),
        context,
    })
}

/// What holds a session's calls back, or why its keys do not say
///
/// The reader must be the harness's own: an agent on Claude Code spends
/// the Claude account whatever its `usage` says.
fn limit(keys: &SessionKeys<'_>) -> Result<Limit, String> {
    let own = (keys.harness.facts().usage)
        .or(keys.usage)
        .unwrap_or(UsageReader::Claude);
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
    let gateway = matches!(keys.host, Some(ModelHost::Gateway(_)));
    match usage {
        UsageReader::None if gateway => Ok(Limit::Gateway),
        UsageReader::None => Ok(Limit::Unqueued),
        UsageReader::Claude => Ok(Limit::Account(Account::Claude)),
        UsageReader::Codex => Ok(Limit::Account(Account::Codex)),
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
// unknown key is already named, and placed at the start of a line. An item of
// a block list is led by the key of its list.
fn yaml_error(yaml: &str, error: &serde_saphyr::Error) -> String {
    let message = error.without_snippet().to_string();
    let key = error
        .location()
        .filter(|at| at.column() > 1)
        .and_then(|at| {
            let lines: Vec<&str> = yaml.lines().collect();
            let at = usize::try_from(at.line()).ok()?.checked_sub(1)?;
            let indent = |line: &str| line.len() - line.trim_start().len();
            let line = lines.get(at)?;
            let line = if line.trim_start().starts_with("- ") {
                let own = indent(line);
                let key = |l: &&&str| {
                    let t = l.trim_start();
                    !t.is_empty() && !t.starts_with(['#', '-']) && indent(l) <= own
                };
                lines[..at].iter().rev().find(key)?
            } else {
                line
            };
            let (key, _) = line.split_once(':')?;
            Some(key.trim()).filter(|k| !k.is_empty() && !k.starts_with('#'))
        });
    match key {
        Some(key) => format!("`{key}`: {message}"),
        None => message,
    }
}

// The frontmatter between the opening and closing `---` lines, and the body after.
pub(super) fn split(text: &str) -> Option<(&str, &str)> {
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
