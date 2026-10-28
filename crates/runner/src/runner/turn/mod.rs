//! The worker's turns
//!
//! A turn begins (prepare the worktree and profile, mark the turn running,
//! save), its call runs in flight ([`super::flight`]), and a later pass
//! ends it (record the call, save). A turn still marked running when the
//! runner starts was cut short, and its session is resumed. A session cut
//! short before it wrote anything starts over. A turn that ends on a
//! question block parks the worker on a ruling.

use std::num::NonZeroU32;
use std::path::Path;
use std::time::Duration;

use super::Runner;
use super::adopt;
use super::briefing::git::files_on;
use super::browser;
use super::claim::main_fetch_key;
use super::claude_files::Unchecked;
use super::contributions::Sent;
use super::git_reads::start_of;
use super::instructions;
use super::pace::Pace;
use super::pm::WORKER_COMPACT_PAST;
use super::pull_request_file;
use super::question::asked;
use super::report::{Begin, StepReport};
use super::rework;
use super::ruling::park;
use crate::board::briefing::{closest, missing_paths, names_a_path};
use crate::guard::Pause;
use crate::pacer::Scope;
use crate::ports::{
    AgentCall, AgentError, AgentReply, COMPACT, Cost, InstallationToken, Issue, Reach, Role,
    Session, ThreadComment, Timestamp, Tools,
};
use crate::profile::{self, WorkerProfile};
use crate::settings::{AgentHarness, Effort, Limit};
use crate::skills::{Step, split_command};
use crate::state::{Resume, RulingKind, StateError};
use crate::ticket::TicketId;
use crate::work_item::{Phase, Review, ReviewStage, Seat, Turn, WorkItem};
use crate::worker_folder;
use crate::worktree::{self, Start};
use handover::{Handover, Refused};
use held_up::Reason;
use unfinished::{awaits_a_push, timed_out, uncommitted_prompt};
pub(super) use unfinished::{failed, named_files};

mod budget;
mod dep_reads;
mod end_reads;
mod ended;
mod handoff;
mod handover;
pub(super) mod held_up;
mod issue_comments;
mod maintainer_login;
pub(super) mod paused;
pub(super) mod reads;
mod unfinished;
pub(super) mod unreachable;

/// What a work item's worker runs on this turn
pub(super) struct WorkerAgent {
    /// Its harness
    pub(super) harness: AgentHarness,
    /// What holds its turns back
    pub(super) limit: Limit,
    /// The model, as the harness takes it
    pub(super) model: String,
    /// How hard the model thinks
    pub(super) effort: Effort,
    /// The tools its agent file names, in place of a worker's own
    pub(super) tools: Option<String>,
    /// Its agent file's body, added to kelpie's own instructions
    pub(super) prompt: Option<String>,
}

/// The prompt for a turn resumed after the runner restarted
pub(super) const CONTINUE: &str = "Kelpie restarted while your last turn was running. \
                                   Carry on with the work item from where you left off.";

/// What every fix turn's prompt asks, so the pull request's body stays true
/// to the head the turn pushes
///
/// It sits where the prompt asks for the push. A prompt may add further
/// instructions after it. It says nothing of how: the worker's instructions
/// do ([`profile`]), for a worker that edits the pull request itself and for
/// one that rewrites the file kelpie sets the body from.
pub(super) const KEEP_BODY_TRUE: &str = "Update the pull request's body so it matches the head \
    you push: what changed, the files touched, the test evidence, and the decisions you made.";

/// How a worker's prompt names its ticket
///
/// A GitHub ticket is an issue, which the worker can read with `gh`. A ticket
/// from another source is only a ticket.
#[must_use]
pub(super) fn ticket_name(ticket: TicketId) -> String {
    match ticket.github() {
        Some(_) => format!("issue #{ticket}"),
        None => format!("ticket {ticket}"),
    }
}

// The title and body, then the comments the maintainer wrote on the issue
// before this turn began, if any.
fn first_prompt(ticket: TicketId, issue: &Issue, comments: &[ThreadComment]) -> String {
    format!(
        "Your work item is {}: {}\n\n{}\n{}",
        ticket_name(ticket),
        issue.title,
        issue.body.trim_end(),
        issue_comments::block(ticket, comments)
    )
}

