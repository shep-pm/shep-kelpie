//! Calls in flight
//!
//! The runner starts each agent call on a thread of its own and goes on, so
//! no thread of the runner's waits on one. The call's end comes back on a
//! channel, which wakes the runner's loop, and its next pass records it. A
//! work item has at most one call in flight, and the open work items' calls
//! run at once, beside the project manager's one call. A turn's ceiling is
//! a deadline each pass checks, and a call past it is ended through its own
//! process group.

use std::any::Any;
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::Runner;
use super::alert::post_due;
use super::briefing::{Watched, brief};
use super::drain::{CallRole, CallRunning};
use super::in_flight::InFlight;
use super::replies::answer_replies;
use super::report::{Begin, ReviewCall, ReviewResult, Reviewed, Spent, StepReport};
use super::retro;
use super::review::run_review_call;
use super::trigger::lock;
use crate::ports::{AgentCall, AgentError, AgentReply, Ending, RoundStage, Timestamp, Wait};
use crate::settings::Harness;
use crate::state::StateError;
use crate::usage::{CallKind, Draft, Line};

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
    flying: BTreeMap<u64, Flight>,
    // The project manager's call, its ceiling and its hold
    pm: Option<(Timestamp, Ending)>,
    // The issue writer's call labelling an issue
    labelling: Option<label::Labelling>,
    // Every call's line in the usage ledger, as it started
    ledger: InFlight,
    send: Sender<News>,
    news: Receiver<News>,
    // Told after each piece of news, so the loop waiting on it wakes
    wake: Option<Sender<()>>,
}

impl Default for Flights {
    fn default() -> Self {
        let (send, news) = mpsc::channel();
        Self {
            flying: BTreeMap::new(),
            pm: None,
            labelling: None,
            ledger: InFlight::default(),
            send,
            news,
            wake: None,
        }
    }
}

impl Flights {
    /// Whether no call is in flight, the project manager's included
    #[cfg(test)]
    fn idle(&self) -> bool {
        self.flying.is_empty() && self.pm.is_none() && self.labelling.is_none()
    }

    /// The issue the issue writer is labelling, while it is
    pub(super) fn labelling(&self) -> Option<u64> {
        self.labelling.as_ref().map(|l| l.issue)
    }

    /// Whether `issue`'s work item has a call in flight
    pub(super) fn flying(&self, issue: u64) -> bool {
        self.flying.contains_key(&issue)
    }

    /// Whether `issue`'s work item has a worker's turn in flight
    ///
    /// A turn marked running in the state file is not enough to call its
    /// time the worker's: a stop leaves one marked, and so does a restart
    /// until a pass resumes it.
    pub(super) fn runs_turn(&self, issue: u64) -> bool {
        self.flying
            .get(&issue)
            .is_some_and(|f| f.deadline.is_some() && !f.retro)
    }

    /// Forgets `issue`'s call in flight, once its end is heard
    pub(super) fn landed(&mut self, issue: u64) {
        self.flying.remove(&issue);
    }

    /// Whether `issue`'s call in flight is its work item's retro
    pub(super) fn runs_retro(&self, issue: u64) -> bool {
        self.flying.get(&issue).is_some_and(|f| f.retro)
    }

    /// Ends `issue`'s call in flight, as its ceiling would
    pub(super) fn end(&self, issue: u64) {
        if let Some(flight) = self.flying.get(&issue) {
            flight.ending.end();
        }
    }

    /// Every call in flight: each work item's by its issue, then the
    /// project manager's
    pub(super) fn running(&self) -> Vec<CallRunning> {
        let items = self.flying.iter().map(|(&issue, flight)| CallRunning {
            issue: Some(issue),
            role: match flight.deadline {
                Some(_) => CallRole::Worker,
                None => CallRole::Reviewer,
            },
        });
        let pm = self.pm.iter().map(|_| CallRunning {
            issue: None,
            role: CallRole::Pm,
        });
        let labelling = self.labelling.iter().map(|l| CallRunning {
            issue: Some(l.issue),
            role: CallRole::IssueWriter,
        });
        items.chain(pm).chain(labelling).collect()
    }

    /// The calls in flight as the usage ledger knows them
    pub(super) fn in_flight(&self) -> InFlight {
        self.ledger.clone()
    }

