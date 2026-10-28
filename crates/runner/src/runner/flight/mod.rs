//! Calls in flight
//!
//! The runner starts each agent call on a thread of its own and goes on, so
//! no thread of the runner's waits on one. The call's end comes back on a
//! channel, which wakes the runner's loop, and its next pass records it. A
//! work item has at most one call in flight, and the open work items' calls
//! run at once, beside the project manager's one call. A turn's ceiling is
//! a deadline each pass checks, and a call past it is ended through its own
//! process group.

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::Runner;
use super::alert::post_due;
use super::briefing::{Watched, brief};
use super::dev_server;
use super::in_flight::{InFlight, Key};
use super::login_refresh;
use super::pm::wake_off_lock;
use super::replies::answer_replies;
use super::report::{Begin, ReviewCall, Reviewed, Spent, StepReport};
use super::review::run_review_call;
use super::transcript_count::Mark;
use super::trigger::lock;
use super::turn_hooks::TurnHooks;
use crate::board::briefing::elapsed;
use crate::ports::{
    AgentCall, AgentError, AgentReply, Ending, Flying, OpenBridge, RoundStage, Timestamp, Wait,
};
use crate::settings::Harness;
use crate::state::StateError;
use crate::ticket::TicketId;
use crate::usage::{CallKind, Draft};

/// What one pass of the runner's loop did
#[derive(Debug)]
pub enum Pass {
    /// Nothing was due
    Idle,
    /// Something happened, for the runner's log
    Report(StepReport),
    /// A call started, and goes on in flight
    Started,
}

/// The runner's calls in flight, and the channel their ends come back on
#[derive(Debug)]
pub(super) struct Flights {
    flying: BTreeMap<TicketId, Flight>,
    // The project manager's call, its ceiling and its hold
    pm: Option<(Timestamp, Ending)>,
    // The security review of a contribution's head
    securing: Option<security::Securing>,
    // The triager's research of an untouched issue
    triaging: Option<triage::Triaging>,
    // The e2e run of a bump's head
    proving: proving::Proving,
    // The contributions a security review started on since the last poll, which
    // are not tried again before the next
    security_tried: Vec<u64>,
    // The issues triage looked at since the last poll
    triage_tried: Vec<u64>,
    // The project manager's batch retro
    retro: Option<retro::Retrospecting>,
    // Every call's line in the usage ledger, as it started
    ledger: InFlight,
    send: Sender<News>,
    news: Receiver<News>,
    // Told after each piece of news, so the loop waiting on it wakes
    wake: Option<Sender<()>>,
    // The models whose lease gauges were set, so one whose calls have ended is set to 0
    leased_models: BTreeSet<String>,
}

impl Default for Flights {
    fn default() -> Self {
        let (send, news) = mpsc::channel();
        Self {
            flying: BTreeMap::new(),
            pm: None,
            securing: None,
            triaging: None,
            proving: proving::Proving::default(),
            security_tried: Vec::new(),
            triage_tried: Vec::new(),
            retro: None,
            ledger: InFlight::default(),
            send,
            news,
            wake: None,
            leased_models: BTreeSet::new(),
        }
    }
}

impl Flights {
    /// Whether `ticket`'s work item has a call in flight
    pub(super) fn flying(&self, ticket: TicketId) -> bool {
        self.flying.contains_key(&ticket)
    }

    /// Whether `ticket`'s work item has a worker's turn in flight
    ///
    /// A turn marked running in the state file is not enough to call its
    /// time the worker's: a stop leaves one marked, and so does a restart
    /// until a pass resumes it.
    pub(super) fn runs_turn(&self, ticket: TicketId) -> bool {
        self.flying
            .get(&ticket)
            .is_some_and(|f| f.deadline.is_some())
    }

    /// Forgets `ticket`'s call in flight, once its end is heard
    pub(super) fn landed(&mut self, ticket: TicketId) {
        self.flying.remove(&ticket);
    }

    /// The call of `ticket`'s worker turn in flight, whether it is its work
    /// item's first, and the seconds of its ceiling it began with
    pub(super) fn turn_call(&self, ticket: TicketId) -> Option<(AgentCall, bool, u64)> {
        let flight = self.flying.get(&ticket)?;
        let call = flight.watched.call.clone()?;
        (flight.deadline.is_some() && !flight.compact).then_some((call, flight.first, flight.left))
    }

