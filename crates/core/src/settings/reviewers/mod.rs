//! The review's reviewers, by name
//!
//! `agents.reviewers` lists them, by agent files whose role is `reviewer`, and
//! narrows a review pass's candidates. With none, every reviewer file
//! `agents.allow` holds is a candidate but the one `contributions.security_reviewer`
//! names. The agent board picks each round's reviewer, so order does not matter.
//! A contribution's pass with no `contributions.reviewers` runs `agents.reviewers`,
//! else `iq3-xxs-review` where a gateway is named `paddock`, then
//! `defect-hunter-lite`.

use std::path::Path;

use super::agents::{Allow, find, left_out};
use super::{AgentName, Gateways, NonBlank, Settings, SettingsError};
use crate::agents::{Agent, Agents, DEFECT_HUNTER_LITE, IQ3_XXS_REVIEW, Role, Runs};
use crate::review_bot::BotReviewer;

/// Where a project lists its reviewers, as a refusal names it
const SETTING: &str = "agents.reviewers";

/// Where a project lists its contributions' reviewers, as a refusal names it
const CONTRIBUTION_SETTING: &str = "contributions.reviewers";

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
    /// The names `agents.reviewers` lists when it is set, in order, each once.
    /// Otherwise every agent file whose role is `reviewer` that `agents.allow`
    /// holds and `gateways` can route, but the agent
    /// `contributions.security_reviewer` names. Their order does not matter.
    /// Agents come from `agents`, and `~/` in a command expands against `home`.
    ///
    /// # Errors
    ///
    /// [`SettingsError::Invalid`] naming a reviewer `agents.reviewers` lists that
    /// has no agent file, whose file is an implementer's, that `agents.allow` leaves
    /// out, or a bot that reviews on ready listed after another bot.
    pub fn lineup(
        &self,
        agents: &Agents,
        gateways: &Gateways,
        home: &Path,
    ) -> Result<Vec<ListedReviewer>, SettingsError> {
        let allow = self.agents.allow.as_ref();
        match &self.agents.reviewers {
            Some(names) => listed(names.clone(), agents, home, SETTING, allow),
            None => {
                let security = &self.contributions.security_reviewer;
                let candidates = (agents.iter())
                    .filter(|(name, agent)| {
                        agent.role == Role::Reviewer
                            && *name != security
                            && allow.is_none_or(|allow| allow.holds(name))
                            && gateways.check_listed_without_keys(agents, [*name]).is_ok()
                    })
                    .map(|(name, _)| name.clone());
                each(candidates.collect(), agents, home, SETTING, allow)
            }
        }
    }

    /// The reviewers a contribution gets, in order, each once
    ///
    /// `contributions.reviewers` when set. Otherwise `agents.reviewers` when
    /// set, else the default reviewers for `gateways`: a contribution's pass
    /// runs them all, so it does not take every reviewer file.
    ///
    /// # Errors
    ///
    /// As [`Settings::lineup`], with a refusal of `contributions.reviewers`
    /// naming it.
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
        if let Some(names) = &self.agents.reviewers {
            return listed(names.clone(), agents, home, SETTING, allow);
        }
        let names = default_reviewers(gateways);
        if let Some(refusal) =
            (names.iter()).find_map(|n| left_out(allow, n, CONTRIBUTION_SETTING, "name another"))
        {
            return Err(SettingsError::Invalid {
                setting: "agents",
                reason: refusal,
            });
        }
        listed(names, agents, home, CONTRIBUTION_SETTING, allow)
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
        let mut runs = agent.runs.clone();
        if let Runs::Command(command) = &mut runs
            && let Ok(rest) = command.command.strip_prefix("~")
        {
            command.command = home.join(rest);
        }
        lineup.push(ListedReviewer {
            name,
            runs,
            prompt: agent.prompt.clone(),
            paths: agent.paths.clone(),
            second_look: agent.second_look,
        });
    }
    Ok(lineup)
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