/// What a step can work on
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Slot {
    /// The open work item for this ticket
    Item(TicketId),
    /// The board, while a slot is free under `concurrency.active_items` and fewer than
    /// `concurrency.pending_rulings` items are parked on rulings other than a follow-up.
    /// 0 counts as 1: the board opens nothing while any such ruling waits.
    Board,
    /// The contributions' calls, while a slot is free and the board is closed
    /// (by parked rulings, or by finishing), so they still start
    Contributions,
}

impl Runner {
    // Begins a pass: its snapshot, and what a trigger reads. What follows asks the forge, a step
    // at a time, with the runner's lock held for that step only.
    pub(super) fn open_pass(&mut self) {
        self.begin_pass();
        self.looks.pass_began(self.forge_ledger.writes());
        self.post();
    }

    // Follows the contributions and settles what the rotation reads, and returns the rotation:
    // the open work items with no call in flight, and the board while a slot is free, from the
    // one after the last to act, with the time its steps are judged at
    pub(super) fn plan_rotation(&mut self) -> Result<(Vec<Slot>, Timestamp), StateError> {
        self.follow_contributions()?;
        let open: Vec<TicketId> = self.state.work_items.iter().map(|i| i.issue).collect();
        self.unreachable.keep_only(&open);
        self.held_up.keep_only(&open);
        self.queued_reruns.keep_only(&open);
        self.no_checks.retain(|ticket| open.contains(ticket));
        self.seat_waiting()?;
        let now = self.ports.clock.now();
        let rotation = self.rotation();
        self.note_left_out(&rotation, now);
        self.looks.keep_only(&rotation);
        Ok((rotation, now))
    }

    // Steps `slot`, with what a trigger reads posted before the step asks the forge with the
    // runner's lock held. A step that asks for a read is not recorded as stepped, since it has
    // not finished.
    pub(super) fn step_slot(&mut self, slot: Slot, now: Timestamp) -> Result<Begin, StateError> {
        self.post();
        let begin = match slot {
            Slot::Item(ticket) => {
                self.focus = Some(ticket);
                // Attached, or in flight, since the rotation was planned or the read began
                if self.current().is_some_and(|item| self.rotates(item)) {
                    let begin = self.begin_item(false)?;
                    if !matches!(begin, Begin::Reads(_)) {
                        self.note_held_up(ticket, &begin);
                    }
                    begin
                } else {
                    // The reason a step that asked for a read left belongs to this item alone.
                    self.held_up.forget_pending();
                    Begin::Idle
                }
            }
            Slot::Board => {
                self.focus = None;
                self.dispatch()?
            }
            Slot::Contributions => {
                self.focus = None;
                self.contribution_due()?.unwrap_or(Begin::Idle)
            }
        };
        if !matches!(begin, Begin::Reads(_)) {
            let held = self.forge_hold.until(now);
            self.looks.stepped(slot, &begin, now, held);
        }
        Ok(begin)
    }

    // Begins `ticket`'s turn again, as its session died unborn
    pub(super) fn step_over(&mut self, ticket: TicketId) -> Result<Begin, StateError> {
        self.focus = Some(ticket);
        let begin = if self.current().is_some_and(|item| item.attached.is_some()) {
            self.held_up.because(Reason::Attached);
            Begin::Idle
        } else {
            self.begin_item(true)?
        };
        if !matches!(begin, Begin::Reads(_)) {
            self.note_held_up(ticket, &begin);
        }
        Ok(begin)
    }

    // Remembers why `ticket`'s step began `begin`, and logs a turn held up long
    pub(in crate::runner) fn note_held_up(&mut self, ticket: TicketId, begin: &Begin) {
        let now = self.ports.clock.now();
        if let Some(line) = self.held_up.stepped(ticket, begin, now) {
            self.notes.push(line);
        }
    }

    // Says what holds back each open work item the rotation leaves out, whose
    // turn is due, and forgets the wait of the rest: the rotation never steps
    // them, so no step would
    fn note_left_out(&mut self, rotation: &[Slot], now: Timestamp) {
        let left_out: Vec<(TicketId, Option<Reason>)> = (self.state.work_items.iter())
            .filter_map(|i| {
                let ticket = i.issue;
                if rotation.contains(&Slot::Item(ticket)) {
                    return None;
                }
                let due = matches!(i.turn, Turn::Due | Turn::Next { .. });
                let reason = if !due || self.flights.flying(ticket) {
                    None
                } else if i.attached.is_some() {
                    Some(Reason::Attached)
                } else {
                    (i.seat == Seat::Waiting).then_some(Reason::Unseated)
                };
                Some((ticket, reason))
            })
            .collect();
        for (ticket, reason) in left_out {
            if let Some(reason) = reason {
                self.held_up.because(reason);
            }
            if let Some(line) = self.held_up.stepped(ticket, &Begin::Idle, now) {
                self.notes.push(line);
            }
        }
    }

