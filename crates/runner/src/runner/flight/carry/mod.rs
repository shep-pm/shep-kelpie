//! Worker turns in flight across a handover
//!
//! A runner that hands over to a new build keeps its pid and its children,
//! so a turn's `claude` goes on running. The outgoing image carries what the
//! incoming one needs to finish the turn as it would have: the call's record
//! on disk, its line in the usage ledger as it started, and when it started.
//! Anything else in flight is not carried, and the handover is refused while
//! it runs. A call whose record is gone has just ended unheard, so the
//! handover waits [`CARRY_WAIT`] for the pass that hears it.

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::super::Runner;
use super::super::attach::live;
use super::super::briefing::Watched;
use super::super::in_flight::{Key, Open};
use super::super::transcript_count::Mark;
use super::{End, Flight, News, Tell, unstarted};
use crate::lease::wire::AskerCarry;
use crate::ports::{Adopted, Ending, Flying, KeptServer, NotAdopted, Timestamp};
use crate::settings::{GatewayName, Harness};
use crate::ticket::TicketId;
use crate::work_item::WorkItem;

/// How long a handover waits for the end of a call whose record is gone to be
/// heard, before it is refused
pub const CARRY_WAIT: Duration = Duration::from_secs(10);

/// Why the turns in flight cannot be taken now
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotCarried {
    /// A call just ended and its end is not yet heard, so a pass later may
    /// carry what this one cannot
    Waiting(String),
    /// Nothing a pass does makes the turns carryable
    Refused(String),
}

impl fmt::Display for NotCarried {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (Self::Waiting(why) | Self::Refused(why)) = self;
        f.write_str(why)
    }
}

impl From<String> for NotCarried {
    fn from(why: String) -> Self {
        Self::Refused(why)
    }
}

/// A worker's turn as it crosses a handover
// wire format: changing this is a change to the handover's state format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CarriedTurn {
    // A build that carried issue numbers names this field `issue`: a handover
    // from one still loads, each number read as the ticket id of that number.
    #[serde(alias = "issue")]
    ticket: TicketId,
    adopted: Adopted,
    flying: Flying,
    started: Timestamp,
    open: Open,
    // Where the turn's transcript stood as it began, so the next image counts its requests
    #[serde(default, skip_serializing_if = "Option::is_none")]
    requests: Option<Mark>,
}

impl CarriedTurn {
    /// The ticket of the work item the turn is for
    pub fn ticket(&self) -> TicketId {
        self.ticket
    }

    /// The process the turn's call runs as
    pub fn pid(&self) -> u32 {
        self.adopted.pid
    }
}

impl Runner {
    /// The turns in flight, as the next image takes them, or why they
    /// cannot be taken
    ///
    /// # Errors
    ///
    /// [`NotCarried::Refused`] for a call in flight that is not a worker's
    /// turn, one that holds a browser, one a handover already carried, or one
    /// whose record is there and cannot be read. [`NotCarried::Waiting`] for
    /// a turn with no record, which has just ended or has not yet written
    /// one, until [`CARRY_WAIT`] has passed since it was first seen so.
    pub fn carry_calls(&mut self) -> Result<Vec<CarriedTurn>, NotCarried> {
        let now = self.ports.clock.now();
        let flights = &self.flights;
        let others = [
            (flights.pm.is_some(), "the project manager's call"),
            (flights.retro.is_some(), "the batch retro"),
        ];
        if let Some((_, what)) = others.iter().find(|(flying, _)| *flying) {
            return Err(format!("{what} is in flight, and only a worker's turn is carried").into());
        }
        for harness in Harness::ALL {
            let Some(why) = harness.facts().holds_handover_at_attach else {
                continue;
            };
            if let Some(issue) = self.sessions_attached_on(harness).first() {
                let name = harness.as_str();
                return Err(format!(
                    "#{issue} is attached, and the {name} session in the maintainer's terminal \
                     {why}"
                )
                .into());
            }
        }
        let flights = &mut self.flights;
        let mut turns = Vec::new();
        let mut waiting = None;
        for (&ticket, flight) in &mut flights.flying {
            if flight.browser.is_some() {
                return Err(format!(
                    "#{ticket}'s turn drives a browser, whose socket this image holds, \
                     which the next image would not"
                )
                .into());
            }
            let Some(call) = &flight.watched.call else {
                return Err(format!(
                    "the call of #{ticket} is a review or one the last handover carried, \
                     and only a worker's turn from this image is carried"
                )
                .into());
            };
            let adopted = match self.ports.agents.adoption(call) {
                Ok(adopted) => {
                    flight.record_gone = None;
                    adopted
                }
                Err(NotAdopted::RecordGone(why)) => {
                    let since = *flight.record_gone.get_or_insert(now);
                    let why = format!("the turn of #{ticket} cannot be carried: {why}");
                    if now.0.saturating_sub(since.0) >= CARRY_WAIT.as_secs() {
                        return Err(NotCarried::Refused(why));
                    }
                    waiting.get_or_insert(why);
                    continue;
                }
                Err(NotAdopted::Refused(why)) => {
                    return Err(format!("the turn of #{ticket} cannot be carried: {why}").into());
                }
            };
            let flying = adopted.flying.ok_or_else(|| {
                format!("the record of #{ticket}'s turn notes no ceiling, so it cannot be timed")
            })?;
            let open = (flights.ledger.peek(Key::Item(ticket)))
                .ok_or_else(|| format!("the turn of #{ticket} has no line in the usage ledger"))?;
            turns.push(CarriedTurn {
                ticket,
                adopted,
                flying,
                started: flight.watched.started,
                open,
                requests: flight.requests.clone(),
            });
        }
        match waiting {
            Some(why) => Err(NotCarried::Waiting(why)),
            None => Ok(turns),
        }
    }

