//! The actions a ruling's options name, and kelpie carrying them out
//!
//! The project manager is read-only: it runs no git, `gh` or command. An
//! option that needs one names an action instead, in the words of its table
//! row, so the project manager and the maintainer choose the action. Kelpie
//! carries it out in code when the option is chosen, and the item's events
//! say which option asked for it.

use std::path::Path;

use super::{Action, Answer, RuleError};
use crate::fence;
use crate::ports::Rerun;
use crate::runner::Runner;
use crate::runner::gate::short;
use crate::runner::ruling::Answerer;
use crate::runner::voice;
use crate::state::{Ruling, RulingKind, Stuck};
use crate::ticket::TicketId;
use crate::worktree;

impl Action {
    /// The word that chooses it in an answer: `<id> rerun`
    pub fn word(self) -> &'static str {
        match self {
            Self::Rerun => "rerun",
            Self::Restore => "restore",
        }
    }

    pub(super) fn verb(self) -> &'static str {
        match self {
            Self::Rerun => "rerun the failed jobs",
            Self::Restore => "restore an agent file",
        }
    }
}

/// An action as the ruling that offers it fixes it
pub(super) enum Act {
    Rerun { head: String },
    Restore { files: Vec<String> },
    RerunAll { run: u64 },
}

/// The action `answer` names for a ruling of `kind`, if it names one
///
/// # Errors
///
/// [`RuleError::NotOffered`] when the ruling lists no option for it.
pub(super) fn plan(
    id: TicketId,
    answer: &Answer,
    kind: &RulingKind,
) -> Result<Option<Act>, RuleError> {
    // A check no workflow posted has no run to rerun, so a yes only waits.
    if let (Answer::Yes, RulingKind::Stuck(Stuck::CheckQueued { run: Some(run), .. })) =
        (answer, kind)
    {
        return Ok(Some(Act::RerunAll { run: *run }));
    }
    let Answer::Do(action) = answer else {
        return Ok(None);
    };
    match (action, kind) {
        (
            Action::Rerun,
            RulingKind::Stuck(Stuck::StillRed {
                head, rerun: true, ..
            }),
        ) => Ok(Some(Act::Rerun { head: head.clone() })),
        (Action::Restore, RulingKind::Stuck(Stuck::TurnFailed { restore, .. }))
            if !restore.is_empty() =>
        {
            let files = restore.clone();
            Ok(Some(Act::Restore { files }))
        }
        _ => Err(RuleError::NotOffered(id, *action)),
    }
}

/// Who chose which option, as the item's events open a line about its action
pub(super) fn asked(ruling: &Ruling, answer: &Answer, by: Answerer) -> String {
    let kind = &ruling.kind;
    let option = voice::chosen(kind, answer)
        .and_then(|key| Some((key, voice::named(kind, key)?)))
        .map_or_else(String::new, |(key, name)| format!(" option {key} ({name})"));
    let who = match by {
        Answerer::Maintainer => "the maintainer",
        Answerer::Pm => "the project manager",
    };
    let on = ruling
        .issue
        .map_or(String::new(), |ticket| format!("{}: ", ticket.shown()));
    format!("{on}ruling {}{option} chosen by {who}", ruling.number())
}

impl Runner {
    // Carries out `act`, and says what it did. `worktree` is the one of the
    // work item the ruling parks.
    pub(super) fn carry_out(
        &self,
        id: TicketId,
        act: &Act,
        worktree: Option<&Path>,
    ) -> Result<String, RuleError> {
        match act {
            Act::Rerun { head } => {
                let rerun = self.ports.forge.rerun_failed(&self.remote, head);
                rerun.map_err(|e| RuleError::Act(id, format!("cannot rerun the jobs: {e}")))?;
                Ok(format!("reran the failed jobs of {}", short(head)))
            }
            Act::RerunAll { run } => {
                let rerun = self.ports.forge.rerun_run(&self.remote, *run, Rerun::All);
                rerun.map_err(|e| RuleError::Act(id, format!("cannot rerun the run: {e}")))?;
                Ok(format!("reran run {run}"))
            }
            Act::Restore { files } => {
                let worktree = worktree.ok_or_else(|| {
                    RuleError::Act(id, "no worktree is held for this ruling".to_owned())
                })?;
                restore(&self.settings.git.checkout, worktree, files)
                    .map_err(|e| RuleError::Act(id, format!("cannot restore the files: {e}")))?;
                Ok(format!(
                    "restored {} to the head's version",
                    files.join(", ")
                ))
            }
        }
    }

    /// The agent files `issue`'s worktree holds uncommitted changes to, which a
    /// failed turn's ruling offers to restore
    ///
    /// A file the worktree's git ignores is still one the fence refuses, so the
    /// files that differ from `main` and the head does not hold count too.
    pub(in crate::runner) fn restorable(&self, issue: TicketId) -> Vec<String> {
        let Some(item) = self.state.item(issue) else {
            return Vec::new();
        };
        let repo = &self.settings.git.checkout;
        let held = worktree::uncommitted(repo, &item.worktree).unwrap_or_default();
        let mut files: Vec<String> = held.into_iter().filter(|f| agent_file(f)).collect();
        let accepted = item.claude_files_accepted.as_deref();
        let base = self.item_base(item);
        let differing = fence::differ(repo, &item.worktree, accepted, base).unwrap_or_default();
        if let Ok(git) = worktree::trusted(repo, &item.worktree) {
            files.extend(differing.into_iter().filter(|file| !in_head(&git, file)));
        }
        files.sort();
        files.dedup();
        files
    }
}

// Whether the last commit holds `file`
fn in_head(git: &impl Fn(&[&str]) -> Result<String, worktree::WorktreeError>, file: &str) -> bool {
    git(&["cat-file", "-e", &format!("HEAD:{file}")]).is_ok()
}

// Whether `path`, relative to a worktree, is a file that decides what an
// agent does there: a harness's own, or the instructions it reads
fn agent_file(path: &str) -> bool {
    let path = Path::new(path);
    let name = path.file_name().and_then(|name| name.to_str());
    fence::fenced(path) || matches!(name, Some("CLAUDE.md" | "AGENTS.md"))
}

// Puts `files` back as the last commit has them: a changed or staged file to
// its committed text, and a new file unstaged and then removed, since `clean`
// leaves a staged one alone.
fn restore(repo: &Path, worktree: &Path, files: &[String]) -> Result<(), worktree::WorktreeError> {
    let git = worktree::trusted(repo, worktree)?;
    let (kept, new): (Vec<&str>, Vec<&str>) =
        (files.iter().map(String::as_str)).partition(|file| in_head(&git, file));
    let checkout: &[&str] = &["checkout", "HEAD"];
    let unstage: &[&str] = &["reset", "--quiet"];
    let clean: &[&str] = &["clean", "--force", "-x"];
    for (command, paths) in [(checkout, kept), (unstage, new.clone()), (clean, new)] {
        if paths.is_empty() {
            continue;
        }
        // Each name is a path, whatever characters it holds.
        let mut args = vec!["--literal-pathspecs"];
        args.extend(command.iter().copied());
        args.push("--");
        args.extend(paths);
        git(&args)?;
    }
    Ok(())
}