    /// When `ticket`'s call in flight started
    pub(super) fn started(&self, ticket: TicketId) -> Option<Timestamp> {
        Some(self.flying.get(&ticket)?.watched.started)
    }

    /// Whether `ticket`'s call in flight is a `/compact` of its worker's session
    pub(super) fn runs_compact(&self, ticket: TicketId) -> bool {
        self.flying.get(&ticket).is_some_and(|f| f.compact)
    }

    /// Whether the batch retro is in flight
    pub(super) fn retro_running(&self) -> bool {
        self.retro.is_some()
    }

    /// Ends `ticket`'s call in flight, as its ceiling would
    pub(super) fn end(&self, ticket: TicketId) {
        if let Some(flight) = self.flying.get(&ticket) {
            flight.ending.end();
        }
    }

    /// The calls in flight as the usage ledger knows them
    pub(super) fn in_flight(&self) -> InFlight {
        self.ledger.clone()
    }

    /// `ticket`'s call in flight, as the board watches it
    pub(super) fn watched(&self, ticket: TicketId) -> Option<&Watched> {
        self.flying.get(&ticket).map(|f| &f.watched)
    }

    /// Every call in flight by its work item's ticket, as the board watches it
    pub(super) fn watched_mut(&mut self) -> impl Iterator<Item = (TicketId, &mut Watched)> {
        self.flying
            .iter_mut()
            .map(|(&ticket, f)| (ticket, &mut f.watched))
    }
}

// One call in flight
#[derive(Debug)]
struct Flight {
    // The turn's ceiling for a worker's turn, and none for a review call
    deadline: Option<Timestamp>,
    // How much of the ceiling the turn had when it started, which it has
    // again from when it gets the lease it waited for
    left: u64,
    ending: Ending,
    // Whether this call starts over a session that died before it began
    again: bool,
    watched: Watched,
    // Whether a worker's turn is its work item's first, with no session to keep
    first: bool,
    // Whether it is a `/compact` of the worker's session, sent before a turn
    compact: bool,
    // The browser the turn drives, which lasts as long as the turn does
    browser: Option<Box<dyn OpenBridge>>,
    // Where the turn's transcript stood as it began, for a handover to carry
    requests: Option<Mark>,
    // When a handover first found the call's record gone, which ends with the flight
    record_gone: Option<Timestamp>,
}

impl Flight {
    // What the call's record keeps of this flight, counting from `began`
    fn flying(&self, began: Timestamp) -> Flying {
        Flying {
            left: self.left,
            began,
            again: self.again,
            first: self.first,
            compact: self.compact,
        }
    }
}

// A call to start, as a step found it
enum Launch {
    Turn {
        call: AgentCall,
        deadline: Timestamp,
        again: bool,
        first: bool,
        compact: bool,
    },
    Review(ReviewCall),
}

impl Runner {
    /// Wakes `wake` whenever a call in flight has news, as each call started
    /// from now on does
    pub fn wake_with(&mut self, wake: Sender<()>) {
        self.flights.wake = Some(wake);
    }

    /// How long until the soonest ceiling of a turn in flight that has not
    /// been ended yet, or `None` with no such turn
    pub fn next_ceiling(&self) -> Option<Duration> {
        let now = self.ports.clock.now();
        let turns = (self.flights.flying.values())
            .filter(|flight| !flight.ending.asked())
            .filter_map(|flight| flight.deadline);
        let pm = (self.flights.pm.iter())
            .filter(|(_, ending)| !ending.asked())
            .map(|(deadline, _)| *deadline);
        let securing = (self.flights.securing.iter()).filter_map(security::Securing::ceiling);
        let triaging = (self.flights.triaging.iter()).filter_map(triage::Triaging::ceiling);
        let retro = (self.flights.retro.iter()).filter_map(retro::Retrospecting::ceiling);
        turns
            .chain(pm)
            .chain(securing)
            .chain(triaging)
            .chain(retro)
            .map(|deadline| Duration::from_secs(deadline.0.saturating_sub(now.0)))
            .min()
    }

    // Ends each call in flight past its turn's ceiling. Its end comes back
    // as a call that timed out, and parks as one.
    fn end_overdue(&self) {
        let now = self.ports.clock.now();
        for flight in self.flights.flying.values() {
            if flight.deadline.is_some_and(|deadline| deadline <= now) {
                flight.ending.end();
            }
        }
        if let Some((deadline, ending)) = &self.flights.pm
            && *deadline <= now
        {
            ending.end();
        }
        if let Some(securing) = &self.flights.securing {
            securing.end_overdue(now);
        }
        if let Some(triaging) = &self.flights.triaging {
            triaging.end_overdue(now);
        }
        if let Some(retro) = &self.flights.retro {
            retro.end_overdue(now);
        }
    }