    /// `issue`'s call in flight, as the board watches it
    pub(super) fn watched(&self, issue: u64) -> Option<&Watched> {
        self.flying.get(&issue).map(|f| &f.watched)
    }

    /// Every call in flight by its work item's issue, as the board watches it
    pub(super) fn watched_mut(&mut self) -> impl Iterator<Item = (u64, &mut Watched)> {
        self.flying
            .iter_mut()
            .map(|(&issue, f)| (issue, &mut f.watched))
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
    // Whether it is a finished work item's retro, not a turn of its worker
    retro: bool,
    // When the call began, past the lease it waited for
    began_at: Option<Timestamp>,
    // What the call last said it waits for, and since when
    told: Option<(Timestamp, Wait)>,
    // Where a first turn that waited too long moves
    fallback: waiting::Fallback,
}

// What a call's thread sends the runner
enum News {
    Ended {
        issue: u64,
        end: End,
    },
    // The project manager's call ended
    Pm(End),
    // The issue writer's call labelling an issue ended
    Labelled {
        issue: u64,
        end: End,
    },
    // A local round queued for the GPU or began to run. The call waits until
    // `_seen` drops, once the stage is saved, so its time is charged right.
    Stage {
        issue: u64,
        stage: RoundStage,
        _seen: Sender<()>,
    },
    // The call has the lease it waited for, and begins now. It waits until
    // `_seen` drops, once its ceiling counts from now.
    Began {
        issue: u64,
        _seen: Sender<()>,
    },
    // The call waits for its model
    Waits {
        issue: u64,
        wait: Wait,
    },
}

// How a call ended
enum End {
    Turn(Result<AgentReply, AgentError>),
    Review(Reviewed),
    // The call's thread panicked, which ends the runner as a panic in its
    // own thread does
    Panicked(Box<dyn Any + Send>),
}

impl End {
    // Whether it ended before reaching a model: its settings, its harness or
    // its session never got that far, or a local model sat on the CPU
    fn reached_no_model(&self) -> bool {
        match self {
            Self::Turn(result) => crate::usage::ended(result).is_none(),
            Self::Review(reviewed) => matches!(
                (&reviewed.result, &reviewed.spent),
                (ReviewResult::Spilled(_), _) | (ReviewResult::Findings(Err(_)), None)
            ),
            Self::Panicked(_) => false,
        }
    }
}

// What a call's thread needs to tell the runner about its call
#[derive(Clone)]
struct Tell {
    issue: u64,
    news: Sender<News>,
    wake: Option<Sender<()>>,
}

impl Tell {
    fn send(&self, news: News) -> bool {
        let sent = self.news.send(news).is_ok();
        if let Some(wake) = &self.wake {
            let _ = wake.send(());
        }
        sent
    }

    fn stage(&self, stage: RoundStage) {
        let issue = self.issue;
        self.until_heard(|seen| News::Stage {
            issue,
            stage,
            _seen: seen,
        });
    }

    fn began(&self) {
        let issue = self.issue;
        self.until_heard(|seen| News::Began { issue, _seen: seen });
    }

    fn waits(&self, wait: Wait) {
        let issue = self.issue;
        self.send(News::Waits { issue, wait });
    }