    // The open work items with no call in flight, none attached and none
    // waiting for a slot, oldest first, then the board while it may open one,
    // then the contributions' calls while the board is closed and a slot is
    // free, from the one after the last to act, so one cannot starve the rest.
    pub(super) fn rotation(&self) -> Vec<Slot> {
        let items = (self.state.work_items.iter())
            .filter(|i| self.rotates(i))
            .map(|i| Slot::Item(i.issue));
        let board = self.board_open().then_some(Slot::Board);
        let contributions = (board.is_none() && self.slot_free() && self.contribution_unread())
            .then_some(Slot::Contributions);
        let mut slots: Vec<Slot> = items.chain(board).chain(contributions).collect();
        if let Some(at) = slots.iter().position(|&s| Some(s) == self.last_acted) {
            slots.rotate_left(at + 1);
        }
        slots
    }

    // Whether the rotation steps `item`: it has no call in flight, is not attached and does not
    // wait for a seat. The lock is let go between steps and across a read, so a step asks again.
    fn rotates(&self, item: &WorkItem) -> bool {
        !self.flights.flying(item.issue) && item.attached.is_none() && item.seat != Seat::Waiting
    }

    // Idle for `reason`, which `status` and the board name
    fn idle(&mut self, reason: Reason) -> Begin {
        self.held_up.because(reason);
        Begin::Idle
    }

