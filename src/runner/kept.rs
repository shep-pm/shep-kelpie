//! Agent files for the work items a state file opened before agent files
//!
//! Such an item's worker ran a model at an effort, which loads as the agent
//! `<model>-<effort>`. Where kelpie has no agent of that name, the runner's
//! start writes one on Claude Code from what the state held, so the item runs
//! on as it did. A local worker's or another harness's model cannot be
//! rebuilt from the state, so that item's turn fails naming the file to write.

use std::path::Path;

use crate::agents::{Agents, AgentsError, write_kept};
use crate::settings::{AgentName, Effort};
use crate::state::StateStore;

// The model aliases Claude Code takes, beside ids starting `claude-`.
const ALIASES: [&str; 4] = ["opus", "sonnet", "haiku", "fable"];

/// `book` with a file written in `folder` for each work item in `store` that
/// opened on a Claude model before agent files and whose agent kelpie lacks,
/// and a log line for each file written
///
/// # Errors
///
/// [`AgentsError`] naming the file that could not be written or read back.
pub(super) fn keep_old_agents(
    store: &StateStore,
    folder: &Path,
    book: Agents,
) -> Result<(Agents, Vec<String>), AgentsError> {
    let mut notes = Vec::new();
    for old in store.old_workers() {
        let claude = old.model.starts_with("claude-") || ALIASES.contains(&old.model.as_str());
        let (Ok(name), Some(effort)) = (
            AgentName::try_from(old.agent.clone()),
            Effort::parse(&old.effort),
        ) else {
            continue;
        };
        if old.local || !claude || book.get(&name).is_some() {
            continue;
        }
        if write_kept(folder, &name, &old.model, effort)? {
            notes.push(format!(
                "issue #{}'s worker ran {} at {} before agent files, so kelpie wrote \
                 `agents/{name}.md` for it, on Claude Code",
                old.issue, old.model, old.effort
            ));
        }
    }
    match notes.is_empty() {
        true => Ok((book, notes)),
        false => Ok((Agents::load(folder)?, notes)),
    }
}
