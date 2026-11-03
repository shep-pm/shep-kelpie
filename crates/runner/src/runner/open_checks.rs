//! What a start checks of the settings that act on other people's work: the
//! App must cover the repo, and the agent files they name must be there

use std::path::Path;

use super::OpenError;
use crate::agents::Agents;
use crate::ports::Voice;
use crate::settings::{ForgeSlug, ListedReviewer, Settings, SettingsError};

// The refusal when `setting` picks up other people's work and the App does not
// cover `remote`, which would make `who` post as the maintainer.
fn uncovered(setting: &'static str, who: &str, remote: &ForgeSlug) -> OpenError {
    SettingsError::Invalid {
        setting,
        reason: format!(
            "the GitHub App does not cover {}, so {who} would otherwise post as the \
             maintainer: set up the App for the repo, or set `pick = \"off\"`",
            remote.as_str()
        ),
    }
    .into()
}

/// The reviewers a contribution gets, checked with its agents, or none while
/// `contributions.pick` is `off`
///
/// # Errors
///
/// [`OpenError`] naming `contributions.pick` when the App does not cover the
/// repo, else the setting whose agent file is missing or has the wrong role.
pub(super) fn contributions(
    settings: &Settings,
    voice: &dyn Voice,
    remote: &ForgeSlug,
    book: &Agents,
    home: &Path,
) -> Result<Vec<ListedReviewer>, OpenError> {
    if settings.contributions.is_off() {
        return Ok(Vec::new());
    }
    if !voice.covers(remote) {
        return Err(uncovered(
            "contributions.pick",
            "the representative",
            remote,
        ));
    }
    let lineup = settings.contribution_lineup(book, home)?;
    settings.security_reviewer(book)?;
    settings.representative(book)?;
    Ok(lineup)
}

/// Checks the triage settings against the App and the agent files, while
/// `triage.pick` is not `off`
///
/// # Errors
///
/// [`OpenError`] naming `triage.pick` when the App does not cover the repo,
/// else `triage.agent` or `agents.representative` when its agent file is
/// missing or has the wrong role.
pub(super) fn triage(
    settings: &Settings,
    voice: &dyn Voice,
    remote: &ForgeSlug,
    book: &Agents,
) -> Result<(), OpenError> {
    if settings.triage.is_off() {
        return Ok(());
    }
    if !voice.covers(remote) {
        return Err(uncovered("triage.pick", "the triager", remote));
    }
    settings.triager(book)?;
    settings.representative(book)?;
    Ok(())
}