    fn begin_item(&mut self, start_over: bool) -> Result<Begin, StateError> {
        let Some(item) = self.current() else {
            return Ok(Begin::Idle);
        };
        if self.unseated() {
            return Ok(self.idle(Reason::Unseated));
        }
        let owes_replies =
            item.talked.head.is_some() && matches!(item.phase, Phase::Review(_) | Phase::Ci { .. });
        if owes_replies && let Some(begin) = self.answer_talk()? {
            return Ok(begin);
        }
        let Some(item) = self.current() else {
            return Ok(Begin::Idle);
        };
        match &item.phase {
            Phase::Implement => {}
            // A fix turn, or a turn to push before a round, that ended goes
            // back to its round, to check it pushed.
            Phase::Review(review)
                if matches!(
                    review.stage,
                    ReviewStage::Fixing { .. } | ReviewStage::Pushing
                ) && !matches!(item.turn, Turn::Ended { .. }) => {}
            Phase::Review(_) => {
                if let Some(parked) = self.fence_gate()? {
                    return Ok(parked);
                }
                return self.review_step();
            }
            Phase::Ci { .. } => return self.check_ci(),
            Phase::Ruling { id } => return self.late_while_parked(*id),
            Phase::Merge { .. } => return self.merge(),
            Phase::Done { merged, .. } => return self.finish(*merged),
        }
        // A turn whose model host could not be reached is tried again as it
        // was, once its pause has passed.
        if !start_over && let Some(retry) = self.retry_for_host(item.issue) {
            return Ok(retry);
        }
        // Only a turn that has not begun waits on the pacer: one already
        // running carries on, since a turn is never interrupted, and start_over
        // resumes the same not-yet-begun turn after a session died unborn.
        let due = matches!(item.turn, Turn::Due | Turn::Next { .. });
        let kept_paused = self.pause_held(item);
        let started = self.reads.has_started(item.issue);
        if let Some(begin) = self.pushed_by_someone_else()? {
            // A read the gate asks for is made and the step made again in the gate's phase, which
            // would not say why, so the reason stands until that step ends.
            if matches!(begin, Begin::Idle | Begin::Reads(_)) {
                self.held_up.because(Reason::PushedByAnother);
            }
            return Ok(begin);
        }
        // A rework can start on a branch that already changes them.
        if due && let Some(parked) = self.claude_files_changed(Unchecked::CarryOn)? {
            return Ok(parked);
        }
        if due && let Some(parked) = self.park_over_budget()? {
            return Ok(parked);
        }
        if due && kept_paused {
            return Ok(self.idle(Reason::PauseHeld));
        }
        if due && let Some(why) = self.builds_hold() {
            return Ok(self.idle(why));
        }
        // A step made again over the start's reads or the worktree was paced clear by the step
        // before it, and asking again would read the account's usage twice for one turn.
        if due && !started && !self.reads.git.is_prepared() {
            if let Some(handed) = self.hand_off()? {
                return Ok(handed);
            }
            let pace = self.pace_worker(Scope::Turn)?;
            if let Pace::StillHeld(hold) | Pace::Held(hold) = &pace {
                self.held_up.because(Reason::Paced(hold.reason.clone()));
            }
            if let Some(held) = pace.holds() {
                return Ok(held);
            }
        }
        let Some(item) = self.current() else {
            return Ok(Begin::Idle);
        };
        let id = item.session.clone();
        let now = self.ports.clock.now();
        // A turn already running when the runner starts keeps the start it
        // was saved with, so a restart can never buy it a fresh ceiling: the
        // call it resumes gets only however much of the ceiling is left.
        let (session, prompt, since) = match &item.turn {
            Turn::Due => (Session::New(id), None, now),
            Turn::Running { since, .. } if start_over => (Session::New(id), None, *since),
            Turn::Running { since, .. } => (Session::Resume(id), Some(CONTINUE.to_owned()), *since),
            // An adopted work item's session begins with whatever the gate sends it.
            Turn::Next { prompt } if adopt::unborn(item) => {
                let (command, then) = split_command(prompt);
                let first = adopt::first_prompt(item, Some(then));
                let first = match command {
                    Some(command) => format!("{command} {first}"),
                    None => first,
                };
                (Session::New(id), Some(first), now)
            }
            // No session to resume: the retry of a turn whose call failed
            // before its session existed starts it over, as a killed one does.
            Turn::Next { .. } if start_over => (Session::New(id), None, now),
            Turn::Next { prompt } => (Session::Resume(id), Some(prompt.clone()), now),
            // A first turn that ended with no pull request and no question
            // stopped short, maybe on a tool that failed.
            Turn::Ended { .. } if item.pull_request.is_none() || awaits_a_push(item) => {
                return self.stopped_short();
            }
            Turn::Ended { .. } | Turn::Failed { .. } => return Ok(Begin::Idle),
        };
        // A session started over begins from its brief, then gets what the lost one was sent.
        let lost = match &item.turn {
            Turn::Next { prompt } if start_over => Some(prompt.clone()),
            Turn::Running { prompt, .. } if start_over => prompt.clone(),
            _ => None,
        };
        // A turn the runner sends into a session grown past the mark is
        // sent behind a `/compact`, which leaves the turn due as it was.
        let compacting = matches!(item.turn, Turn::Next { .. })
            && matches!(session, Session::Resume(_))
            && item.context.is_some_and(|c| c > WORKER_COMPACT_PAST);
        // What a yes on this turn's failure sends again: the prompt a resumed
        // turn was sent, which a restart of the turn leaves as it was.
        let kept = match &item.turn {
            Turn::Next { prompt } if !start_over => Some(prompt.clone()),
            Turn::Running { prompt, .. } if !start_over => prompt.clone(),
            _ => None,
        };
        let prompt = if compacting {
            Some(COMPACT.to_owned())
        } else {
            prompt
        };
        let ceiling = self.turn_ceiling();
        let elapsed = Duration::from_secs(now.0.saturating_sub(since.0));
        let remaining = ceiling.saturating_sub(elapsed);
        if remaining.is_zero() {
            // The ceiling passed while kelpie was down, before a new call
            // could even be tried: park it as a call that hit
            // `AgentError::TimedOut` would, with no call spent.
            return self.park_ceiling_passed(now);
        }
        if self.draining() {
            return Ok(self.idle(Reason::Draining));
        }
        let issue = item.issue;
        let ticket = item.issue;
        // A later turn restarted with a new session, as one that found no
        // session to resume is, has a worker call on record and is not first.
        let first = matches!(session, Session::New(_))
            && prompt.is_none()
            && !(item.rework || item.adopted)
            && !item.calls.iter().any(|c| c.role == Role::Worker);
        // A token GitHub would not give a moment ago is not asked for at every pass.
        if self.token_wait.holds(now) {
            return Ok(self.idle(Reason::Token));
        }
        // The token and the first prompt's issue are read with the lock let go, and the step is
        // made again over them. Asked for only here, past every check that holds the turn back.
        if let Some(job) = self.start_job(item, takes_issue(item, prompt.as_deref())) {
            return Ok(Begin::Reads(job));
        }
        // Its worktree is made the same way, once the token is in hand.
        if let Some(reads) = self.preparing() {
            return Ok(reads);
        }
        // Its packages are installed in the worker's sandbox once the worktree is there. A
        // compaction runs no code.
        if !compacting && let Some(reads) = self.installing() {
            return Ok(reads);
        }
        self.token_wait.begins();
        let prepared = self.prepare(item, session, prompt, lost.as_deref());
        let mut next = self.state.clone();
        let mut resumed = false;
        let mut begin = match prepared {
            // A covered worker never runs as the maintainer, so a token that
            // passes waits here and is tried again, not parked on a ruling.
            Err(reason) if self.token_wait.is_busy() => {
                if self.token_wait.first_told() {
                    self.notes
                        .push(format!("issue #{issue} waits to start its turn: {reason}"));
                }
                return Ok(self.idle(Reason::Token));
            }
            Ok(call) => {
                // `prepare` mints the worker's GitHub token and prepares its
                // worktree with the runner's lock held, and a `drain` that lands
                // in that time sets its flag there without waiting for it.
                // Checked again here, so the turn stays due rather than starting late.
                if self.draining() {
                    self.token_wait.ends();
                    // No flight will take the bridge this turn's call opened.
                    drop(self.opened.take(issue));
                    return Ok(self.idle(Reason::Draining));
                }
                self.token_wait.ends();
                resumed = !compacting && paused::resumes_pause(item);
                self.builds_turn_begins();
                // Dropped or moved on since the check, no call starts.
                let Some(item) = self.current_in(&mut next) else {
                    // No flight will take the bridge this turn's call opened.
                    drop(self.opened.take(issue));
                    return Ok(Begin::Idle);
                };
                if !compacting {
                    item.turn = Turn::Running {
                        since,
                        prompt: kept,
                    };
                }
                let deadline = Timestamp(since.0.saturating_add(ceiling.as_secs()));
                Begin::Call {
                    call,
                    deadline,
                    first,
                    compact: compacting,
                }
            }
            Err(reason) => {
                let names = self.names();
                let restore = self.restorable(issue);
                Begin::Report(failed(names, &mut next, issue, now, reason, restore, false))
            }
        };
        if let Err(e) = self.save(next) {
            // No flight will take the bridge this turn's call opened.
            drop(self.opened.take(issue));
            return Err(e);
        }
        self.packages_installed(issue);
        if resumed {
            self.note_resumed(ticket);
        }
        if let Begin::Report(report) = &mut begin {
            self.fill_comment_failed(report);
        }
        Ok(begin)
    }