    /// Starts the project manager's `call`, a `kind` on the agent named
    /// `agent`, on a thread of its own, ended past `ceiling` seconds, whose
    /// end comes back as news
    pub(super) fn launch_pm(&mut self, call: AgentCall, ceiling: u64, agent: &str, kind: CallKind) {
        let now = self.ports.clock.now();
        let draft = Draft::of(&call, agent, kind, now);
        self.flights
            .ledger
            .open(Key::Manager, draft, self.pacer_lines());
        self.call_started(&call, agent);
        let ledger = self.flights.ledger.clone();
        let (news, wake) = (self.flights.send.clone(), self.flights.wake.clone());
        let ending = Ending::default();
        ending.carry(Flying {
            left: ceiling,
            began: now,
            again: false,
            first: false,
            compact: false,
        });
        let held = ending.clone();
        let agents = self.counted(TurnHooks::around(
            Arc::clone(&self.ports.agents),
            &self.hooks.get(),
            &self.notes,
            None,
        ));
        let run = move || {
            let ran = catch_unwind(AssertUnwindSafe(|| End::Turn(agents.run(&call, &held))));
            let end = ran.unwrap_or_else(End::Panicked);
            if end.reached_no_model() {
                ledger.no_model(Key::Manager);
            }
            let _ = news.send(News::Pm(end));
            if let Some(wake) = &wake {
                let _ = wake.send(());
            }
        };
        if let Err(e) = thread::Builder::new().name("pm".into()).spawn(run) {
            let reason = format!("cannot start a thread for the call: {e}");
            let end = End::Turn(Err(AgentError::Spawn(Harness::ClaudeCode, reason)));
            let _ = self.flights.send.send(News::Pm(end));
        }
        self.flights.pm = Some((Timestamp(now.0.saturating_add(ceiling)), ending));
    }

    // Counts `ticket`'s turn ceiling from now, once its call has the lease it
    // waited for, and starts the board's quiet clock from the same moment.
    fn began(&mut self, ticket: TicketId) {
        let now = self.ports.clock.now();
        let told = self.flights.flying.get_mut(&ticket).map(|flight| {
            let told = flight.watched.told.take();
            let said = flight.watched.woken;
            let turn = flight.watched.call.is_some();
            let waited = told.map_or(0, |(since, _)| now.0.saturating_sub(since.0));
            flight.watched.granted = Some(now);
            flight.watched.woken = false;
            if flight.deadline.is_some() {
                flight.deadline = Some(Timestamp(now.0.saturating_add(flight.left)));
                flight.ending.carry(flight.flying(now));
            }
            let model = flight
                .watched
                .lease
                .as_ref()
                .map(|(_, model)| model.clone());
            (said, turn, told.map(|(_, why)| why), waited, model)
        });
        if let Some((.., waited, Some(model))) = &told {
            self.metrics.model_lease_granted(model, *waited);
        }
        self.model_leases();
        // The board names the gap the wait took, so a reader sees the wait was
        // real, and names what the call got. Only a wait for a lease got one.
        if let Some((said, turn, why, waited, _)) = told
            && said
        {
            let who = if turn { "worker" } else { "review call" };
            let got = match why.unwrap_or(Wait::Lease) {
                Wait::Lease => "its model's lease",
                Wait::Busy => "its gateway back",
                Wait::Silent => "its model",
                Wait::Unreachable => "its model host back",
            };
            self.board_event(format!(
                "#{ticket}: {who} has {got}, after waiting {}",
                elapsed(waited)
            ));
        }
        self.board_changed();
    }

