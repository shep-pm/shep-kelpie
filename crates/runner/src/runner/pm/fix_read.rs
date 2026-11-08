//! The project manager's read of a fix it drafted: a second call, which saw
//! none of the text the draft was written from, checking it against the code
//!
//! The project manager reads worker and bot text, and an issue it files reaches
//! a worker with no human read. This call reads the draft alone, in an empty
//! folder of its own, with the checkout as the only code, nothing to write and
//! no host, and answers `{"verdict": "pass"}` or a refusal with its why.
//! Kelpie files the draft only on a pass, at the front of the board. A failed
//! call, a timeout or a reply with no verdict is a refusal too, and the item's
//! ruling then carries the why and the draft, so the maintainer hears.

use std::io;

use serde_json::Value;

use super::answer::last_named;
use super::fix::{self, FixDraft};
use super::{CEILING, Flying, Kind};
use crate::board::briefing::ticket_ref;
use crate::ports::{AgentCall, AgentError, AgentReply, Role, Session, Tools};
use crate::runner::Runner;
use crate::runner::report::StepReport;
use crate::runner::untrusted::fenced;
use crate::settings::PmAgent;
use crate::state::StateError;
use crate::ticket::TicketId;
use crate::usage::CallKind;
use crate::work_item::new_session_id;

/// The folder a drafted fix is read in, under the worker folder and outside the
/// project manager's own: its `board.md`, its notes and each `cases/` file stay
/// out of reach, as do the worktrees, which a worker writes
const READ_FOLDER: &str = "pm-fix-read";

/// What the read of a draft said
enum Read {
    /// The draft holds up against the code
    Pass,
    /// It does not, in the read's words, or why it could not say
    Refused(String),
}

impl Runner {
    /// Starts the read of `draft`, and says whether its call is in flight
    ///
    /// A read kelpie cannot start at all puts the draft to the maintainer, as a
    /// refusal does, and says no.
    pub(super) fn begin_fix_read(&mut self, agent: &PmAgent, draft: FixDraft) -> bool {
        let made = match new_session_id() {
            Ok(id) => self
                .pm_fix_read_call(agent, Session::New(id.clone()), &draft)
                .map(|call| (call, id)),
            Err(e) => Err(format!("cannot draw a session id: {e}")),
        };
        let (call, session) = match made {
            Ok(made) => made,
            Err(why) => return self.gave_up_read(&draft, &why),
        };
        self.pm.flying = Some(Flying {
            kind: Kind::FixRead,
            wakes: Vec::new(),
            read_to: self.state.last_event,
            told: Vec::new(),
            lost: Vec::new(),
            session,
            fresh: false,
            draft: Some(draft),
        });
        self.launch_pm(call, CEILING, agent.name.as_str(), CallKind::FixRead);
        true
    }

    /// Acts on the end of a drafted fix's read: a pass files the draft and parks
    /// the item on it, and anything else puts the draft to the maintainer
    ///
    /// # Errors
    ///
    /// [`StateError`] when what it did to the board or the ruling cannot be saved.
    pub(super) fn fix_read_ended(
        &mut self,
        flying: Flying,
        result: Result<AgentReply, AgentError>,
    ) -> Result<Option<StepReport>, StateError> {
        let Some(draft) = flying.draft else {
            return Ok(None);
        };
        let verdict = match result {
            Ok(reply) => read_of(&reply.text),
            // A call that failed, was ended at its ceiling or never reached its
            // model is a refusal: nothing is filed on what kelpie cannot read.
            Err(e) => Read::Refused(format!("the read call failed: {e}")),
        };
        let (fix, verdict, dropped) = match verdict {
            Read::Pass => {
                let (fix, dropped) = self.filed_fix(&draft)?;
                (fix, "pass".to_owned(), dropped)
            }
            Read::Refused(why) => {
                let dropped = self.asked_fix(&draft, &why)?;
                (None, why, dropped)
            }
        };
        Ok(Some(StepReport::PmFixRead {
            item: draft.item,
            fix,
            verdict,
            dropped,
        }))
    }

    // The draft put to the maintainer with `why`, and whether a call is in
    // flight, which giving up the read never starts
    fn gave_up_read(&mut self, draft: &FixDraft, why: &str) -> bool {
        let verdict = format!("kelpie could not read your draft: {why}");
        let dropped = match self.ask_fix(draft.item, &verdict, &draft.draft) {
            Ok(Ok(_)) => Vec::new(),
            Ok(Err(why)) => vec![why],
            Err(e) => vec![format!("cannot ask the maintainer: {e}")],
        };
        self.started.push(StepReport::PmFixRead {
            item: draft.item,
            fix: None,
            verdict,
            dropped,
        });
        false
    }