    // Sends the news `news` makes, and waits until the runner has heard it.
    fn until_heard(&self, news: impl FnOnce(Sender<()>) -> News) {
        let (seen, heard) = mpsc::channel();
        if self.send(news(seen)) {
            let _ = heard.recv();
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
        retro: bool,
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
            .filter_map(|flight| flight.deadline)
            .chain(self.fallback_due());
        let pm = (self.flights.pm.iter())
            .filter(|(_, ending)| !ending.asked())
            .map(|(deadline, _)| *deadline);
        let labelling = (self.flights.labelling.iter()).filter_map(label::Labelling::ceiling);
        turns
            .chain(pm)
            .chain(labelling)
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
        if let Some(labelling) = &self.flights.labelling {
            labelling.end_overdue(now);
        }
    }

    /// Starts the project manager's `call`, a `kind` on the agent named
    /// `agent`, on a thread of its own, ended past `ceiling` seconds, whose
    /// end comes back as news
    pub(super) fn launch_pm(&mut self, call: AgentCall, ceiling: u64, agent: &str, kind: CallKind) {
        let now = self.ports.clock.now();
        let draft = Draft::of(&call, agent, kind, now);
        self.flights.ledger.open(None, draft, self.pacer_lines());
        let ledger = self.flights.ledger.clone();
        let (news, wake) = (self.flights.send.clone(), self.flights.wake.clone());
        let ending = Ending::default();
        let held = ending.clone();
        let agents = Arc::clone(&self.ports.agents);
        let run = move || {
            let ran = catch_unwind(AssertUnwindSafe(|| End::Turn(agents.run(&call, &held))));
            let end = ran.unwrap_or_else(End::Panicked);
            if end.reached_no_model() {
                ledger.no_model(None);
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

    // Counts `issue`'s turn ceiling from now, once its call has the lease it
    // waited for.
    fn began(&mut self, issue: u64) {
        let now = self.ports.clock.now();
        if let Some(flight) = self.flights.flying.get_mut(&issue)
            && flight.deadline.is_some()
        {
            flight.deadline = Some(Timestamp(now.0.saturating_add(flight.left)));
            flight.began_at = Some(now);
            flight.told = None;
        }
        self.board_changed();
    }

    // Begins what is due next, starting the call it names. `start_over` is
    // the work item whose session died unborn, which begins its turn again.
    fn begin(&mut self, start_over: Option<u64>) -> Result<Pass, StateError> {
        let begin = self.begin_turn(start_over)?;
        let launch = match begin {
            Begin::Idle => return Ok(Pass::Idle),
            Begin::Report(report) => return Ok(Pass::Report(report)),
            Begin::Call {
                call,
                deadline,
                first,
            } => Launch::Turn {
                call,
                deadline,
                again: start_over.is_some(),
                first,
                retro: false,
            },
            Begin::Retro(call) => {
                let now = self.ports.clock.now();
                Launch::Turn {
                    call,
                    deadline: Timestamp(now.0.saturating_add(retro::CEILING)),
                    again: false,
                    first: false,
                    retro: true,
                }
            }
            Begin::Review(action) => Launch::Review(action),
            Begin::Label(issue) => {
                let report = self.ask_writer(&issue)?;
                return Ok(report.map_or(Pass::Started, Pass::Report));
            }
        };
        let issue = self.focus.expect("a call is a work item's");
        self.launch(issue, launch);
        Ok(Pass::Started)
    }

    // Starts `launch` on a thread of its own, whose end comes back as news.
    fn launch(&mut self, issue: u64, launch: Launch) {
        let tell = Tell {
            issue,
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
        let retro = matches!(launch, Launch::Turn { retro: true, .. });
        let role = harness.map_or("reviewer", |_| "worker");
        let now = self.ports.clock.now();
        let watched = Watched {
            started: now,
            call: match &launch {
                Launch::Turn { call, .. } => Some(call.clone()),
                Launch::Review(_) => None,
            },
            idle: false,
        };
        let draft = match &launch {
            Launch::Turn { call, .. } => {
                let kind = if retro {
                    CallKind::Retro
                } else {
                    CallKind::Turn
                };
                Draft::of(call, &self.ledger_worker(issue), kind, now)
            }
            Launch::Review(ReviewCall::Session(call)) => {
                Draft::of(call, &self.ledger_reviewer(issue), CallKind::Review, now)
            }
            Launch::Review(ReviewCall::Local { local, .. }) => {
                Draft::local(local, issue, &self.ledger_reviewer(issue), now)
            }
        };
        let draft = self.with_pull_request(draft);
        self.flights
            .ledger
            .open(Some(issue), draft, self.pacer_lines());
        let ledger = self.flights.ledger.clone();
        let (agents, reviewer) = (
            Arc::clone(&self.ports.agents),
            Arc::clone(&self.ports.reviewer),
        );
        let held = ending.clone();
        let run = move || {
            let ran = catch_unwind(AssertUnwindSafe(|| match launch {
                Launch::Turn { call, .. } => End::Turn(agents.run(&call, &held)),
                Launch::Review(action) => {
                    let watch = |stage| tell.stage(stage);
                    let reviewed =
                        run_review_call(agents.as_ref(), reviewer.as_ref(), action, &held, &watch);
                    End::Review(reviewed)
                }
            }));
            let end = ran.unwrap_or_else(End::Panicked);
            if end.reached_no_model() {
                ledger.no_model(Some(issue));
            }
            tell.send(News::Ended { issue, end });
        };
        let started = thread::Builder::new()
            .name(format!("#{issue} {role}"))
            .spawn(run);
        if let Err(e) = started {
            let end = unstarted(harness, &e);
            let _ = self.flights.send.send(News::Ended { issue, end });
        }
        let left = deadline.map_or(0, |deadline| deadline.0.saturating_sub(now.0));
        let flight = Flight {
            deadline,
            left,
            ending,
            again,
            watched,
            first,
            retro,
            began_at: None,
            told: None,
            fallback: waiting::Fallback::Stays,
        };
        self.flights.flying.insert(issue, flight);
        self.board_changed();
    }

    // Records the end of `issue`'s turn. A session that died before it
    // began starts over once, at once, with the same id.
    fn turn_ended(
        &mut self,
        issue: u64,
        result: Result<AgentReply, AgentError>,
    ) -> Result<Option<StepReport>, StateError> {
        let again = self.flights.flying.get(&issue).is_some_and(|f| f.again);
        if !again && matches!(result, Err(AgentError::NoSession(..))) {
            // The dead call's time is the worker's, so it is saved while its
            // flight still counts it, before a drain can outrun the new turn.
            let saved = self.save_time();
            self.flights.flying.remove(&issue);
            saved?;
            return match self.begin(Some(issue))? {
                Pass::Report(report) => Ok(Some(report)),
                Pass::Idle | Pass::Started => Ok(None),
            };
        }
        if let Some(moved) = self.fell_back(issue, &result) {
            self.flights.flying.remove(&issue);
            return moved;
        }
        let ended = self.on(Some(issue)).end_turn(result);
        self.flights.flying.remove(&issue);
        ended
    }

    fn next_news(&self) -> Option<News> {
        self.flights.news.try_recv().ok()
    }
}

// The end of a call whose thread could not start: a turn on `harness`, or a
// review call.
fn unstarted(harness: Option<Harness>, e: &std::io::Error) -> End {
    let reason = format!("cannot start a thread for the call: {e}");
    match harness {
        Some(harness) => End::Turn(Err(AgentError::Spawn(harness, reason))),
        None => End::Review(Reviewed {
            result: ReviewResult::Findings(Err(reason)),
            spent: None,
        }),
    }
}

/// Runs one pass of the runner's loop
///
/// Records the end of a call in flight, ends any call past its turn's
/// ceiling, posts a ruling or a notice, handles a reply on the webhook's
/// topic, or begins what is due next for a work item with no call in
/// flight, starting its call. Each open work item is stepped in turn,
/// starting after the one that did something last, and one with nothing to
/// do yields to the next. A report that waits, such as a forge that cannot
/// be read, is returned only when no other work item did anything. A
/// ruling answered while the runner was stopped is acted on first. A ruling
/// is posted, and a reply handled, while the runner drains too. A runner
/// finishing with no work item left open reports that it finished.
///
/// # Errors
///
/// [`StateError`] when a call's start or end, or a post, cannot be saved.
///
/// # Panics
///
/// Resumes the panic of a call's thread, as one in the runner's own would be.
#[track_caller]
pub fn advance(runner: &Mutex<Runner>) -> Result<Pass, StateError> {
    let pass = one_pass(runner);
    brief(runner);
    // The project manager wakes only on a pass that started nothing else,
    // once the board it reads is written.
    match pass {
        Ok(Pass::Idle) if lock(runner).wake_pm() => Ok(Pass::Started),
        Ok(Pass::Report(report)) if report.waits() => {
            lock(runner).wake_pm();
            Ok(Pass::Report(report))
        }
        pass => pass,
    }
}

#[track_caller]
fn one_pass(runner: &Mutex<Runner>) -> Result<Pass, StateError> {
    lock(runner).beat();
    lock(runner).let_go_of_the_gone();
    // Before any answer is taken, so none resumes a worker on a closed issue.
    lock(runner).read_parked()?;
    super::left::answer_left(runner);
    loop {
        let news = lock(runner).next_news();
        let Some(news) = news else { break };
        if let Some(report) = hear(runner, news)? {
            return Ok(Pass::Report(report));
        }
    }
    lock(runner).end_overdue();
    lock(runner).fall_back();
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
    lock(runner).begin(None)
}

// Records one piece of news from a call in flight.
fn hear(runner: &Mutex<Runner>, news: News) -> Result<Option<StepReport>, StateError> {
    let mut runner = lock(runner);
    let (issue, end) = match news {
        News::Stage { issue, stage, .. } => {
            if stage == RoundStage::Running {
                let now = runner.ports.clock.now();
                runner.flights.ledger.ran(Some(issue), now);
            }
            // A failed save is told and let go. A GPU wait may then count as
            // the round's, but the phases still sum to the wall time.
            if let Err(e) = runner.on(Some(issue)).round_stage(stage) {
                eprintln!("cannot save the local round's stage: {e}");
            }
            return Ok(None);
        }
        News::Began { issue, .. } => {
            runner.began(issue);
            return Ok(None);
        }
        News::Waits { issue, wait } => {
            runner.told(issue, wait);
            return Ok(None);
        }
        News::Labelled { issue, end } => return runner.label_ended(issue, end),
        News::Ended { issue, end } => (issue, end),
        News::Pm(end) => {
            runner.flights.pm = None;
            let open = runner.flights.ledger.take(None);
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
    let open = runner.flights.ledger.take(Some(issue));
    // A call's line goes in only once its end is saved: one not saved runs
    // again, and its session's cost by then takes in this call's.
    match end {
        End::Turn(result) if runner.flights.runs_retro(issue) => {
            let line = open.and_then(|open| runner.turn_line(&open, &result));
            runner.retro_ended(issue, result);
            if let Some(line) = line {
                runner.ledger.append(&Line::Call(line));
            }
            Ok(None)
        }
        // A panicking retro is skipped as a failed one is: it must not end the runner.
        End::Panicked(_) if runner.flights.runs_retro(issue) => {
            if let Some(open) = &open {
                runner.panicked_line(open);
            }
            runner.retro_ended(issue, Err(AgentError::Setup("its call panicked".into())));
            Ok(None)
        }
        End::Turn(result) => {
            let line = open.and_then(|open| runner.turn_line(&open, &result));
            let unreported = result.is_err();
            let ended = runner.turn_ended(issue, result);
            if let (Some(line), Ok(_)) = (line, &ended) {
                runner.append_call(line, unreported);
            }
            runner.pm_after_end(issue)?;
            ended
        }
        End::Review(reviewed) => {
            let line = open.and_then(|open| runner.review_line(&open, &reviewed));
            let unreported = !matches!(reviewed.spent, Some(Spent::Claude { .. } | Spent::Local));
            let ended = runner.on(Some(issue)).end_review(reviewed);
            runner.flights.flying.remove(&issue);
            if let (Some(line), Ok(_)) = (line, &ended) {
                runner.append_call(line, unreported);
            }
            ended
        }
        End::Panicked(panic) => {
            runner.flights.flying.remove(&issue);
            if let Some(open) = &open {
                runner.panicked_line(open);
            }
            drop(runner);
            resume_unwind(panic)
        }
    }
}

/// Runs one pass, and waits for any call it starts to end and be recorded
///
/// What a test steps with: a pass that starts a call returns what its end
/// reports, as the loop would on the wake that brings it.
///
/// # Errors
///
/// [`StateError`] as [`advance`].
#[cfg(test)]
pub fn step(runner: &Mutex<Runner>) -> Result<Option<StepReport>, StateError> {
    let stepped = step_once(runner);
    brief(runner);
    stepped
}

#[cfg(test)]
fn step_once(runner: &Mutex<Runner>) -> Result<Option<StepReport>, StateError> {
    // A call's end that never comes fails the test that waited for it.
    const PATIENCE: Duration = Duration::from_secs(120);
    let (wake, woken) = mpsc::channel();
    lock(runner).wake_with(wake);
    match advance(runner)? {
        Pass::Idle => return Ok(None),
        Pass::Report(report) => return Ok(Some(report)),
        Pass::Started => {}
    }
    loop {
        let news = lock(runner).next_news();
        match news {
            Some(news) => {
                if let Some(report) = hear(runner, news)? {
                    return Ok(Some(report));
                }
            }
            None if lock(runner).flights.idle() => return Ok(None),
            None => woken
                .recv_timeout(PATIENCE)
                .expect("a call in flight never ended"),
        }
    }
}

pub(super) mod label;
#[cfg(test)]
mod tests;
mod waiting;

pub(super) use waiting::Waiting;
