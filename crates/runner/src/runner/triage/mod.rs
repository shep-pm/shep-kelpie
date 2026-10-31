//! Which issues other people opened are due for triage
//!
//! The runner lists the open issues once a board poll ([`poll_read`]) and checks
//! them ([`checks`]). While `triage.pick` is not `off`, `status` lists as due,
//! oldest first, each the check refused that carries no [`TRIAGE_LABELS`] but
//! `ready-for-agent` or a `needs-triage` above `pruned_through`, opened by a person
//! or the App after `triage.since` (a `ready-for-agent` one at any time), and under
//! `maintainer` by the maintainer or the App. [`Runner::triage_due`] starts one research call at a time ([`flight::triage`]),
//! on the oldest or the issue `shep kelpie triage` queued; [`research`] reads its
//! reply, [`comment`] posts it, and [`ready`] rewrites a maintainer's issue.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;

use super::Runner;
use super::report::Begin;
use super::review::calls::build_call;
use crate::board::{READY, TRIAGE};
use crate::ports::{
    AgentCall, AuthorAssociation, ForgeError, OpenIssue, Role, ThreadComment, Timestamp, Tools,
};
use crate::settings::{ExpertAgent, Settings};
use crate::state::{ProjectState, StateError, TriageState, TriageStep, Triaged};
use crate::ticket::TicketId;
use research::{Listed, ListedPull};

use super::thread_replies::plain_login;
use ask::Purpose;

pub(super) mod addressed;
pub(super) mod ask;
mod checks;
pub(super) mod comment;
mod facts;
pub(super) mod hand;
pub(super) mod heard;
pub(super) mod images;
pub(super) mod limits;
pub(super) mod local;
pub(super) mod marker;
pub(super) mod poll_read;
pub(super) mod ready;
pub(super) mod research;
pub(super) mod rewrite;
mod scrub;
pub(super) mod spam;

use super::contributions::post::forge_is_busy;
use super::flight::triage::{Job, Research};
use images::Pictures;
use spam::POSSIBLE_SPAM;

/// The labels that say an issue has been looked at. All but `needs-triage`, a
/// queue state anyone may put on by hand, make the pick rule leave it alone.
/// `possible-spam` is one of them: an issue its classifier read as spam is left
/// alone until the maintainer says otherwise.
pub(super) const TRIAGE_LABELS: [&str; 6] = [
    "needs-triage",
    "needs-info",
    "ready-for-agent",
    "ready-for-human",
    "wontfix",
    POSSIBLE_SPAM,
];

/// The ticket of an issue triage is working on. Triage reads and writes only
/// GitHub issues, whose numbers are ticket ids, so a number that is not one is
/// a bug in the caller.
pub(super) fn ticket(issue: u64) -> Result<TicketId, ForgeError> {
    TicketId::from_issue(issue)
        .ok_or_else(|| ForgeError::Unreadable(format!("{issue} is not an issue number")))
}

/// What one runner's triage holds for its own run only: the account ages it
/// read from the forge, and what its spam check answered. The next start reads
/// the forge and the gateway again.
#[derive(Debug, Default)]
pub(super) struct Run {
    /// When each triage author's account was created, keyed by the login lowered.
    /// A `None` is an age the forge could not say, kept so a poll does not ask again
    pub(super) accounts: BTreeMap<String, Option<Timestamp>>,
    /// Whether the spam check is off for the rest of the run, told once
    pub(super) spam_off: bool,
    /// Whether an answer kelpie could not read has been told yet, so a broken
    /// route is named once and not on every issue. The check stays on.
    pub(super) spam_unreadable_told: bool,
    /// What the last poll read, which the triager's call is made from. None until a
    /// poll has read the listing, and again after one that could not.
    pub(super) listing: Option<poll_read::Listing>,
    /// How many times the check's comment on each issue failed, short of
    /// `checks::CHECK_TRIES`
    pub(super) check_fails: BTreeMap<u64, u32>,
}

/// An untouched issue, as `status` lists it
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DueStatus {
    /// Its number
    pub issue: u64,
    /// Its title
    pub title: String,
    /// The login of whoever opened it, empty when their account is gone
    pub author: String,
    /// How the forge counts whoever opened it to the repo, which is what the
    /// new-account limit reads, or none when the listing does not say
    pub author_association: Option<AuthorAssociation>,
    /// When it was opened
    pub created: Timestamp,
}

