//! The project manager's retro over a batch of finished work items
//!
//! After every [`EVERY`] work items finish, and when a drain starts with any
//! finished since the last retro, the project manager's agent is called once
//! to look back on them. It is a one-off call, not a turn of the standing
//! session, so its report does not grow that session's context. Kelpie builds
//! what it reads in code (see [`digest`]), and its tools change nothing. The
//! reply is the report, saved under `retro-reports/unverified`. Kelpie never
//! acts on a report. A retro that fails, times out or comes back empty is
//! logged and skipped, never retried, and holds no slot.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use super::{Runner, StepReport, ruling};
use crate::home::Build;
use crate::ports::{AgentCall, AgentError, AgentReply, Session, Timestamp};
use crate::skills::Step;
use crate::state::Finished;
use crate::ticket::TicketId;
use crate::usage;
use crate::work_item::new_session_id;

mod digest;
pub(super) mod flakes;
mod gaps;
pub(super) mod journal;

/// How long a retro gets, in seconds
pub(super) const CEILING: u64 = 15 * 60;

/// How many finished work items start a retro
pub(super) const EVERY: usize = 10;

/// What the runner keeps of the retros, in memory only
#[derive(Debug, Default)]
pub(super) struct Book {
    // The last retro's start and the work items it covered, read from the
    // journal the first time they are asked for
    last: Option<Option<(u64, Vec<TicketId>)>>,
    /// Whether a drain started since the retro last looked
    pub(super) at_drain: bool,
}