    // Starts the call `begin` names, as a step found it. `again` is a work item whose
    // session died unborn, which begins its turn again.
    fn start(&mut self, begin: Begin, again: bool) -> Result<Pass, StateError> {
        let launch = match begin {
            Begin::Idle => return Ok(Pass::Idle),
            Begin::Report(report) => return Ok(Pass::Report(report)),
            Begin::Call {
                call,
                deadline,
                first,
                compact,
            } => Launch::Turn {
                call,
                deadline,
                again,
                first,
                compact,
            },
            Begin::Review(action) => Launch::Review(action),
            Begin::Secure(job) => {
                self.launch_security(job);
                self.post();
                return Ok(Pass::Started);
            }
            Begin::Triage(job) => {
                self.launch_triage(*job);
                self.post();
                return Ok(Pass::Started);
            }
            // The rotation runs a read before it starts anything.
            Begin::Reads(_) => return Ok(Pass::Idle),
        };
        let ticket = self.focus.expect("a call is a work item's");
        self.launch(ticket, launch);
        // The call is in flight from here, and the pass goes on with the lock
        // held. A trigger refused it answers from this post, so `drain` never
        // answers that nothing runs while a call has just started.
        self.post();
        Ok(Pass::Started)
    }

    // Starts `launch` on a thread of its own, whose end comes back as news.
    fn launch(&mut self, ticket: TicketId, launch: Launch) {
        let tell = Tell {
            ticket,
            news: self.flights.send.clone(),
            wake: self.flights.wake.clone(),
        };
        let ending = Ending::telling(
            {
                let tell = tell.clone();
                move || tell.began()
            },
            {
                let tell = tell.clone();
                move |wait| tell.waits(wait)
            },
        );
        // A turn's harness, which a turn that cannot start names
        let (deadline, again, first, harness) = match &launch {
            Launch::Turn {
                call,
                deadline,
                again,
                first,
                ..
            } => (
                Some(*deadline),
                *again,
                *first,
                Some(call.harness.harness()),
            ),
            Launch::Review(_) => (None, false, false, None),
        };
        let compact = matches!(launch, Launch::Turn { compact: true, .. });
        let role = harness.map_or("reviewer", |_| "worker");
        let now = self.ports.clock.now();
        let watched = Watched {
            started: now,
            call: match &launch {
                Launch::Turn { call, .. } => Some(call.clone()),
                Launch::Review(_) => None,
            },
            idle: false,
            told: None,
            granted: None,
            woken: false,
            lease: waiting::lease_of(&launch),
        };
        let mut draft = match &launch {
            Launch::Turn { call, .. } => {
                let kind = match compact {
                    true => CallKind::Compact,
                    false => CallKind::Turn,
                };
                Draft::of(call, &self.ledger_worker(ticket), kind, now)
            }
            Launch::Review(ReviewCall::Session(call)) => {
                Draft::of(call, &self.ledger_reviewer(ticket), CallKind::Review, now)
            }
            Launch::Review(ReviewCall::Local { local, .. }) => {
                Draft::local(local, ticket, &self.ledger_reviewer(ticket), now)
            }
        };
        // The launch names the ticket, which is what the ledger keys the call
        // by, and what its line carries.
        draft.issue = Some(ticket);
        let draft = self.with_pull_request(draft);
        self.flights
            .ledger
            .open(Key::Item(ticket), draft, self.pacer_lines());
        match &launch {
            Launch::Turn { call, .. } => self.call_started(call, &self.ledger_worker(ticket)),
            Launch::Review(ReviewCall::Session(call)) => {
                self.call_started(call, &self.ledger_reviewer(ticket));
            }
            Launch::Review(ReviewCall::Local { .. }) => {}
        }
        let ledger = self.flights.ledger.clone();
        let item = (self.state.work_items.iter()).find(|i| i.issue == ticket);
        let agents = self.counted(TurnHooks::around(
            Arc::clone(&self.ports.agents),
            &self.hooks.get(),
            &self.notes,
            item,
        ));
        let reviewer = Arc::clone(&self.ports.reviewer);
        let left = deadline.map_or(0, |deadline| deadline.0.saturating_sub(now.0));
        let flight = Flight {
            deadline,
            left,
            ending,
            again,
            watched,
            first,
            compact,
            browser: match &launch {
                Launch::Turn { .. } => self.opened.take(ticket),
                Launch::Review(_) => None,
            },
            requests: match &launch {
                Launch::Turn { call, .. } => Mark::of(self.ports.agents.as_ref(), call),
                Launch::Review(_) => None,
            },
            record_gone: None,
        };
        // Only a turn has a ceiling to carry, for its record on disk.
        if flight.deadline.is_some() {
            flight.ending.carry(flight.flying(now));
        }
        let held = flight.ending.clone();
        let (voice, remote) = (Arc::clone(&self.ports.voice), self.remote.clone());
        let (notes, metrics) = (self.notes.clone(), Arc::clone(&self.metrics));
        let servers = Arc::clone(&self.ports.dev_servers);
        let dev = match &launch {
            Launch::Turn { call, .. } => self.dev_run(call),
            Launch::Review(_) => None,
        };
        let run = move || {
            let ran = catch_unwind(AssertUnwindSafe(|| match launch {
                // A covered worker's token lasts an hour, a call may not.
                Launch::Turn { mut call, .. } => {
                    // Here, with the runner's lock let go, since an index takes a while.
                    super::code_graph::indexed(&mut call, &notes, &metrics, &held);
                    // Started here too, and stopped when the call returns, however it ends.
                    let _server = (dev.as_ref()).and_then(|run| {
                        dev_server::started(servers.as_ref(), run, &mut call, &notes, &metrics)
                    });
                    let build = login_refresh::covered_build(&call);
                    End::Turn(login_refresh::while_running(
                        voice.as_ref(),
                        &remote,
                        build.as_deref(),
                        login_refresh::EVERY,
                        || agents.run(&call, &held),
                    ))
                }
                Launch::Review(mut action) => {
                    if let ReviewCall::Session(call) = &mut action {
                        super::code_graph::indexed(call, &notes, &metrics, &held);
                    }
                    let watch = |stage| tell.stage(stage);
                    let reviewed =
                        run_review_call(agents.as_ref(), reviewer.as_ref(), action, &held, &watch);
                    End::Review(reviewed)
                }
            }));
            let end = ran.unwrap_or_else(End::Panicked);
            if end.reached_no_model() {
                ledger.no_model(Key::Item(ticket));
            }
            tell.send(News::Ended { ticket, end });
        };
        let started = thread::Builder::new()
            .name(format!("#{ticket} {role}"))
            .spawn(run);
        if let Err(e) = started {
            let end = unstarted(harness, &e);
            let _ = self.flights.send.send(News::Ended { ticket, end });
        }
        self.flights.flying.insert(ticket, flight);
        self.board_changed();
    }