/// What the daily triage cap says, as `status` shows it
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DailyStatus {
    /// The triage calls the records place in the last 24 hours
    pub used: u32,
    /// `triage.per_day`
    pub cap: u32,
    /// When the next place opens, while the day's are all taken
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opens: Option<Timestamp>,
    /// How long from now that is, in seconds, which is the same fact for the
    /// plain block, and has no clock of its own to work it out from
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait: Option<u64>,
}

/// What `status` says of triage
#[derive(Debug, Serialize)]
pub struct TriageStatus<'a> {
    /// The untouched issues, oldest first
    pub due: &'a [DueStatus],
    /// The issue the research call is on, while it runs
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running: Option<u64>,
    /// The issues kelpie has commented on, and how far each has got
    #[serde(skip_serializing_if = "<[Triaged]>::is_empty")]
    pub issues: &'a [Triaged],
    /// The day's cap and how many of its places are taken, while triage is on
    /// and the cap is
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daily: Option<DailyStatus>,
}

/// Sets `state.triage` for a start or a reread under `settings`: stamped `now` when
/// `triage.pick` is not `off` and none is saved, cleared when it is `off`.
/// Returns whether it changed.
pub(super) fn settle_since(state: &mut ProjectState, settings: &Settings, now: Timestamp) -> bool {
    match (settings.triage.is_off(), &state.triage) {
        (true, Some(_)) => state.triage = None,
        (false, None) => {
            state.triage = Some(TriageState {
                since: now,
                issues: Vec::new(),
                unrecorded: Vec::new(),
                pruned_through: 0,
            })
        }
        _ => return false,
    }
    true
}

impl Runner {
    // What the daily cap says, or none while triage is off or the cap is
    fn triage_daily(&self) -> Option<DailyStatus> {
        let cap = self.settings.triage.per_day;
        if self.state.triage.is_none() || cap == 0 {
            return None;
        }
        let now = self.ports.clock.now();
        let (used, opens) = self.triage_day(now);
        let wait = (opens).map(|at| at.0.saturating_sub(now.0));
        Some(DailyStatus {
            used,
            cap,
            opens,
            wait,
        })
    }