// The part of the prompt that asks. It stands alone, for a step set to run
// kelpie's own prompt, and follows the skill's command otherwise.
fn prompt(
    project: &str,
    issues: &[TicketId],
    build: &Build,
    gaps: &gaps::Open,
    digest: &str,
) -> String {
    let named: Vec<String> = issues.iter().map(TicketId::shown).collect();
    let known = match gaps.listed.is_empty() {
        true => "No earlier finding of yours is still open.".to_owned(),
        false => {
            let told = match gaps.left_out {
                0 => "the known gaps".to_owned(),
                n => format!(
                    "the known gaps, the newest {} listed and {n} older ones left out",
                    gaps.listed.len()
                ),
            };
            format!(
                "These findings of earlier retros are still open ({told}):\n{}\n\
                 Report none of them again. Where the batch hit one, count it in the report's \
                 `Known gaps seen again` section as `- <theme>: <number of work items>`, with no \
                 description. The digest's across-the-batch section shows which work items name \
                 each.",
                gaps.listed
                    .iter()
                    .map(|g| format!("- {g}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        }
    };
    format!(
        "{} work items of {project} finished since the last retro: {}. Below is a digest \
         kelpie built from its records: one section for each, then the batch's totals. \
         Kelpie's build for this batch: {} (made with shep {}).\n\n\
         {known}\n\n\
         Run a retrospective on how the batch went, reporting new items only. Your reply is \
         the report, in markdown, in this order:\n\
         1. Patterns across the batch: project manager wakes that repeat for one cause, \
         rulings that waited, review findings that were stale or wrong, steps that cost more \
         than the work they did, the files the work items conflicted on most, and review \
         findings first found in the third round read or beyond. For a late finding, say its \
         cause where you can tell (stale base, wording, lifecycle, security).\n\
         2. Problems in a single work item.\n\
         3. If nothing is worth the maintainer's time, say so in a sentence and write nothing else.\n\
         4. A `## {}` section, last, with one bullet for each new problem, \
         `- <theme>: <one line>`, where the theme is a few words that name the gap. A \
         later retro is told these themes as the known gaps until the maintainer clears \
         the report.\n\n\
         For each problem, suggest the change that would remove it (a setting, a prompt, a \
         check, a steering file). Where a digest line is not enough, you may read the code \
         and the records in your folders. Edit no file and run no command; nothing in the \
         report is applied, and the maintainer reads it later.\n\n---\n\n{digest}",
        issues.len(),
        named.join(", "),
        build.kelpie,
        build.shep,
        gaps::HEADING,
    )
}

impl Runner {
    /// Adds `entry` to the project's journal, made now
    ///
    /// A journal that cannot be written is told and let go.
    pub(super) fn journal(&mut self, entry: journal::Entry) {
        let at = self.ports.clock.now();
        self.journal_at(at, entry);
    }

    /// Adds `entry` to the project's journal, made at `at`
    pub(super) fn journal_at(&mut self, at: Timestamp, entry: journal::Entry) {
        if let Err(why) = journal::append(&self.paths.journal, at, entry) {
            self.notes.push(why);
        }
    }

    /// Adds what `report` says to the journal, if it says anything the retro reads
    pub(super) fn journal_report(&mut self, report: &StepReport) {
        if let Some(entry) = journal::Entry::of(report) {
            self.journal(entry);
        }
    }

    // The last retro's start and the work items it covered, or none yet
    fn last_retro(&mut self) -> (u64, Vec<TicketId>) {
        let path = &self.paths.journal;
        let last = self.retro_book.last.get_or_insert_with(|| {
            let entries = journal::read(path);
            entries.into_iter().rev().find_map(|s| match s.entry {
                journal::Entry::Retro { issues } => Some((s.at.0, issues)),
                _ => None,
            })
        });
        last.clone().unwrap_or_default()
    }

    // The work items finished since the last retro, oldest first: the latest
    // `EVERY` of them, so a history that built up with the retro off, or a
    // retro that waited long, is not read all at once.
    fn finished_since_retro(&mut self) -> Vec<Finished> {
        let (since, covered) = self.last_retro();
        let mut batch: Vec<Finished> = (self.state.history.iter())
            .filter(|f| f.at.0 > since || (f.at.0 == since && !covered.contains(&f.issue)))
            .cloned()
            .collect();
        batch.drain(..batch.len().saturating_sub(EVERY));
        batch
    }

    /// Starts the batch retro if one is due and the project manager can run
    /// it, and says whether it did
    ///
    /// Due is [`EVERY`] work items finished since the last retro, or any when
    /// a drain has just started. A draining runner starts no other. The retro
    /// is off unless the skills table names it. A batch that cannot be asked
    /// for now is asked for on a later pass, except at a drain, which asks once.
    pub(super) fn retro_launched(&mut self) -> bool {
        let Some(agent) = self.agents.pm.clone() else {
            self.retro_book.at_drain = false;
            return false;
        };
        if self.skills.off(Step::Retro) {
            self.retro_book.at_drain = false;
            return false;
        }
        // A drain's request waits for the retro in flight to end.
        if self.flights.retro_running() {
            return false;
        }
        let draining_now = std::mem::take(&mut self.retro_book.at_drain);
        let batch = self.finished_since_retro();
        let due = batch.len() >= EVERY || (draining_now && !batch.is_empty());
        if !due || (self.draining() && !draining_now) {
            return false;
        }
        // A wake in flight has the project manager's account; the retro follows.
        if self.pm.waking() {
            self.retro_book.at_drain |= draining_now;
            return false;
        }
        if !self.pm_available() {
            if draining_now {
                self.notes
                    .push("no retro at the drain, since the project manager is held back".into());
            }
            return false;
        }
        let issues: Vec<TicketId> = batch.iter().map(|f| f.issue).collect();
        let built = self.retro_call(&agent, &batch);
        // Marked before the call starts: a stop or a crash in the middle of
        // it does not ask again, and a batch that cannot start is let go.
        let now = self.ports.clock.now();
        let mark = journal::Entry::Retro {
            issues: issues.clone(),
        };
        let marked = journal::append(&self.paths.journal, now, mark);
        self.retro_book.last = Some(Some((now.0, issues.clone())));
        if let Err(why) = marked {
            // Unmarked, a restart would pay for the same batch twice.
            self.notes.push(format!(
                "no retro of {}, since {why}",
                issues
                    .iter()
                    .map(TicketId::shown)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            return false;
        }
        match built {
            Ok(call) => {
                self.launch_retro(call, CEILING, agent.name.as_str());
                // The retro's call is in flight from here.
                self.post();
                true
            }
            Err(why) => {
                self.notes.push(format!(
                    "no retro of {}, since {why}",
                    issues
                        .iter()
                        .map(TicketId::shown)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                false
            }
        }
    }

    // The project manager's agent called fresh with the digest, in its own
    // folder, with tools that change nothing.
    fn retro_call(
        &mut self,
        agent: &crate::settings::PmAgent,
        batch: &[Finished],
    ) -> Result<AgentCall, String> {
        let (since, _) = self.last_retro();
        let journal = journal::read(&self.paths.journal);
        let rulings = ruling::records(&self.paths.rulings);
        let ledger = usage::read(self.ledger.path()).unwrap_or_default();
        let gaps = gaps::open(&self.paths.retro_reports, self.project.as_str());
        let digest = digest::build(&digest::Records {
            project: self.project.as_str(),
            batch,
            journal: &journal,
            rulings: &rulings,
            ledger: &ledger,
            since,
            gaps: &gaps,
        });
        let issues: Vec<TicketId> = batch.iter().map(|f| f.issue).collect();
        let build = Build::this();
        let ask = prompt(self.project.as_str(), &issues, &build, &gaps, &digest);
        let ask = self.skills.invoke(Step::Retro, &ask);
        let id = new_session_id().map_err(|e| format!("cannot draw a session id: {e}"))?;
        self.pm_retro_call(agent, Session::New(id), ask)
    }

    /// Records the end of the retro in flight: its reply is saved as a
    /// report, and anything else is logged and let go
    pub(super) fn retro_ended(&mut self, result: Result<AgentReply, AgentError>) {
        let skipped =
            |why: String| format!("the batch retro is skipped and not asked again, since {why}");
        let note = match result {
            Ok(reply) if reply.text.trim().is_empty() => skipped("it came back empty".into()),
            Ok(reply) => match self.save_retro(&reply) {
                Ok(file) => format!("the batch retro is saved unverified in {}", file.display()),
                Err(why) => skipped(format!("it cannot be saved: {why}")),
            },
            Err(AgentError::Stopped) => skipped("the runner stopped during it".into()),
            Err(e) => skipped(format!("it failed: {e}")),
        };
        self.notes.push(note);
    }

    // Writes the report for the batch the last retro covered, under the
    // project's name and the time, so no two collide.
    fn save_retro(&mut self, reply: &AgentReply) -> Result<PathBuf, String> {
        let (_, issues) = self.last_retro();
        let folder = &self.paths.retro_reports;
        fs::create_dir_all(folder).map_err(|e| format!("{}: {}", folder.display(), e.kind()))?;
        let now = self.ports.clock.now();
        let at = i64::try_from(now.0)
            .ok()
            .and_then(|s| jiff::Timestamp::from_second(s).ok())
            .map_or_else(
                || now.0.to_string(),
                |t| t.strftime("%Y%m%dT%H%M%SZ").to_string(),
            );
        let project = self.project.as_str();
        let file = folder.join(format!("{project}-batch-{at}.md"));
        let named: Vec<String> = issues.iter().map(TicketId::shown).collect();
        let named = named.join(", ");
        let agent = self.agents.pm.as_ref().map_or("none", |a| a.name.as_str());
        let text = format!(
            "# Batch retro: {project} {named}\n\n\
             - Project: {project}\n\
             - Work items: {named}\n\
             - Agent: {agent}\n\
             - Session: {}\n\n\
             ---\n\n\
             {}\n",
            reply.session_id.0,
            reply.text.trim()
        );
        let written = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&file)
            .and_then(|mut f| f.write_all(text.as_bytes()));
        written.map_err(|e| format!("{}: {}", file.display(), e.kind()))?;
        Ok(file)
    }
}

#[cfg(test)]
mod tests;