    // Everything the worker needs on disk before it starts: its worktree, its
    // build folder, its settings file and kelpie's instructions. A turn with
    // no prompt of its own is the first, and takes the issue, the review or
    // what it adopted, followed by `lost`, what a session it starts over lost.
    pub(super) fn prepare(
        &self,
        item: &WorkItem,
        session: Session,
        prompt: Option<String>,
        lost: Option<&str>,
    ) -> Result<AgentCall, String> {
        // A session with no prompt (attach) or only a compaction runs no browser.
        let browses = prompt
            .as_deref()
            .is_none_or(|p| !p.is_empty() && p != COMPACT);
        let call = self.worker_call(item, session, prompt, lost, browses)?;
        self.prepared(call)
    }

    // The worker's call for `item`, with its worktree, build folder, settings
    // path and instructions made, before its harness has written anything.
    // `browses` is whether its instructions may name a browser, which only a
    // turn that goes on to open one has.
    pub(super) fn worker_call(
        &self,
        item: &WorkItem,
        session: Session,
        prompt: Option<String>,
        lost: Option<&str>,
        browses: bool,
    ) -> Result<AgentCall, String> {
        let start = start_of(item);
        let WorkerAgent {
            harness,
            limit: _,
            model,
            effort,
            tools: agent_tools,
            prompt: agent_prompt,
        } = self.worker_agent(item)?;
        // Where the App covers the repo, the worker holds its token and never
        // the maintainer's login; a token it cannot have now fails the call
        // rather than run it as the maintainer.
        let started = self.reads.started(item.issue);
        let token = (started.token())
            .unwrap_or_else(|| self.ports.voice.worker_token(&self.remote))
            .map_err(|e| {
                self.token_error_passes(&e);
                format!("cannot get the worker a GitHub token: {e}")
            })?;
        let reach = self.worker_reach(item, start, token.as_ref())?;
        // Named for the work item: the open items' calls run at once, and
        // each harness keeps its own files beside its settings file.
        let folder = &self.paths.worker;
        let settings = folder.join(format!("settings-{}.json", item.issue));
        let instructions = folder.join(format!("instructions-{}.md", item.issue));
        let handed_over = token
            .is_some()
            .then(|| pull_request_file::path(&item.build));
        let hosts = (self.browser_hosts(&harness, &session, Some(item.issue))).filter(|_| browses);
        let dev = self.dev_server(item, &reach, hosts);
        // The owner's template reaches the worker as a file in its build folder,
        // which it reads when it writes its body. Written on every call, so a
        // template kelpie reread is the one the next read finds. The worktree's
        // own template outranks it, and then no file is written.
        let owners = self
            .owner_template
            .as_ref()
            .filter(|_| instructions::template(&item.worktree).is_none());
        let template_file = match owners {
            Some(owners) => {
                let path = item.build.join(instructions::TEMPLATE_FILE);
                write(&item.build, &path, &owners.text())?;
                Some(path)
            }
            None => None,
        };
        let text = instructions::compose(
            instructions::Extra {
                agent: agent_prompt.as_deref(),
                project: self.extra_instructions.as_deref(),
                // Its token opens no pull request, so kelpie opens it.
                pull_request_file: handed_over.as_deref(),
                e2e: self.settings.e2e.as_ref(),
                owner_template: owners.zip(template_file.as_deref()),
                browser: hosts,
                dev_server: dev.as_ref(),
                local_ticket: item.issue.github().is_none(),
                base: Some(self.item_base(item)),
                kelpie_pushes: !item.remote.is_origin(),
            },
            &item.worktree,
            &self.skills,
            &self.kelpie,
        );
        write(folder, &instructions, &text)?;
        // A pause set for an earlier call, or one that ended it, is not this call's.
        Pause::in_build(&item.build).clear();
        let briefed = prompt.is_none();
        let prompt = match prompt {
            Some(prompt) => prompt,
            None if item.rework => rework::first_prompt(item),
            None if item.adopted => adopt::first_prompt(item, None),
            None => {
                let ticket = item.issue;
                let issue = (started.issue())
                    .unwrap_or_else(|| self.ports.tickets.issue(&self.remote, ticket))
                    .map_err(|e| format!("cannot read {}: {e}", ticket.shown()))?;
                let comments = self.maintainer_comments(ticket, started.comments())?;
                let first = first_prompt(ticket, &issue, &comments);
                let moved = moved_paths(&issue.body, || {
                    files_on(&self.settings.git.checkout, self.item_base(item))
                });
                self.skills
                    .invoke(Step::Implement, &format!("{first}{moved}"))
            }
        };
        let prompt = match briefed {
            true => handoff::carried(prompt, lost, item),
            false => prompt,
        };
        let prompt = match self.reads.git.installed().and_then(|i| i.failed.as_deref()) {
            Some(why) if prompt != COMPACT => format!("{prompt}\n\n{}", install_failed(why)),
            _ => prompt,
        };
        // A `/compact` has nothing to bound, and would hand itself back after.
        let compact_window =
            NonZeroU32::new(self.settings.worker.compact_window).filter(|_| prompt != COMPACT);
        Ok(AgentCall {
            issue: Some(item.issue),
            settings,
            instructions: Some(instructions),
            plugin_dirs: self.skills.plugin_dirs().to_vec(),
            agent_tools,
            reach,
            compact_window,
            ..AgentCall::new(
                Role::Worker,
                Tools::Work,
                harness,
                model,
                effort,
                session,
                item.worktree.clone(),
                prompt,
            )
        })
    }