    // The draft filed as the read passed it, first on the board, and the item
    // parked on it: the fix's ticket, and what kelpie could not do
    fn filed_fix(
        &mut self,
        draft: &FixDraft,
    ) -> Result<(Option<TicketId>, Vec<String>), StateError> {
        let ticket = match self.file_ticket(&draft.draft.title, &draft.draft.body, &fix::LABELS) {
            Ok(ticket) => ticket,
            Err(e) => {
                let why = format!("the read passed your draft, but kelpie could not file it: {e}");
                return Ok((None, self.asked_fix(draft, &why)?));
            }
        };
        // Top priority is first in the board's order, as a red fix's is.
        let mut next = self.state.clone();
        next.order.retain(|&t| t != ticket);
        next.order.insert(0, ticket);
        self.save(next)?;
        self.board_changed();
        let dropped = match self.settle_fix(draft.item, ticket, &draft.why)? {
            Ok(done) => {
                self.notes.push(done);
                Vec::new()
            }
            Err(why) => vec![why],
        };
        Ok((Some(ticket), dropped))
    }

    // The draft put to the maintainer on its item's ruling, with `why` and the
    // draft, and what kelpie could not do
    fn asked_fix(&mut self, draft: &FixDraft, why: &str) -> Result<Vec<String>, StateError> {
        match self.ask_fix(draft.item, why, &draft.draft)? {
            Ok(done) => {
                self.notes.push(done);
                Ok(Vec::new())
            }
            Err(why) => Ok(vec![why]),
        }
    }

    // The read's call: an empty folder cleared first, the checkout to read,
    // nothing to write, no host, no plugin and no standing instructions, on the
    // project manager's own model, so it runs on whatever harness that model is on.
    fn pm_fix_read_call(
        &self,
        agent: &PmAgent,
        session: Session,
        draft: &FixDraft,
    ) -> Result<AgentCall, String> {
        let folder = self.paths.worker.join(READ_FOLDER);
        match std::fs::remove_dir_all(&folder) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot clear {}: {e}", folder.display())),
            _ => {}
        }
        std::fs::create_dir_all(&folder)
            .map_err(|e| format!("cannot make {}: {e}", folder.display()))?;
        let code = [self.settings.git.checkout.clone()];
        let reach = self.fenced_reach(&folder, &code, Vec::new(), Vec::new());
        let mut call = AgentCall::new(
            Role::Pm,
            Tools::Review,
            agent.model.harness.clone(),
            agent.model.model.as_str().to_owned(),
            agent.model.effort,
            session,
            folder,
            read_prompt(self.project.as_str(), draft),
        );
        call.settings = self.paths.worker.join("pm-fix-read-settings.json");
        call.reach = reach;
        // Not `prepared`, which gives the repo's plugins and their servers: this
        // call is given no skill, and reaches no host but the model's.
        self.ports
            .agents
            .prepare(&call)
            .map_err(|e| e.to_string())?;
        Ok(call)
    }

    // Notes each draft the queue held when the runner stopped starting calls:
    // the queue is in memory, so a restart loses them.
    pub(super) fn pm_reads_given_up(&mut self) {
        for draft in self.pm.fix_reads.drain(..) {
            self.notes.push(format!(
                "the fix drafted for {} was never read, so kelpie filed nothing for it",
                ticket_ref(draft.item)
            ));
        }
    }
}

// The read's prompt: the draft quoted as data, and what to hold it against
fn read_prompt(project: &str, draft: &FixDraft) -> String {
    format!(
        "You are the project manager for {project}, reading one fix issue you drafted for {}.\n\
         \n\
         You saw none of the text it was written from, and kelpie files it only if you pass it. \
         Read the code in the checkout, which is all you may read, and hold the draft against it:\n\
         \n\
         - does each file and each name it gives exist, where it says it is?\n\
         - does each claim it makes hold in this code?\n\
         - does it ask for work a worker could take, with every part an issue must give one?\n\
         \n\
         {}\n\n\
         {}\n\n\
         {}\n\n\
         Answer with one JSON object and nothing else, either\n\
         \n\
         {{\"verdict\": \"pass\"}}\n\
         \n\
         or\n\
         \n\
         {{\"verdict\": \"refuse\", \"why\": \"<one sentence, at most 300 characters>\"}}",
        ticket_ref(draft.item),
        fenced("Its title", &draft.draft.title),
        fenced("Why it is being filed", &draft.why),
        fenced("The fix issue you drafted", &draft.draft.body),
    )
}

// The verdict, from the last object in the reply that has one
fn read_of(text: &str) -> Read {
    let words = |key: &str| {
        (last_named(text, &["verdict"])?)
            .get(key)
            .and_then(Value::as_str)
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    };
    let Some(verdict) = words("verdict") else {
        return Read::Refused("the read gave no verdict kelpie reads".to_owned());
    };
    match verdict.as_str() {
        "pass" => Read::Pass,
        "refuse" => Read::Refused(
            words("why")
                .unwrap_or_else(|| "the read refused the draft, and gave no reason".to_owned()),
        ),
        other => Read::Refused(format!(
            "the read's verdict is {other}, which kelpie does not take"
        )),
    }
}
