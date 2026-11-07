//! A case's last parts, which say what already happened on its item: the
//! rulings answered on it, what it has spent, all its worker said, and the
//! runner's log lines that name it
//!
//! The worker's words and a log line can hold what a harness or a stranger
//! wrote, so they go in whole through `shown` and `quoted`. The steps file and
//! each transcript are read in the logs job, off the lock, and inline where
//! no job ran.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use super::Parked;
use crate::board::briefing::date_time;
use crate::home::STEPS_FILE;
use crate::ports::SessionId;
use crate::runner::Runner;
use crate::runner::ruling::{Answerer, records};
use crate::settings::RulingName;
use crate::ticket::TicketId;
use crate::watch::{self, Format};

/// What one item's case reads of the runner's log and its worker's transcript
#[derive(Debug, Clone)]
pub(in crate::runner::pm) struct Told {
    // The steps naming the item, or why the steps file could not be read
    steps: Result<Vec<String>, String>,
    // Everything its worker said, or why that is not known
    said: Result<Vec<String>, String>,
}

/// The steps file read once, and where each wanted session's transcript is
pub(in crate::runner::pm) struct Files {
    steps: Result<String, String>,
    found: BTreeMap<String, (Format, PathBuf)>,
}

impl Files {
    /// Reads the steps file `steps`, and finds the transcript of each of `sessions`
    pub(in crate::runner::pm) fn read(steps: &Path, sessions: &BTreeSet<String>) -> Self {
        // A runner that has logged no step has no file, which is no lines
        let steps = match std::fs::read(steps) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            read => read
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .map_err(|e| format!("kelpie could not read {STEPS_FILE} ({})", e.kind())),
        };
        let found = match &steps {
            Ok(text) => watch::transcripts_of(text, sessions),
            Err(_) => BTreeMap::new(),
        };
        Self { steps, found }
    }

    /// What the case of `ticket`, whose worker's session is `session`, reads of them
    pub(in crate::runner::pm) fn told(&self, ticket: TicketId, session: &SessionId) -> Told {
        let said = match (&self.steps, self.found.get(&session.0)) {
            (_, Some((format, file))) => Ok(watch::said(*format, file)),
            (Ok(_), None) => Err(
                "kelpie found no transcript of its session, so what it said is \
                                  not here (a session whose harness keeps none, or one that has \
                                  written none yet)"
                    .to_owned(),
            ),
            (Err(_), None) => Err(format!(
                "its transcript is looked for in {STEPS_FILE}, which could not be read"
            )),
        };
        let steps = match &self.steps {
            Ok(text) => Ok(watch::steps_of(text, ticket)),
            Err(why) => Err(why.clone()),
        };
        Told { steps, said }
    }
}

impl Runner {
    /// The steps file of this project
    pub(in crate::runner::pm) fn steps_file(&self) -> PathBuf {
        self.paths.folder.join(STEPS_FILE)
    }

    // What the wake's job read for `parked`, or a read now
    pub(super) fn case_told(&self, parked: &Parked) -> Told {
        let kept = self.pm.logs.as_ref().and_then(|l| l.told(parked.ticket));
        kept.cloned().unwrap_or_else(|| {
            let sessions = BTreeSet::from([parked.session.0.clone()]);
            Files::read(&self.steps_file(), &sessions).told(parked.ticket, &parked.session)
        })
    }

    // The parts after the pull request's: past rulings, spend, what the worker
    // said and the log lines. Says which could not be read, with why.
    pub(super) fn history_parts(
        &self,
        out: &mut String,
        parked: &Parked,
        told: &Told,
    ) -> Vec<(&'static str, String)> {
        self.past_rulings_part(out, parked);
        self.spend_part(out, parked.ticket);
        let mut gaps = Vec::new();
        out.push_str("## What the worker said\n\n");
        match &told.said {
            Ok(said) if said.is_empty() => out.push_str("It has said nothing.\n\n"),
            Ok(said) => {
                for (n, text) in said.iter().enumerate() {
                    let _ = write!(
                        out,
                        "Said {} of {}:\n\n{}\n\n",
                        n + 1,
                        said.len(),
                        self.data(text)
                    );
                }
            }
            Err(why) => {
                let _ = write!(out, "({why})\n\n");
            }
        }
        out.push_str("## Log lines\n\n");
        match &told.steps {
            Ok(steps) if steps.is_empty() => {
                out.push_str("No line of the runner's log names it.\n")
            }
            Ok(steps) => {
                let _ = writeln!(out, "{}", self.data(&steps.join("\n")));
            }
            Err(why) => {
                let _ = writeln!(out, "({why})");
                gaps.push(("the log lines", why.clone()));
            }
        }
        gaps
    }

    // The rulings answered on the item, newest first, headed by how many of
    // this ruling's kind it has had
    fn past_rulings_part(&self, out: &mut String, parked: &Parked) {
        let mut past = records(&self.paths.rulings);
        past.retain(|r| r.issue == Some(parked.ticket));
        past.reverse();
        out.push_str("## Past rulings\n\n");
        if let Some(ruling) = &parked.ruling {
            let kind = RulingName::of(&ruling.kind);
            let before = past.iter().filter(|r| r.kind == Some(kind)).count();
            let _ = write!(
                out,
                "Kelpie's own count: this is the {} {} on this item.\n\n",
                ordinal(before + 1),
                kind.as_str()
            );
        }
        if past.is_empty() {
            out.push_str("No ruling on this item was answered before.\n\n");
        }
        for record in past {
            let kind = record.kind.map_or("unlabelled", RulingName::as_str);
            let by = match record.by() {
                Answerer::Pm => "the project manager",
                Answerer::Maintainer => "the maintainer",
            };
            let _ = write!(
                out,
                "### {} UTC, {kind}, answered by {by}\n\nAsked:\n\n{}\n\nAnswer:\n\n{}\n\n",
                date_time(record.at),
                self.data(&record.asked),
                self.data(&record.answer)
            );
            if let Some(stem) = &record.stem {
                let _ = write!(out, "Stem:\n\n{}\n\n", self.data(stem));
            }
        }
    }

    // What the item's priced calls cost against its cap, as the board shows it
    fn spend_part(&self, out: &mut String, ticket: TicketId) {
        out.push_str("## Spend\n\n");
        let Some(item) = self.state.item(ticket) else {
            out.push_str("(the work item is gone)\n\n");
            return;
        };
        let cap = item.budget_cap(self.settings.budget.per_item_usd);
        let _ = write!(
            out,
            "${:.2} of ${:.2}\n\n",
            item.priced().total().usd(),
            cap.usd()
        );
    }
}

// 1 as `1st`, 12 as `12th`, 23 as `23rd`
fn ordinal(n: usize) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}
