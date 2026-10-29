//! The reviewers a review pass may pick from, by name
//!
//! A project names no reviewers. Every agent file whose role is `reviewer` that
//! `agents.allow` holds is a candidate, but the one `contributions.security_reviewer`
//! names. The agent board picks each round's reviewer, so the order does not
//! matter. A project naming no agent is held to none of its candidates, so one
//! `gateways` cannot route is left out; one naming the agents it may use keeps
//! every candidate it named, and a start, a reread and `doctor` refuse one
//! kelpie cannot route. A contribution's pass runs `contributions.reviewers`
//! when it names them, else `iq3-xxs-review` where a gateway is named `paddock`.

use std::path::Path;

use super::agents::{Allow, find};
use super::{AgentName, Gateways, NonBlank, Settings, SettingsError};
use crate::agents::{Agent, Agents, DEFECT_HUNTER_LITE, IQ3_XXS_REVIEW, Role, Runs};
use crate::review_bot::BotReviewer;

/// Where a project lists the reviewers a contribution reads, as a refusal names it
const CONTRIBUTION_SETTING: &str = "contributions.reviewers";

/// The reviewers kelpie gives a contribution with no list, as a refusal names them
const DEFAULT_REVIEWERS: &str = "kelpie's default reviewer";

/// Where a project names the reviewer that reads a contribution for
/// security, as a refusal names it
const SECURITY_SETTING: &str = "contributions.security_reviewer";

pub(super) fn lowercase_name(value: &str) -> bool {
    let allowed = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
    !value.is_empty() && value.chars().all(allowed)
}

/// One reviewer a project lists
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedReviewer {
    /// Its name, as the project lists it
    pub name: AgentName,
    /// What runs its round, a command's path taken from the home folder
    pub runs: Runs,
    /// Its prompt, for a session: its file's body
    pub prompt: Option<String>,
    /// Globs of the files a pull request must change for it to run. Every
    /// pull request when empty.
    pub paths: Vec<NonBlank>,
    /// Whether it reads twice, the second time shown what it found the first
    pub second_look: bool,
}

impl ListedReviewer {
    /// Whether it runs on its own, as a command or an endpoint, not in a session
    pub fn is_local(&self) -> bool {
        self.runs.local().is_some()
    }

    /// The review bot it is, if it is one
    pub fn bot(&self) -> Option<BotReviewer> {
        self.runs.bot()
    }
}

impl Settings {
    /// The reviewers a pass may pick from
    ///
    /// Every agent file whose role is `reviewer` that `agents.allow` holds, but
    /// the agent `contributions.security_reviewer` names. Their order does not
    /// matter. Agents come from `agents`, and `~/` in a command expands against
    /// `home`. A project that names no agent in `agents.allow` takes only the
    /// candidates `gateways` can route, since it asked for none of them and a
    /// round passes over one it cannot run.
    pub fn lineup(&self, agents: &Agents, gateways: &Gateways, home: &Path) -> Vec<ListedReviewer> {
        let named = self.names_reviewers();
        (held(
            self.agents.allow.as_ref(),
            &self.contributions.security_reviewer,
            agents,
        ))
        .filter(|(name, _)| named || gateways.check_listed_without_keys(agents, [*name]).is_ok())
        .map(|(name, agent)| reviewer(name.clone(), agent, home))
        .collect()
    }

    /// Whether the project named the agents it may use: `agents.allow` holds
    /// some. A project that names none lets every reviewer file be a candidate
    /// and is held to none of them.
    pub fn names_reviewers(&self) -> bool {
        self.agents
            .allow
            .as_ref()
            .is_some_and(|allow| !allow.is_empty())
    }

    /// The reviewers a contribution gets, in order, each once
    ///
    /// `contributions.reviewers` when set, else the default reviewers for
    /// `gateways`: a contribution's pass runs them all, so it does not take
    /// every reviewer file.
    ///
    /// # Errors
    ///
    /// [`SettingsError::Invalid`] naming a reviewer `contributions.reviewers`
    /// lists that has no agent file, whose file is an implementer's, that
    /// `agents.allow` leaves out, or a bot that reviews on ready listed after
    /// another bot. A default reviewer `agents.allow` leaves out is refused the
    /// same way, naming it as one of kelpie's own.
    pub fn contribution_lineup(
        &self,
        agents: &Agents,
        gateways: &Gateways,
        home: &Path,
    ) -> Result<Vec<ListedReviewer>, SettingsError> {
        let allow = self.agents.allow.as_ref();
        if let Some(names) = &self.contributions.reviewers {
            return listed(names.clone(), agents, home, CONTRIBUTION_SETTING, allow);
        }
        let names = default_reviewers(gateways);
        let left_out = (names.iter()).find(|n| allow.is_some_and(|allow| !allow.holds(n)));
        if let Some(name) = left_out {
            return Err(SettingsError::Invalid {
                setting: "contributions",
                reason: format!(
                    "kelpie's default reviewer {name} is one `agents.allow` leaves out: add it \
                     to `agents.allow`, or list the reviewers a contribution reads in \
                     `contributions.reviewers`"
                ),
            });
        }
        listed(names, agents, home, DEFAULT_REVIEWERS, allow)
    }