    // What `status` says of triage, or none while there is nothing to say
    pub(super) fn triage_status(&self) -> Option<TriageStatus<'_>> {
        let issues = self.state.triage.as_ref().map_or(&[][..], |t| &t.issues);
        let running = self.flights.triaging();
        let daily = self.triage_daily();
        let silent = self.triage_due.is_empty() && issues.is_empty() && running.is_none();
        (!silent).then_some(TriageStatus {
            due: &self.triage_due,
            running,
            issues,
            daily,
        })
    }

    // Finishes the triage of each issue whose record stopped short of `done`,
    // as a restart leaves it
    fn resume_triage(&mut self) -> Result<(), StateError> {
        let held = self
            .state
            .triage
            .as_ref()
            .map_or_else(Vec::new, |t| t.issues.clone());
        for record in held.iter().filter(|t| {
            !matches!(
                t.step,
                TriageStep::Done | TriageStep::Ready | TriageStep::Taken
            )
        }) {
            if self.triage_given_up(record.issue) || self.flights.triaging() == Some(record.issue) {
                continue;
            }
            self.finish_triage(record.issue)?;
        }
        Ok(())
    }

    // The research call to start, if one is due: the verb's issue, then the oldest
    // untouched one, unless a call is in flight or the runner drains. One issue is
    // tried per poll, so a failure holds up no other. With no App on the repo nothing
    // starts, as the comment could not be posted. The caller holds a free slot.
    pub(super) fn triage_due(&mut self) -> Result<Option<Begin>, StateError> {
        let idle = self.state.triage.is_some() && !self.draining() && self.app_covers();
        if !idle || self.flights.triaging().is_some() {
            return Ok(None);
        }
        let queued = self.triage_queue.iter().map(|&issue| (issue, true));
        let untouched = self.triage_due.iter().map(|due| (due.issue, false));
        let candidates: Vec<(u64, bool)> = queued.chain(untouched).collect();
        for (issue, asked) in candidates {
            if self.flights.triage_tried(issue) || self.triage_given_up(issue) {
                continue;
            }
            // A record is the issue's, to finish or to leave, unless the verb asked again.
            if !asked && !self.first_pass_owed(issue) {
                continue;
            }
            self.flights.triage_try(issue);
            if self.triage_settled(issue)? {
                continue;
            }
            // A research that read but could not be posted is posted again, not redone.
            if self
                .triage_tries
                .get(&issue)
                .is_some_and(|t| t.waiting.is_some())
            {
                self.retry_triage(issue)?;
                continue;
            }
            // The limits, read from the records and the listing, before the call
            // is made.
            let waiting = (!asked)
                .then(|| self.triage_due.iter().find(|d| d.issue == issue).cloned())
                .flatten();
            let limited = match &waiting {
                Some(due) => self.triage_limited(&due.author, asked),
                None => false,
            };
            if let Some(due) = &waiting
                && limited
            {
                match self.triage_held(due) {
                    Some(limits::Held::Day) => continue,
                    Some(limits::Held::Final(limit)) => {
                        self.triage_hold(issue, &due.author, limit)?;
                        continue;
                    }
                    None => {}
                }
            }
            if let Some(job) = self.triage_job(issue, Purpose::First, limited) {
                return Ok(Some(Begin::Triage(Box::new(job))));
            }
        }
        // The author's round: the second triager call, once their answers are
        // in, the body their own session rewrote, and a questions comment or a
        // prompt a restart left owed.
        self.triage_round_due()
    }

    // Reads `issue`'s comments, once, for a triage comment of the App's own: one
    // there records the issue as triaged with no call, and closes it again when
    // its marker says it was closing a duplicate. Whether the issue needs no
    // call now: it was recorded, or its comments could not be read.
    fn triage_settled(&mut self, issue: u64) -> Result<bool, StateError> {
        if self.triage_tries.get(&issue).is_some_and(|t| t.read) {
            return Ok(false);
        }
        let before = self.triage_record(issue).map(|t| t.comment);
        match self.marked_by_app(issue, before) {
            Ok(Some(marked)) => {
                self.record_triage(
                    issue,
                    marked.comment,
                    marked.created,
                    marked.duplicate_of,
                    marked.asked,
                )?;
                match marked.duplicate_of {
                    Some(of) => self.close_duplicate(issue, of)?,
                    None => self.finish_triage(issue)?,
                }
                Ok(true)
            }
            Ok(None) => {
                self.triage_tries.entry(issue).or_default().read = true;
                Ok(false)
            }
            Err(e) => {
                let why = format!("cannot read its comments: {e}");
                self.triage_failed(issue, &why, !forge_is_busy(&e));
                Ok(true)
            }
        }
    }

    // What a pass asked again over an issue kelpie already triaged is shown:
    // the App's triage comments on it and their author's replies as one text,
    // oldest first, with the questions those rounds asked and their author
    // answered after them. `None` when the issue holds no triage comment.
    fn earlier_rounds(
        &self,
        issue: u64,
        author: &str,
    ) -> Result<Option<(String, Vec<String>)>, ForgeError> {
        let comments = self
            .ports
            .tickets
            .issue_comments(&self.remote, ticket(issue)?)?;
        let Some(app) = self.app_login().map(|app| plain_login(&app)) else {
            return Ok(None);
        };
        let mine = plain_login(author);
        let mut ours: Vec<&ThreadComment> = (comments.iter())
            .filter(|c| {
                (plain_login(&c.author) == app && c.body.contains(marker::MARKER))
                    || plain_login(&c.author) == mine
            })
            .collect();
        if ours.iter().all(|c| plain_login(&c.author) != app) {
            return Ok(None);
        }
        ours.sort_by_key(|c| (c.created, c.id));
        let text = (ours.iter())
            .map(|c| format!("{} wrote:\n\n{}", c.author, c.body.trim()))
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");
        // What each round's author settled, from their replies to that round
        // alone: a line that is only `ok` settles the round, a `Qn: ...` line
        // settles that one, and a reply in neither form settles none that code can
        // name. Their replies end where the next round went up.
        let mut settled: Vec<String> = Vec::new();
        let rounds: Vec<&ThreadComment> = (ours.iter())
            .filter(|c| plain_login(&c.author) == app && ask::holds_questions(&c.body))
            .copied()
            .collect();
        for (at, asked) in rounds.iter().enumerate() {
            let until = rounds.get(at + 1).map(|c| (c.created, c.id));
            let replies: Vec<&ThreadComment> = (ours.iter())
                .filter(|c| {
                    plain_login(&c.author) == mine
                        && (c.created, c.id) > (asked.created, asked.id)
                        && until.is_none_or(|next| (c.created, c.id) < next)
                })
                .copied()
                .collect();
            for (n, question) in asked_in(&asked.body).iter().enumerate() {
                if (replies.iter()).any(|c| ask::answers_one(&c.body, n + 1)) {
                    settled.push(ask::question_key(question));
                }
            }
        }
        Ok(Some((text, settled)))
    }

    // Everything the research on `issue` needs, or `None` when it cannot be
    // had: the issue is no longer open, or what it needs cannot be read or built.
    fn triage_job(&mut self, issue: u64, purpose: Purpose, limited: bool) -> Option<Job> {
        // The poll's listing, read with the runner's lock let go. An issue asked for
        // before the poll has read one waits for the next, which the verb makes due.
        let poll_read::Listing { open, pulls } = self.triage_run.listing.clone()?;
        let Some(this) = open.iter().find(|i| i.number == issue) else {
            self.triage_queue.retain(|&n| n != issue);
            return None;
        };
        let role = self.author_role(&this.author, this.author_association);
        // The labels the issue carries when its pass begins, kept once so a
        // triage label that comes by later is the maintainer's own.
        let taken = self.taken_seen(issue);
        // A pass asked again over an issue kelpie already triaged reads the
        // rounds it ran: its own triage comments on the issue and their
        // author's replies go into the prompt, and the questions those rounds
        // settled are kept from the new pass.
        let earlier = match (matches!(purpose, Purpose::First)
            && self.triage_record(issue).is_some_and(|t| t.comment != 0))
        {
            true => match self.earlier_rounds(issue, &this.author) {
                Ok(earlier) => earlier,
                Err(e) => {
                    let why = format!("cannot read its earlier triage rounds: {e}");
                    self.triage_failed(issue, &why, !forge_is_busy(&e));
                    return None;
                }
            },
            false => None,
        };
        let tries = self.triage_tries.entry(issue).or_default();
        tries.role = Some(role);
        if tries.seen.is_none() {
            tries.seen = Some(taken.unwrap_or_else(|| this.labels.clone()));
        }
        tries.begun = Some(local::body_hash(&this.body));
        // The author's login comes from the same listing that made the call, so
        // a call that never posted is still counted against their cap.
        if tries.author.is_none() {
            tries.author = Some(this.author.clone());
        }
        tries.settled = earlier
            .as_ref()
            .map(|(_, settled)| settled.clone())
            .unwrap_or_default();
        // The classifier, after the limits and before the call. The maintainer's
        // own issues and one the verb asked for skip it, as they skip the limits.
        if matches!(purpose, Purpose::First) && limited {
            match self.triage_spam(this) {
                Ok(true) => return None,
                Ok(false) => {}
                Err(e) => {
                    let why = format!("cannot record its spam check: {e}");
                    self.triage_failed(issue, &why, true);
                    return None;
                }
            }
        }
        let listed: Vec<Listed> = (open.iter())
            .filter(|i| i.number != issue)
            .map(|i| (i.number, i.title.clone()))
            .collect();
        // Read by the poll where it had a use for them. A poll that read none leaves
        // the issue to the next.
        let pulls = match pulls? {
            Ok(pulls) => pulls,
            Err(e) => {
                let why = format!("cannot list the open pull requests: {e}");
                self.triage_failed(issue, &why, !forge_is_busy(&e));
                return None;
            }
        };
        let listed_pulls: Vec<ListedPull> = (pulls.iter())
            .filter(|pull| pull.number != issue)
            .map(|pull| ListedPull {
                number: pull.number,
                title: pull.title.clone(),
                closes: pull.closes.clone(),
            })
            .collect();
        let shown: Vec<u64> = (listed.iter())
            .map(|(n, _)| *n)
            .chain(listed_pulls.iter().map(|pull| pull.number))
            .collect();
        let agent = match self.settings.triager(&self.book) {
            Ok(agent) => agent,
            Err(e) => {
                self.triage_failed(issue, &e.to_string(), true);
                return None;
            }
        };
        let most = role.most_questions(self.settings.triage.max_questions);
        // The second pass reads the questions it asked and the author's answers, each
        // fenced. It is not made without them: a triager shown neither answers from
        // the repo alone, and its `body` would be written into the issue as if the
        // author wrote it.
        let settling = match purpose {
            Purpose::Answers => self.answers_round(issue),
            Purpose::First | Purpose::LocalRead => None,
        };
        if matches!(purpose, Purpose::Answers) && settling.is_none() {
            return None;
        }
        let subject = research::Subject {
            number: issue,
            title: &this.title,
            body: &this.body,
            // The body limit is one of the four, so it reaches no issue the
            // limits do not.
            max_body_kb: if limited {
                self.settings.triage.max_body_kb
            } else {
                0
            },
            association: this.author_association,
            role,
        };
        let prompt = research::prompt(
            &subject,
            &listed,
            &listed_pulls,
            most,
            settling.as_ref(),
            earlier.as_ref().map(|(rounds, _)| rounds.as_str()),
        );
        let Some(ticket) = TicketId::from_issue(issue) else {
            self.triage_failed(issue, "no forge numbers an issue zero", true);
            return None;
        };
        let view = self.paths.triage(ticket);
        let call = match self.triage_call(issue, &agent, &view, prompt) {
            Ok(call) => call,
            Err(why) => {
                self.triage_failed(issue, &why, true);
                return None;
            }
        };
        Some(Job {
            issue,
            author: this.author.clone(),
            said: format!("{}\n\n{}", this.title.trim(), this.body.trim()),
            agent: agent.name.as_str().to_owned(),
            shown,
            purpose,
            research: Research {
                call,
                checkout: self.settings.git.checkout.clone(),
                base: self.settings.git.base().to_owned(),
                view,
                open: listed,
                most,
                role,
                pictures: Pictures {
                    tickets: Arc::clone(&self.ports.tickets),
                    repo: self.remote.clone(),
                    ticket,
                    author: this.author.clone(),
                    body: this.body.clone(),
                    folder: self.paths.triage_images(ticket),
                },
            },
        })
    }

    // The triager's call on `issue`: a fresh session in `view`, reading the repo
    // and changing nothing. It gets none of the repo's plugins, as the security
    // reviewer gets none, so it is not `prepared`.
    fn triage_call(
        &self,
        issue: u64,
        agent: &ExpertAgent,
        view: &std::path::Path,
        prompt: String,
    ) -> Result<AgentCall, String> {
        let folder = &self.paths.worker;
        let mut call = build_call(
            Role::Triager,
            TicketId::from_issue(issue),
            view,
            &agent.model,
            prompt,
        )?;
        let instructions = folder.join(format!("triage-instructions-{issue}.md"));
        super::turn::write(
            folder,
            &instructions,
            agent.prompt.as_deref().unwrap_or_default(),
        )?;
        call.settings = folder.join(format!("triage-settings-{issue}.json"));
        call.instructions = Some(instructions);
        call.tools = Tools::Review;
        // The issue skill is the format the triager writes a body in.
        call.plugin_dirs = (self.skills.issue_plugin().into_iter())
            .map(std::path::Path::to_owned)
            .collect();
        // It reads the repo, so it reads the installed sources of its
        // dependencies and toolchains, as every other call that reads code does.
        self.with_machine_sources(&mut call);
        self.with_code_graph(&mut call);
        Ok(call)
    }

    // Queues issue `issue` ahead of the untouched ones, for `shep kelpie triage`.
    // The pick rule's `since` and author tests do not apply; a recorded issue is
    // triaged again and kelpie's own `possible-spam` comes off. Refused when
    // `triage.pick` is `off`, the issue is not open or labelled, or its call is in flight.
    pub(super) fn triage_ask(&mut self, issue: u64) -> Result<(), String> {
        self.triage_allowed(issue)?;
        let open = (self.ports.tickets.open_issues(&self.remote))
            .map_err(|e| format!("cannot read the open issues: {e}"))?;
        self.triage_found(issue, &open)
    }

    // Whether `issue` may be asked for now, before the open issues are read.
    pub(super) fn triage_allowed(&self, issue: u64) -> Result<(), String> {
        if self.state.triage.is_none() {
            return Err("`triage.pick` is `off`, so no issue is triaged".to_owned());
        }
        if self.flights.triaging() == Some(issue) {
            return Err(format!("the triage call on #{issue} is in flight"));
        }
        Ok(())
    }

    // Asks for `issue` from the open issues as read. What was checked before
    // the read is checked again, since the runner may have moved since.
    pub(super) fn triage_found(&mut self, issue: u64, open: &[OpenIssue]) -> Result<(), String> {
        self.triage_allowed(issue)?;
        let Some(found) = open.iter().find(|i| i.number == issue) else {
            return Err(format!("#{issue} is not an open issue"));
        };
        // `needs-triage` lets the verb through, whoever put it on: it is a queue
        // state, and the `possible-spam` its classifier put on is the one the
        // maintainer overrules by asking for it.
        let held = (found.labels.iter())
            .find(|l| TRIAGE_LABELS.contains(&l.as_str()) && *l != POSSIBLE_SPAM && *l != TRIAGE);
        if let Some(label) = held {
            return Err(format!(
                "#{issue} carries `{label}`, so it is not untouched"
            ));
        }
        // Asking for an issue read as spam is the maintainer's answer to that read, so
        // kelpie's own `possible-spam` comes off now: else it would stay on the board as
        // suspected spam. One kelpie's record does not say it put on came by hand and
        // stands. A record keeps the issue out of the pick rule, so no second triage opens.
        if self.triage_record(issue).is_some_and(|t| t.spam.is_some())
            && let Err(e) = self.label_issue(issue, POSSIBLE_SPAM, false)
        {
            self.notes.push(format!(
                "cannot take `{POSSIBLE_SPAM}` off issue #{issue}: {e}"
            ));
        }
        if !self.triage_queue.contains(&issue) {
            self.triage_queue.push(issue);
        }
        // Asked again by hand, it is tried now, and whatever kept it gives way. Its
        // comments are read only once a post may have gone up, since the one
        // asked for is the first of the new triage.
        self.triage_tries.insert(issue, comment::Tries::asked());
        self.flights.triage_forget(issue);
        self.looks.board_moved();
        // The pass lists the issues again, since the triager's call is made from
        // that listing and the one it holds may predate the issue.
        self.looks.triage_asked();
        Ok(())
    }
}

