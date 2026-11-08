//! The project manager's fix calls: parking a stuck item on a fix that is
//! already open, and the fix issue it drafts for one that has none
//!
//! A fault in kelpie's settings, its own code or this machine is the
//! maintainer's to fix, and the project manager changes none of them. Where it
//! can name the open fix a stuck item waits on, it parks the item there: the
//! ruling stays, and its card names the fix, so the maintainer has both in one
//! place. Where it drafts the fix instead, kelpie checks the draft here and
//! reads it against the code (`fix_read.rs`) before filing it. A draft that
//! fails either goes to the maintainer, with nothing filed.

use super::StuckState;
use super::act::plain;
use super::answer::{Action, FixCall, FixHow};
use crate::board::READY;
use crate::board::briefing::ticket_ref;
use crate::board::check;
use crate::local_paths::Surface;
use crate::runner::Runner;
use crate::state::StateError;
use crate::ticket::TicketId;

/// The most of an issue's title kept, in characters
pub(super) const TITLE: usize = 120;

/// The labels a drafted fix is filed with, once its read passes it: the board may
/// start it, and it is not an emergency, which is what `priority: P0` is for
pub(super) const LABELS: [&str; 3] = [READY, "priority: P1", "kind:fix"];

/// A fix issue the project manager wrote, as kelpie would file it
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Draft {
    /// Its title, on one line and cut to what a title holds
    pub(super) title: String,
    /// Its body, as the project manager wrote it
    pub(super) body: String,
}

/// What kelpie found when it checked a drafted fix before filing or reading it
pub(super) enum Checked {
    /// It may be filed as it stands
    Clear(Draft),
    /// Its title or body names this machine, which nothing kelpie files may
    Machine(String),
    /// The issue check refuses it, each failure in words
    Refused(Vec<String>),
}

// The labels a fix is checked with besides its text. A triage pass puts the priority and kind on,
// so the check here reads only the title and body, which are the project manager's to write.
fn stood_in() -> [String; 2] {
    ["priority: P0".to_owned(), "kind:fix".to_owned()]
}

impl Runner {
    /// Keeps `item`'s work item parked on the fix it names, and returns what
    /// it did in words, or why the call was dropped
    ///
    /// # Errors
    ///
    /// [`StateError`] when the ruling it rewords cannot be saved.
    pub(super) fn settle_fix(
        &mut self,
        item: TicketId,
        on: TicketId,
        why: &str,
    ) -> Result<Result<String, String>, StateError> {
        let named = ticket_ref(item);
        let id = match self.parked_ruling(item) {
            Ok(id) => id,
            Err(why) => return Ok(Err(why)),
        };
        let fix = ticket_ref(on);
        if on == item {
            return Ok(Err(format!("fix {named}: it cannot be its own fix")));
        }
        let issue = match self.ports.tickets.issue(&self.remote, on) {
            Ok(issue) => issue,
            Err(e) => return Ok(Err(format!("fix {named}: cannot read {fix}: {e}"))),
        };
        if !issue.open {
            return Ok(Err(format!("fix {named}: {fix} is closed")));
        }
        match self.settle(item, id, Action::Ask, why, Some(on), None)? {
            Ok(()) => Ok(Ok(format!("parked {named} on {fix}"))),
            Err(why) => Ok(Err(why)),
        }
    }