    // Records the end of `ticket`'s turn. A session that died before it
    // began starts over once, at once, with the same id: the caller starts it
    // when the runner's lock is let go.
    fn turn_ended(
        &mut self,
        ticket: TicketId,
        result: Result<AgentReply, AgentError>,
    ) -> Result<Ended, StateError> {
        // A host that cannot be reached holds the turn back, to be tried
        // again, and keeps its session. Time spent on the call is saved
        // while its flight still counts it.
        if self.waits_for_host(ticket, &result) {
            let saved = self.save_time();
            self.flights.flying.remove(&ticket);
            saved?;
            return Ok(Ended::Recorded(None));
        }
        let again = self.flights.flying.get(&ticket).is_some_and(|f| f.again);
        if !again && matches!(result, Err(AgentError::NoSession(..))) {
            if let Some((call, began)) =
                (self.flights.watched(ticket)).and_then(|w| Some((w.call.clone()?, w.started)))
            {
                self.resume_lost(&call, began);
            }
            // The dead call's time is the worker's, so it is saved while its
            // flight still counts it, before a drain can outrun the new turn.
            let saved = self.save_time();
            self.flights.flying.remove(&ticket);
            saved?;
            return Ok(Ended::StartOver(ticket));
        }
        let ended = self.on(Some(ticket)).end_turn(result);
        self.flights.flying.remove(&ticket);
        ended.map(Ended::Recorded)
    }

    fn next_news(&self) -> Option<News> {
        self.flights.news.try_recv().ok()
    }
}

// How the end of a worker's turn was recorded
enum Ended {
    // The turn's end is recorded, and may report
    Recorded(Option<StepReport>),
    // The turn's session died before it began, so its work item begins the turn again
    StartOver(TicketId),
}

// The end of a call whose thread could not start: a turn on `harness`, or a
// review call.
fn unstarted(harness: Option<Harness>, e: &std::io::Error) -> End {
    let reason = format!("cannot start a thread for the call: {e}");
    match harness {
        Some(harness) => End::Turn(Err(AgentError::Spawn(harness, reason))),
        None => End::Review(Reviewed::failed(reason)),
    }
}