    /// The lease totals as the next image takes them up, if this runner's
    /// leases keep any
    pub fn carry_leases(&self) -> Option<AskerCarry> {
        self.ports.leases.keep()
    }

    // The work items whose hold stands on a worker of `harness`, whose session in the
    // maintainer's terminal dials a forwarder only this image holds.
    pub(in crate::runner) fn sessions_attached_on(&self, harness: Harness) -> Vec<TicketId> {
        let holds = |item: &&WorkItem| item.attached.as_ref().is_some_and(live);
        let on_it = |item: &&WorkItem| {
            // parity: harness.attach-ends
            (self.worker_agent(item)).is_ok_and(|agent| agent.harness.harness() == harness)
        };
        let items = self.state.work_items.iter();
        items
            .filter(|item| holds(item) && on_it(item))
            .map(|item| item.issue)
            .collect()
    }

    /// Takes on `turns` a previous image carried, each finished on its own
    /// thread as the image that started it would have
    pub fn adopt_calls(&mut self, turns: Vec<CarriedTurn>) {
        for turn in turns {
            self.adopt_turn(turn);
        }
    }

    fn adopt_turn(&mut self, turn: CarriedTurn) {
        let CarriedTurn {
            ticket,
            adopted,
            flying,
            started,
            open,
            requests,
        } = turn;
        let harness = adopted.harness;
        let lease = lease_kept(&adopted);
        let ending = Ending::default();
        ending.carry(flying);
        let tell = Tell {
            ticket,
            news: self.flights.send.clone(),
            wake: self.flights.wake.clone(),
        };
        self.flights.ledger.restore(Key::Item(ticket), open);
        let (held, ledger) = (ending.clone(), self.flights.ledger.clone());
        let agents = Arc::clone(&self.ports.agents);
        let metrics = Arc::clone(&self.metrics);
        let notes = self.notes.clone();
        let run = move || {
            let ran = catch_unwind(AssertUnwindSafe(|| {
                End::Turn(agents.adopt(&adopted, &held))
            }));
            if let Some(mark) = &requests {
                mark.count(adopted.harness, ticket, &metrics, &notes);
            }
            let end = ran.unwrap_or_else(End::Panicked);
            if end.reached_no_model() {
                ledger.no_model(Key::Item(ticket));
            }
            tell.send(News::Ended { ticket, end });
        };
        let began = thread::Builder::new()
            .name(format!("#{ticket} worker"))
            .spawn(run);
        if let Err(e) = began {
            let end = unstarted(Some(harness), &e);
            let _ = self.flights.send.send(News::Ended { ticket, end });
        }
        self.flights.flying.insert(
            ticket,
            Flight {
                deadline: Some(Timestamp(flying.began.0.saturating_add(flying.left))),
                left: flying.left,
                ending,
                again: flying.again,
                watched: Watched {
                    started,
                    call: None,
                    idle: false,
                    told: None,
                    granted: Some(flying.began),
                    woken: false,
                    lease,
                },
                first: flying.first,
                compact: flying.compact,
                browser: None,
                requests: None,
                record_gone: None,
            },
        );
        self.model_leases();
        self.board_changed();
    }
}

// The gateway and model a carried call still holds a lease on, as its flight names them, so
// the new image counts it held. `None` when it holds none, or the image that carried it did
// not say which model.
fn lease_kept(adopted: &Adopted) -> Option<(GatewayName, String)> {
    let kept = adopted.kept.as_ref()?;
    kept.lease.as_ref()?;
    let KeptServer::Gateway(name) = &kept.server else {
        return None;
    };
    let gateway = GatewayName::try_from(name.clone()).ok()?;
    Some((gateway, kept.model.clone()?))
}

#[cfg(test)]
mod tests;