// Whether `login` is the App's own, as the App's login names it, with or without
// its `[bot]`
fn is_app(login: &str, app: Option<&str>) -> bool {
    app.is_some_and(|app| plain_login(app) == plain_login(login))
}

// Whether a person or the project's own App opened `issue`: any other bot's is no
// work for the check or the triager
pub(super) fn opened_by_us_or_people(issue: &OpenIssue, app: Option<&str>) -> bool {
    !issue.author_is_bot || is_app(&issue.author, app)
}

// Whether `issue` is untouched, `owed` the triager for a failed check, and opened by
// no bot but the App, after `since` unless it carries `ready-for-agent` or was `taken`
// back from it, and by `only` or the App when one is named. `needs-triage` and a
// refused `ready-for-agent` are no decision: kelpie's record is.
fn untouched(
    issue: &OpenIssue,
    since: Timestamp,
    pruned_through: u64,
    only: Option<&str>,
    owed: bool,
    app: Option<&str>,
    taken: bool,
) -> bool {
    let decided = (issue.labels.iter()).any(|l| {
        TRIAGE_LABELS.contains(&l.as_str())
            && (l != TRIAGE || issue.number <= pruned_through)
            && l != READY
    });
    let held = taken || issue.labels.iter().any(|l| l == READY);
    owed && !decided
        && opened_by_us_or_people(issue, app)
        && (held || issue.created > since)
        && only.is_none_or(|login| {
            issue.author.eq_ignore_ascii_case(login) || is_app(&issue.author, app)
        })
}

