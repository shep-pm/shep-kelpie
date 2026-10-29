//! What a runner checks of the reviewers its project named in `agents.allow`,
//! and of every bot among its candidates, when it opens and when it rereads

use crate::ports::{Ports, Visibility};
use crate::review_bot::Bot;
use crate::runner::OpenError;
use crate::settings::{ForgeSlug, ListedReviewer, SettingsError};

// Each local reviewer's command is there, or its endpoint answers.
pub(super) fn check_local(lineup: &[ListedReviewer], ports: &Ports) -> Result<(), SettingsError> {
    for reviewer in lineup {
        let Some(local) = reviewer.runs.local() else {
            continue;
        };
        ports
            .reviewer
            .check(&local)
            .map_err(|reason| SettingsError::Invalid {
                setting: "agents.allow",
                reason: format!("{}: {reason}", reviewer.name),
            })?;
    }
    Ok(())
}

// Every review bot among the candidates needs a profile, which is kelpie's to give.
pub(super) fn check_bots(lineup: &[ListedReviewer], ports: &Ports) -> Result<(), SettingsError> {
    for reviewer in lineup {
        let Some(bot) = reviewer.bot() else {
            continue;
        };
        if !ports.review_bots.iter().any(|p| p.bot() == bot.bot) {
            return Err(SettingsError::Invalid {
                setting: "agents",
                reason: format!("{}: kelpie has no profile for {}", reviewer.name, bot.bot),
            });
        }
    }
    Ok(())
}

// CodeRabbit's free plan reviews public repos only, so a project that names it
// for a repo the forge reports otherwise has it left out of the agent board's
// ask, as an implementer no turn can run on is. The log line says so.
pub(super) fn coderabbit_left_out(
    remote: &ForgeSlug,
    lineup: &[ListedReviewer],
    ports: &Ports,
) -> Result<Option<String>, OpenError> {
    let listed = lineup
        .iter()
        // parity: review-bot.private-repos
        .find(|r| r.bot().is_some_and(|b| b.bot == Bot::Coderabbit));
    let Some(listed) = listed else {
        return Ok(None);
    };
    let visibility = match ports.forge.visibility(remote)? {
        Visibility::Public => return Ok(None),
        Visibility::Private => "private",
        Visibility::Internal => "internal",
    };
    Ok(Some(format!(
        "reviewer {} is left out of the agent board's ask: {} is {visibility}, and {}'s free \
         plan reviews public repos only",
        listed.name,
        remote.as_str(),
        Bot::Coderabbit.name()
    )))
}