/// Runs one pass of the runner's loop
///
/// Each open work item is stepped in turn, starting after the one that did
/// something last. A report that waits, such as an unreadable forge, is
/// returned only when nothing else did. A ruling answered while the runner
/// was stopped is acted on first. Rulings and replies go on while it drains.
/// The panic of a call's thread is resumed here.
///
/// # Errors
///
/// [`StateError`] when a call's start or end, or a post, cannot be saved.
pub fn advance(runner: &Mutex<Runner>) -> Result<Pass, StateError> {
    // A pass holds the runner's lock through its forge calls, and each of those
    // waits its turn behind the login's pace. Post what `status` and `drain`
    // read before the pass asks the forge anything, and again once it has
    // started or ended a call.
    lock(runner).post();
    let pass = one_pass(runner);
    if let Ok(Pass::Report(report)) = &pass {
        lock(runner).journal_report(report);
    }
    // After the pass, so a ruling it raised is told in the pass that raised it.
    lock(runner).tell_rulings();
    brief(runner);
    // The project manager wakes only on a pass that started nothing else,
    // once the board it reads is written.
    let pass = match pass {
        // A wake, a retro or a proof that just started is a call in flight: the loop goes on.
        Ok(Pass::Idle) => {
            let woke = wake_off_lock(runner);
            let looked = !woke && lock(runner).retro_launched();
            let proved = lock(runner).proofs_launched();
            Ok(if woke || looked || proved {
                Pass::Started
            } else {
                Pass::Idle
            })
        }
        Ok(Pass::Report(report)) if report.waits() => {
            wake_off_lock(runner);
            Ok(Pass::Report(report))
        }
        pass => pass,
    };
    lock(runner).post();
    pass
}

fn one_pass(runner: &Mutex<Runner>) -> Result<Pass, StateError> {
    // A `drain` or `undrain` that landed while a pass held the lock set its
    // flag at once; what it asked the runner itself is taken up here.
    lock(runner).take_posted();
    // The verbs that landed then are applied, reading the forge with the lock let go.
    super::queued::take_up(runner);
    lock(runner).beat();
    lock(runner).send_metrics();
    super::timings::read_rate_limit(runner);
    lock(runner).let_go_of_the_gone();
    // Before any answer is taken, so none resumes a worker on a closed issue.
    super::closed::read_parked(runner)?;
    // The forge is asked with the lock let go.
    super::thread_replies::read_threads(runner)?;
    super::left::answer_left(runner);
    loop {
        let news = lock(runner).next_news();
        let Some(news) = news else { break };
        if let Some(report) = hear(runner, news)? {
            return Ok(Pass::Report(report));
        }
    }
    lock(runner).end_overdue();
    super::builds::watch(runner);
    let alerts = Arc::clone(&lock(runner).ports.alerts);
    if let Some(posted) = post_due(runner, alerts.as_ref()) {
        return posted.map(Pass::Report);
    }
    if let Some(answered) = answer_replies(runner, alerts.as_ref()) {
        return answered.map(Pass::Report);
    }
    // After the posts, so a merge's notice goes before the word it finished.
    let finished = lock(runner).finished_now()?;
    if let Some(report) = finished {
        return Ok(Pass::Report(report));
    }
    // Before any step is chosen, so no forge read falls between a step's drain check and the
    // call it starts.
    super::main_ci::read_main_ci_off_lock(runner);
    super::dispatch::recheck_dropped(runner)?;
    super::dependencies::follow_dependencies(runner)?;
    // The forge is asked with the lock let go, and the pass takes it for what it
    // read.
    super::triage::poll_read::read_triage(runner)?;
    // After them, so the board's listing is asked after the call that learns a used-up limit.
    super::board_read::read_board(runner);
    rotation::begin_rotation(runner)
}

// Records one piece of news from a call in flight, and tells the project's `ruling`
// hooks the ruling it may have raised.
fn hear(runner: &Mutex<Runner>, news: News) -> Result<Option<StepReport>, StateError> {
    let heard = hear_news(runner, news);
    lock(runner).tell_rulings();
    // A call's end is what `drain` answers with, and the pass goes on to ask
    // the forge with the lock held.
    lock(runner).post();
    heard
}

