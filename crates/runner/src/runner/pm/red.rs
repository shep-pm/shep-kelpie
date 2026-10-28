//! The project manager's call on a workflow still red on `main` after kelpie
//! reran its failed jobs: a fix work item at the front of the board, or a
//! question to the maintainer that carries the log lines it read
//!
//! Kelpie reruns each new red head itself (`runner/main_ci/`), so only a head
//! that came back red, or whose rerun the forge refused, is the project
//! manager's to settle, once.

use super::Wake;
use super::act::{WORDS, plain};
use super::answer::{RedCall, RedHow};
use crate::board::READY;
use crate::board::briefing::ticket_ref;
use crate::local_paths::Surface;
use crate::runner::Runner;
use crate::state::{Handled, StateError};
use crate::ticket::TicketId;

/// The most of an issue's title kept, in characters
const TITLE: usize = 120;

/// What a call about a red workflow did
pub(super) enum Settled {
    /// It opened a fix work item, now first on the board
    Fixed(TicketId),
    /// It asked the maintainer, in this message
    Asked(String),
}

impl Runner {
    /// Acts on `call`, and returns what it did in words, or why it was dropped
    ///
    /// # Errors
    ///
    /// [`StateError`] when what it did cannot be saved.
    pub(super) fn settle_red(
        &mut self,
        call: &RedCall,
    ) -> Result<Result<(String, Settled), String>, StateError> {
        let named = &call.workflow;
        let Some(red) = self.state.red.iter().find(|r| &r.workflow == named) else {
            return Ok(Err(format!("red {named}: not red on main")));
        };
        if !red.again {
            return Ok(Err(format!(
                "red {named}: kelpie's rerun of {} has not come back red yet, so there is \
                 nothing to fix or ask about",
                short(&red.head)
            )));
        }
        if red.handled.is_some() {
            return Ok(Err(format!(
                "red {named}: {} already has the call {}",
                short(&red.head),
                red.doing()
            )));
        }
        let head = red.head.clone();
        let url = format!(
            "https://github.com/{}/actions/runs/{}",
            self.remote.as_str(),
            red.run
        );
        let why = plain(&self.shown(call.why.clone()), WORDS);
        let (handled, settled, said) = match &call.how {
            RedHow::Fix { title, body } => {
                if let Some(leak) = self.names_machine([title, body]) {
                    return Ok(Err(format!("red {named}: the issue names {leak}")));
                }
                let title = plain(title, TITLE);
                let body = format!(
                    "{body}\n\n`{named}` is red on `main` at `{head}` after a rerun of its \
                     failed jobs: {url}\n"
                );
                let ticket = match self.file_ticket(&title, &body, &[READY]) {
                    Ok(ticket) => ticket,
                    Err(e) => return Ok(Err(format!("red {named}: cannot open the fix: {e}"))),
                };
                let said = format!(
                    "opened fix work item {} at the front of the board for {named}",
                    ticket_ref(ticket)
                );
                (Handled::Fix(ticket), Settled::Fixed(ticket), said)
            }
            RedHow::Ask { log } => {
                if let Some(leak) = self.names_machine([log]) {
                    return Ok(Err(format!("red {named}: the log lines name {leak}")));
                }
                let message = format!(
                    "{named} is red on main at {} after a rerun, and I cannot tell a flake \
                     from a break. {url}\nThe log lines I read:\n{log}\nWhy: {why}",
                    short(&head)
                );
                let said = format!("asked the maintainer about {named} red on main");
                (Handled::Asked, Settled::Asked(message), said)
            }
        };
        let mut next = self.state.clone();
        if let Settled::Fixed(ticket) = settled {
            next.order.retain(|&t| t != ticket);
            next.order.insert(0, ticket);
        }
        if let Some(red) = next.red.iter_mut().find(|r| &r.workflow == named) {
            red.handled = Some(handled);
        }
        self.save(next)?;
        self.board_changed();
        Ok(Ok((said, settled)))
    }

    // The wake for each head that stayed red after its rerun and has no call yet
    pub(super) fn unsettled_reds(&self) -> Vec<Wake> {
        (self.state.red.iter())
            .filter(|r| r.again && r.handled.is_none())
            .map(|r| Wake::Red {
                workflow: r.workflow.clone(),
                head: r.head.clone(),
            })
            .collect()
    }

    /// Wakes the project manager once for each head that stayed red after its
    /// rerun, since a pick wake comes only where two issues could start
    pub(super) fn pm_reds(&mut self) {
        if self.agents.pm.is_none() {
            return;
        }
        for wake in self.unsettled_reds() {
            if let Wake::Red { workflow, head } = &wake
                && self.pm.red_woken.insert((workflow.clone(), head.clone()))
            {
                self.pm_due(wake);
            }
        }
    }

    // What on this machine `texts` name, which nothing kelpie posts may.
    fn names_machine<'a>(&self, texts: impl IntoIterator<Item = &'a String>) -> Option<String> {
        texts
            .into_iter()
            .find_map(|t| self.local.find(t, Surface::Prose))
            .map(|leak| leak.to_string())
    }
}

pub(super) fn short(head: &str) -> &str {
    head.get(..7).unwrap_or(head)
}