    /// What a session working in `item`'s worktree reaches: the worker's own
    /// fence, with its worktree and build folder made ready
    ///
    /// # Errors
    ///
    /// Why the worktree cannot be made ready, or why agents' own files in it
    /// are refused.
    pub(super) fn worker_reach(
        &self,
        item: &WorkItem,
        start: Start,
        token: Option<&InstallationToken>,
    ) -> Result<Reach, String> {
        let dirs = self.worktree_made(item, start).map_err(|e| e.to_string())?;
        // The worker merges `origin/<base>` and runs no fetch of its own, so a
        // fetch that failed leaves it merging a base kelpie last fetched.
        let key = main_fetch_key(item.issue);
        match &dirs.main_fetch_failed {
            Some(why) => self.notes.standing(
                &key,
                format!(
                    "issue #{}'s worker runs with `origin/{}` where kelpie last fetched it: \
                     {why}",
                    item.issue,
                    self.item_base(item)
                ),
            ),
            None => self.notes.ends(&key),
        }
        if let Some(reason) = self.claude_files_refusal() {
            return Err(reason);
        }
        // Its login is written for this call, and a call whose login cannot be
        // written does not start: `gh` would find the maintainer's instead.
        if let Some(token) = token {
            profile::write_login(&item.build, token)
                .map_err(|e| format!("cannot write the worker's GitHub login: {e}"))?;
        }
        let login_token = self.maintainers_login(token)?;
        let registries = self.registries(&item.worktree);
        let profile = WorkerProfile {
            worktree: &item.worktree,
            build: &item.build,
            git_common_dir: &dirs.git_common_dir,
            git_dir: &dirs.git_dir,
            branch: &item.branch,
            base: self.item_base(item),
            // A contributor's fork is out of the worker's token's reach, and
            // the login that reaches it is kelpie's alone: it pushes there.
            kelpie_pushes: !item.remote.is_origin(),
            kelpie: &self.kelpie,
            skills: &self.paths.skills,
            guard_hooks: &self.settings.worker.guard_hooks,
            kelpie_home: &self.paths.kelpie_home,
            repo: &self.settings.git.checkout,
            allowed_domains: &self.settings.worker.allowed_domains,
            registries: &registries,
            build_env: &self.settings.worker.build_env,
            home: &self.home,
            shep_home: &self.paths.shep_home,
            door: &self.paths.door,
            token,
            login_token: login_token.as_ref(),
        };
        // The guard in the fence runs the project's `commit`, `push` and
        // `publish` hooks, so every harness reaches them with its own checks.
        let mut reach = profile.reach();
        if let Some(fence) = reach.fence.as_mut() {
            fence.guard.hooks = self.hooks.get().as_ref().clone().in_folder(&item.worktree);
        }
        Ok(reach)
    }