fn hear_news(shared: &Mutex<Runner>, news: News) -> Result<Option<StepReport>, StateError> {
    let mut runner = lock(shared);
    let (ticket, end) = match news {
        News::Stage { ticket, stage, .. } => {
            if stage == RoundStage::Running {
                let now = runner.ports.clock.now();
                runner.flights.ledger.ran(Key::Item(ticket), now);
                // The round has the lease it queued for.
                runner.began(ticket);
            } else {
                // A round queued at its model's lease waits like a turn does.
                runner.told(ticket, Wait::Lease);
            }
            // A failed save is told and let go. A GPU wait may then count as
            // the round's, but the phases still sum to the wall time.
            if let Err(e) = runner.on(Some(ticket)).round_stage(stage) {
                eprintln!("cannot save the local round's stage: {e}");
            }
            return Ok(None);
        }
        News::Began { ticket, .. } => {
            runner.began(ticket);
            return Ok(None);
        }
        News::Waits { ticket, wait } => {
            runner.told(ticket, wait);
            return Ok(None);
        }
        News::Secured {
            pull_request,
            head,
            reading,
        } => {
            runner.security_heard(pull_request, &head, reading)?;
            return Ok(None);
        }
        News::Triaged { issue, end, found } => {
            runner.triage_heard(issue, end, found.map(|found| *found))?;
            return Ok(None);
        }
        News::Proved(proof) => {
            runner.proof_heard(proof)?;
            return Ok(None);
        }
        News::Retro(end) => {
            runner.retro_heard(end);
            return Ok(None);
        }
        News::Ended { ticket, end } => (ticket, end),
        News::Pm(end) => {
            runner.flights.pm = None;
            let open = runner.flights.ledger.take(Key::Manager);
            return match end {
                End::Turn(result) => {
                    if let Some(open) = &open {
                        runner.plain_line(open, &result);
                    }
                    runner.pm_ended(result)
                }
                End::Review(_) => Ok(None),
                End::Panicked(panic) => {
                    if let Some(open) = &open {
                        runner.panicked_line(open);
                    }
                    drop(runner);
                    resume_unwind(panic)
                }
            };
        }
    };
    let open = runner.flights.ledger.take(Key::Item(ticket));
    // A call's line goes in only once its end is saved: one not saved runs
    // again, and its session's cost by then takes in this call's.
    match end {
        End::Turn(result) if runner.flights.runs_compact(ticket) => {
            let line = open.and_then(|open| runner.turn_line(&open, &result));
            let unreported = result.is_err();
            let ended = runner.on(Some(ticket)).compact_ended(result);
            if let (Some(line), Ok(_)) = (line, &ended) {
                runner.append_call(line, unreported);
            }
            ended
        }
        End::Turn(result) => {
            let line = open.and_then(|open| runner.turn_line(&open, &result));
            let unreported = result.is_err();
            // The turn's end reads git and the forge with the lock let go.
            if result.is_ok()
                && let Some(job) = runner.end_job(ticket)
            {
                drop(runner);
                let read = job.run();
                runner = lock(shared);
                runner.keep_end_read(read);
            }
            let ended = runner.turn_ended(ticket, result);
            if let (Some(line), Ok(_)) = (line, &ended) {
                runner.append_call(line, unreported);
            }
            runner.pm_after_end(ticket)?;
            match ended? {
                Ended::Recorded(report) => Ok(report),
                // Begun with the lock let go, as any step of a pass is.
                Ended::StartOver(ticket) => {
                    drop(runner);
                    rotation::start_over(shared, ticket)
                }
            }
        }
        End::Review(reviewed) => {
            let line = open.and_then(|open| runner.review_line(&open, &reviewed));
            let unreported = !matches!(reviewed.spent, Some(Spent::Claude { .. } | Spent::Local));
            let ended = runner.on(Some(ticket)).end_review(reviewed);
            runner.flights.flying.remove(&ticket);
            if let (Some(line), Ok(_)) = (line, &ended) {
                runner.append_call(line, unreported);
            }
            ended
        }
        End::Panicked(panic) => {
            runner.flights.flying.remove(&ticket);
            if let Some(open) = &open {
                runner.panicked_line(open);
            }
            drop(runner);
            resume_unwind(panic)
        }
    }
}

mod calls;
mod carry;
mod news;
mod proving;
mod rotation;

use news::{End, News, Tell};

pub(in crate::runner) use proving::Verdict;
pub(super) mod retro;
pub(super) mod security;
#[cfg(any(test, feature = "test-support"))]
mod step;
#[cfg(test)]
pub(crate) mod tests;
pub(super) mod triage;
mod waiting;

pub use carry::{CARRY_WAIT, CarriedTurn, NotCarried};
#[cfg(any(test, feature = "test-support"))]
pub use step::step;
pub(super) use waiting::Waiting;