    /// The reviewers the project named: every reviewer file a non-empty
    /// `agents.allow` holds, whether or not a gateway can route them, and none
    /// when the project names no agent at all. A start, a reread and `doctor`
    /// hold only these to what a round cannot work around.
    pub fn named_reviewers(&self, agents: &Agents, home: &Path) -> Vec<ListedReviewer> {
        match self.names_reviewers() {
            true => (held(
                self.agents.allow.as_ref(),
                &self.contributions.security_reviewer,
                agents,
            ))
            .map(|(name, agent)| reviewer(name.clone(), agent, home))
            .collect(),
            false => Vec::new(),
        }
    }

    /// The agent that reads a contribution for security
    ///
    /// # Errors
    ///
    /// [`SettingsError::Invalid`] naming `contributions.security_reviewer`
    /// when it has no agent file, its file is not a reviewer's, or the
    /// reviewer is not one that runs sessions: the diff is read in a session
    /// of its own, which a command, an endpoint and a bot cannot be given.
    pub fn security_reviewer<'a>(&self, agents: &'a Agents) -> Result<&'a Agent, SettingsError> {
        let name = &self.contributions.security_reviewer;
        let agent = find(
            agents,
            name,
            SECURITY_SETTING,
            SECURITY_SETTING,
            Role::Reviewer,
            self.agents.allow.as_ref(),
        )?;
        match agent.runs.session() {
            Some(_) => Ok(agent),
            None => Err(SettingsError::Invalid {
                setting: SECURITY_SETTING,
                reason: format!(
                    "`{SECURITY_SETTING}` names {name}, which does not run sessions: the \
                     diff is read in a session of its own, so name a reviewer that does"
                ),
            }),
        }
    }
}

// Every reviewer file `allow` holds, but the one `security` names. An absent or
// empty `allow` holds every agent with a file.
fn held<'a>(
    allow: Option<&'a Allow>,
    security: &'a AgentName,
    agents: &'a Agents,
) -> impl Iterator<Item = (&'a AgentName, &'a Agent)> + 'a {
    (agents.iter()).filter(move |(name, agent)| {
        agent.role == Role::Reviewer
            && *name != security
            && allow.is_none_or(|allow| allow.holds(name))
    })
}

// The reviewers `names` lists, each once, as `setting` names the list. A
// bot that reviews on ready must come before another bot.
fn listed(
    names: Vec<AgentName>,
    agents: &Agents,
    home: &Path,
    setting: &'static str,
    allow: Option<&Allow>,
) -> Result<Vec<ListedReviewer>, SettingsError> {
    let lineup = each(names, agents, home, setting, allow)?;
    on_ready_first(&lineup, setting)?;
    Ok(lineup)
}

// The reviewers `names` lists, each once, in any order.
fn each(
    names: Vec<AgentName>,
    agents: &Agents,
    home: &Path,
    setting: &'static str,
    allow: Option<&Allow>,
) -> Result<Vec<ListedReviewer>, SettingsError> {
    let mut lineup: Vec<ListedReviewer> = Vec::new();
    for name in names {
        if lineup.iter().any(|r| r.name == name) {
            continue;
        }
        let agent = find(agents, &name, setting, "agents", Role::Reviewer, allow)?;
        lineup.push(reviewer(name, agent, home));
    }
    Ok(lineup)
}

// `name` as a reviewer, with `~/` in a command's path taken from `home`.
fn reviewer(name: AgentName, agent: &Agent, home: &Path) -> ListedReviewer {
    let mut runs = agent.runs.clone();
    if let Runs::Command(command) = &mut runs
        && let Ok(rest) = command.command.strip_prefix("~")
    {
        command.command = home.join(rest);
    }
    ListedReviewer {
        name,
        runs,
        prompt: agent.prompt.clone(),
        paths: agent.paths.clone(),
        second_look: agent.second_look,
    }
}

// A bot listed earlier marks the draft ready for its own summon, which
// draws the review of a bot that reviews on ready outside that bot's lease.
fn on_ready_first(lineup: &[ListedReviewer], setting: &'static str) -> Result<(), SettingsError> {
    let bots = lineup.iter().filter(|r| r.bot().is_some());
    let mut before: Option<&AgentName> = None;
    for reviewer in bots {
        if let (Some(first), Some(bot)) = (before, reviewer.bot())
            && bot.reviews_on_ready
        {
            return Err(SettingsError::Invalid {
                setting,
                reason: format!(
                    "{} reviews a pull request when it leaves draft, so list it before \
                     {first}: a bot listed before it marks the draft ready, which draws \
                     {}'s own review outside its lease",
                    reviewer.name,
                    bot.bot.name()
                ),
            });
        }
        before = before.or(Some(&reviewer.name));
    }
    Ok(())
}

/// `iq3-xxs-review` where `gateways` hold a gateway named `paddock`,
/// then `defect-hunter-lite`
pub fn default_reviewers(gateways: &Gateways) -> Vec<AgentName> {
    let local = gateways.has_paddock().then_some(IQ3_XXS_REVIEW);
    let names = local.into_iter().chain([DEFECT_HUNTER_LITE]);
    names.map(AgentName::kelpies).collect()
}

#[cfg(test)]
mod tests;