    /// The registries of the package managers `worktree` keeps files for, none while
    /// `worker.package_registries` is off
    fn registries(&self, worktree: &Path) -> Vec<&'static str> {
        if !self.settings.worker.package_registries {
            return Vec::new();
        }
        crate::package_managers::registries(&crate::package_managers::found_in(worktree))
    }

    /// The agent `item`'s worker runs on: the one it opened on, as that
    /// agent's file now stands
    ///
    /// # Errors
    ///
    /// Why not, when kelpie no longer has that agent, `agents.allow` now leaves
    /// it out, or the worker's fence would not hold on it.
    pub(super) fn worker_agent(&self, item: &WorkItem) -> Result<WorkerAgent, String> {
        let name = &item.agent;
        let Some(agent) = self.book.get(name) else {
            if let Some(error) = self.book.broken(name) {
                return Err(format!(
                    "issue #{} runs on agent {name}, whose {error}",
                    item.issue
                ));
            }
            return Err(format!(
                "issue #{} runs on agent {name}, which has no agent file now: write \
                 `agents/{name}.md` in kelpie's home again, or drop and add the issue",
                item.issue
            ));
        };
        if (self.settings.agents.allow.as_ref()).is_some_and(|allow| !allow.holds(name)) {
            return Err(format!(
                "issue #{} runs on agent {name}, which `agents.allow` leaves out: add it to \
                 `agents.allow`, or drop and add the issue",
                item.issue
            ));
        }
        let Some((model, limit)) = agent
            .runs
            .session()
            .filter(|_| agent.role == crate::agents::Role::Implementer)
        else {
            return Err(format!(
                "issue #{} runs on agent {name}, whose file is a reviewer's now: give \
                 `agents/{name}.md` in kelpie's home `role: implementer` again, or drop and \
                 add the issue",
                item.issue
            ));
        };
        self.settings
            .under_worker_fence(name, model)
            .map_err(|e| e.to_string())?;
        Ok(WorkerAgent {
            harness: model.harness.clone(),
            limit: limit.clone(),
            model: model.model.as_str().to_owned(),
            effort: model.effort,
            tools: model.tools.as_ref().map(|t| t.as_str().to_owned()),
            prompt: agent.prompt.clone(),
        })
    }

    /// `call`, once its harness has what it needs on disk
    ///
    /// Done before the call is marked running, so a failure keeps the turn
    /// it would have run for a retry.
    pub(super) fn prepared(&self, mut call: AgentCall) -> Result<AgentCall, String> {
        // The plugins the repo turns on reach every call the runner makes in it,
        // unless the project's `repo_setup` keeps them from this call's role.
        if self.settings.repo_setup.reaches(call.role) {
            // A call in a worktree that matches `main` loads the repo's own
            // `.claude` files from it already, and must not get them twice.
            let item = call.issue.and_then(|ticket| self.state.item(ticket));
            let base = item.map_or(self.settings.git.base(), |item| self.item_base(item));
            let own_loaded = call.harness.reads_claude_files()
                && crate::fence::matches_main(&self.settings.git.checkout, &call.cwd, base);
            for plugin in self.repo_plugins.dirs(own_loaded) {
                if !call.plugin_dirs.iter().any(|dir| dir == plugin) {
                    call.plugin_dirs.push(plugin.to_owned());
                }
            }
            // Their MCP servers go in the call's own config, since Claude Code's
            // `--strict-mcp-config` drops every other. A call that only reads (a retro)
            // is given none that could write.
            if call.harness.loads_mcp() && call.tools != Tools::Retro {
                // A server the call already names is not replaced by a repo server of the
                // same name. goose lowercases a name into
                // its key, so a name that differs only in case is no other server either.
                for (name, entry) in self.repo_plugins.mcp_servers() {
                    let held =
                        (call.reach.mcp_servers.keys()).any(|k| k.eq_ignore_ascii_case(&name));
                    if !held {
                        call.reach.mcp_servers.insert(name, entry);
                    }
                }
                self.without_servers_the_harness_cannot_load(&mut call);
            }
        }
        self.with_dep_sources(&mut call);
        self.with_plugin_reads(&mut call);
        self.with_code_graph(&mut call);
        self.with_browser(&mut call)?;
        if let Err(e) = self.ports.agents.prepare(&call) {
            // No flight will take the bridge `with_browser` opened.
            if let Some(ticket) = browser::ticket_of(&call) {
                drop(self.opened.take(ticket));
            }
            return Err(e.to_string());
        }
        Ok(call)
    }

    // The open pull request from `branch`. A forge that cannot be asked
    // leaves it unrecorded, and status shows none.
    pub(super) fn pull_request_from(&self, branch: &str) -> Option<u64> {
        let open = self.ports.forge.open_pull_requests(&self.remote).ok()?;
        end_reads::from_branch(Some(&open), branch)
    }
}