    /// Queues `call`'s draft to be read against the code, or puts it to the
    /// maintainer when kelpie's own checks refuse it, and returns what it did
    /// in words, or why the call was dropped
    ///
    /// # Errors
    ///
    /// [`StateError`] when what it did to the ruling cannot be saved.
    pub(super) fn draft_fix(
        &mut self,
        call: &FixCall,
    ) -> Result<Result<String, String>, StateError> {
        let named = ticket_ref(call.item);
        let FixHow::Draft { title, body } = &call.how else {
            return Ok(Err(format!("fix {named}: it names no drafted fix")));
        };
        if let Err(why) = self.parked_ruling(call.item) {
            return Ok(Err(why));
        }
        let (asked, draft) = match self.checked(title, body) {
            Checked::Clear(draft) => {
                self.pm.fix_reads.push_back(FixDraft {
                    item: call.item,
                    draft,
                    why: call.why.clone(),
                });
                return Ok(Ok(format!(
                    "queued the fix it drafted for {named} to be read against the code"
                )));
            }
            Checked::Machine(leak) => (format!("it names {leak}"), draft_of(title, body)),
            Checked::Refused(failures) => (
                format!("it fails the issue check: {}", failures.join("; ")),
                draft_of(title, body),
            ),
        };
        self.ask_fix(call.item, &asked, &draft)
    }

    /// Puts `item`'s drafted fix to the maintainer with `why` and the draft,
    /// files nothing, and returns what it did in words, or why it was dropped
    ///
    /// The draft goes whole under the ruling's words, which is what its card
    /// renders and the webhook posts (`voice::card::drafted`).
    ///
    /// # Errors
    ///
    /// [`StateError`] when the ruling it rewords cannot be saved.
    pub(super) fn ask_fix(
        &mut self,
        item: TicketId,
        why: &str,
        draft: &Draft,
    ) -> Result<Result<String, String>, StateError> {
        let named = ticket_ref(item);
        let id = match self.parked_ruling(item) {
            Ok(id) => id,
            Err(why) => return Ok(Err(why)),
        };
        // Each part on its own, so a draft that names this machine loses its
        // body to the check and still says why kelpie filed nothing.
        let whole = format!(
            "{}\n\n{}",
            self.shown(draft.title.clone()),
            self.shown(draft.body.clone()),
        );
        match self.settle(
            item,
            id,
            Action::Ask,
            &self.shown(why.to_owned()),
            None,
            Some(whole.as_str()),
        )? {
            Ok(()) => Ok(Ok(format!(
                "asked the maintainer about {named}'s fix draft, which kelpie did not file"
            ))),
            Err(why) => Ok(Err(why)),
        }
    }

    /// `title` and `body` as kelpie would file them, or what stops it from
    /// filing them before anything has read them
    pub(super) fn checked(&self, title: &str, body: &str) -> Checked {
        if let Some(leak) = self.names_machine([title, body]) {
            return Checked::Machine(leak);
        }
        let draft = draft_of(title, body);
        let failures = check::check(&draft.title, &draft.body, &stood_in());
        match failures.is_empty() {
            true => Checked::Clear(draft),
            false => Checked::Refused(failures.iter().map(ToString::to_string).collect()),
        }
    }

    // The ruling `item` is parked on, or why it is no stuck work item parked on one
    fn parked_ruling(&self, item: TicketId) -> Result<TicketId, String> {
        match self.stuck(item) {
            Some(StuckState::Ruling { id, .. }) => Ok(id),
            _ => Err(format!(
                "fix {}: it is no stuck work item parked on a ruling",
                ticket_ref(item)
            )),
        }
    }

    // What on this machine `texts` name, which nothing kelpie files may.
    pub(super) fn names_machine<'a>(
        &self,
        texts: impl IntoIterator<Item = &'a str>,
    ) -> Option<String> {
        texts
            .into_iter()
            .find_map(|t| self.local.find(t, Surface::Prose))
            .map(|leak| leak.to_string())
    }
}

// `title` and `body` as an issue's, the title on one line and cut to what a title holds
fn draft_of(title: &str, body: &str) -> Draft {
    Draft {
        title: plain(title, TITLE),
        body: body.to_owned(),
    }
}

/// A fix the project manager drafted, waiting for its read against the code
#[derive(Debug, Clone)]
pub(super) struct FixDraft {
    /// The work item it is for
    pub(super) item: TicketId,
    /// The issue, as kelpie's checks left it
    pub(super) draft: Draft,
    /// Why the project manager files it
    pub(super) why: String,
}