// The question texts a questions block asks: the lines `questions_block`
// headed with its mark and a number, each held as the block wrote it
fn asked_in(body: &str) -> Vec<String> {
    (body.lines())
        .filter_map(|line| {
            let rest = (line.trim_start().strip_prefix('\u{2753}'))?.trim_start();
            let rest = (rest.strip_prefix('Q'))?.trim_start();
            let (_, question) = rest.split_once(" - ")?;
            (!question.trim().is_empty()).then(|| question.trim().to_owned())
        })
        .collect()
}

#[cfg(test)]
mod addressed_tests;
#[cfg(test)]
mod answers_tests;
#[cfg(test)]
mod ask_tests;
#[cfg(test)]
mod check_tests;
#[cfg(test)]
mod hand_tests;
#[cfg(test)]
mod images_tests;
#[cfg(test)]
mod limits_tests;
#[cfg(test)]
mod local_tests;
#[cfg(test)]
mod mention_tests;
#[cfg(test)]
mod pick_tests;
#[cfg(test)]
mod reach_tests;
#[cfg(test)]
mod ready_tests;
#[cfg(test)]
mod repair_tests;
#[cfg(test)]
mod research_tests;
#[cfg(test)]
mod roles_tests;
#[cfg(test)]
mod scrub_tests;
#[cfg(test)]
mod spam_tests;
#[cfg(test)]
mod verb_tests;