// What the worker is told when kelpie's install of its worktree's packages failed
fn install_failed(why: &str) -> String {
    format!(
        "Kelpie tried to install this worktree's packages before your turn, and it failed. The \
         output ended:\n\n{why}\n\nThe lockfile as it stands could not be installed, so \
         `node_modules` may be missing or half made. Fix what the output names if it is yours \
         to fix, and name the gap in your final message if it is not."
    )
}

// What the worker is told of the paths its issue names that `main` lacks, or nothing. A path
// the issue means to add is among them, so the note says it may be new. `read_tree` is the files
// on `main`, asked of git only when the body names a path.
fn moved_paths(body: &str, read_tree: impl FnOnce() -> Vec<String>) -> String {
    if !names_a_path(body) {
        return String::new();
    }
    let tree = read_tree();
    // Without the files on `main`, every path would read as missing.
    let missing = if tree.is_empty() {
        Vec::new()
    } else {
        missing_paths(body, &tree)
    };
    if missing.is_empty() {
        return String::new();
    }
    let named: Vec<String> = (missing.iter())
        .map(|path| match closest(path, &tree) {
            Some(near) => format!("`{path}` (closest on `main`: `{near}`)"),
            None => format!("`{path}`"),
        })
        .collect();
    format!(
        "\nThese paths the issue names are not on `main` and may have moved, or be new: {}. \
         Look for them under their new place before you assume they are gone.\n",
        named.join(", ")
    )
}

// Whether a turn sent `prompt` is prompted with its issue: the first of a work item that is
// neither a rework nor adopted, which has no prompt of its own
fn takes_issue(item: &WorkItem, prompt: Option<&str>) -> bool {
    prompt.is_none() && !item.rework && !item.adopted
}

pub(super) fn write(folder: &Path, file: &Path, text: &str) -> Result<(), String> {
    let failed = |why: &dyn std::fmt::Display| format!("cannot write {}: {why}", file.display());
    let rel = file
        .strip_prefix(folder)
        .map_err(|_| failed(&"it is not in the folder"))?;
    worker_folder::write(folder, rel, text).map_err(|e| failed(&e))
}

#[cfg(test)]
pub(super) mod tests;
