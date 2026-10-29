//! `lease run cargo-test`: one command run under the counted lease
//!
//! The command's process tree inherits the connection, so the lease stays
//! held until the last of it exits, however this process ends. When the
//! command ends, this process shuts the connection for every holder. A dog
//! that is back is asked again: a waiting run afresh, a running one to be counted.
//!
//! A command that runs past its bound, counted from the grant and not from
//! the wait in the queue, is ended with everything it started, so one hung
//! test binary does not hold a place that others queue for.

use std::path::Path;
use std::time::Duration;

use tokio::time::Instant;

use super::{exit_number, listen};
use crate::interrupt::{Caught, Interrupts};
use crate::lease::counted::CARGO_TEST;
use crate::lease::door::{Answer, Ask, SOCKET_VAR, Visit};
use crate::tree::{Lead, Reach, Signal, Tree};

/// What a run exits with when it could not have the lease, as `EX_TEMPFAIL`
pub const NO_LEASE: u8 = 75;

/// What a run exits with when its command could not start, as a shell does
pub const NOT_RUN: u8 = 127;

/// What a run exits with when its command ran past its bound and was ended, as GNU `timeout` does
pub const TIMED_OUT: u8 = 124;

/// How long a command holds its place before the run ends it: a full suite
/// takes minutes, and the hang this bounds held a place for 28
pub const RUN_LIMIT: Duration = Duration::from_secs(20 * 60);

/// How long a command has to end after SIGTERM before it is killed
const KILL_AFTER: Duration = Duration::from_secs(5);

/// How often a run whose dog went away knocks again; a restart takes seconds
pub const REJOIN_EVERY: Duration = Duration::from_secs(1);

/// How many knocks before a run stops asking again, waiting or running
pub const REJOIN_TRIES: u32 = 60;

/// How long a dog has to answer an ask; a live one answers at once
const ANSWER_WITHIN: Duration = Duration::from_secs(5);

/// How long a run keeps asking for a dog that is gone
#[derive(Debug, Clone, Copy)]
pub struct Patience {
    /// How many knocks before it stops
    pub tries: u32,
    /// How long between knocks
    pub every: Duration,
}

/// What a run has: [`REJOIN_TRIES`] knocks, one every [`REJOIN_EVERY`]
pub const PATIENCE: Patience = Patience {
    tries: REJOIN_TRIES,
    every: REJOIN_EVERY,
};

/// Runs `command` once the door at `socket` grants `cargo-test`, and
/// returns the number to exit with: the command's own, or [`NO_LEASE`],
/// [`NOT_RUN`], [`TIMED_OUT`] if it ran past `limit` from the grant, or a
/// caught signal's
pub async fn run(socket: &Path, command: &[&str], limit: Duration) -> u8 {
    run_with_patience(socket, command, limit, PATIENCE).await
}

/// [`run`], knocking as `patience` says when the dog is gone
pub async fn run_with_patience(
    socket: &Path,
    command: &[&str],
    limit: Duration,
    patience: Patience,
) -> u8 {
    let Some((program, args)) = command.split_first() else {
        return NOT_RUN;
    };
    let mut signals = match listen() {
        Ok(signals) => signals,
        Err(e) => return failed(&e, NO_LEASE),
    };
    let ask = Ask::new(CARGO_TEST, std::process::id(), command.join(" "));
    let visit = match wait_for_turn(socket, &ask, &mut signals, patience).await {
        Ok(Ok(visit)) => visit,
        Ok(Err(caught)) => return caught.number(),
        Err(e) => return failed(&unreached(&e), NO_LEASE),
    };
    let granted = Instant::now();
    // A signal before the start reached nobody else, so it ends the run.
    if let Some(caught) = signals.already().await {
        visit.give_back().await;
        return caught.number();
    }
    if let Err(e) = visit.inheritable(true) {
        say(&format!(
            "{e}: the lease is held by this process alone, and ends if it is killed while the command runs"
        ));
    }
    let spawned = tokio::process::Command::from(crate::spawn::command(program))
        .args(args)
        .lead()
        .spawn();
    let _ = visit.inheritable(false);
    let child = match spawned {
        Ok(child) => child,
        Err(e) => {
            visit.give_back().await;
            return failed(&format!("cannot run {program}: {e}"), NOT_RUN);
        }
    };
    let rejoin = Rejoin {
        socket,
        ask: Ask {
            running: true,
            ..ask
        },
        patience,
    };
    tend(child, Some(visit), &rejoin, &mut signals, (granted, limit)).await
}

// What a run needs to be counted again when its dog went away.
struct Rejoin<'a> {
    socket: &'a Path,
    ask: Ask,
    patience: Patience,
}

// Waits for `child`, which leads its own tree, until it ends or `limit` has
// passed since `granted`, and returns the number to exit with. A signal to the
// run is passed on to the whole tree, and so is an interrupt, which the
// terminal sends to the run's group and not to the command's.
async fn tend(
    mut child: tokio::process::Child,
    visit: Option<Visit>,
    rejoin: &Rejoin<'_>,
    signals: &mut Interrupts,
    (granted, limit): (Instant, Duration),
) -> u8 {
    let tree = Tree::of_tokio(&child).ok();
    let deadline = granted + limit;
    let patience = rejoin.patience;
    let mut held = visit;
    let mut tries = 0;
    loop {
        let rejoining = held.is_none() && tries < patience.tries;
        let event = tokio::select! {
            status = child.wait() => Event::Ended(status),
            caught = signals.recv() => Event::Caught(caught),
            () = closed(held.as_mut()) => Event::DogGone,
            () = tokio::time::sleep(patience.every), if rejoining => Event::Rejoin,
            () = tokio::time::sleep_until(deadline) => Event::Overran,
        };
        match event {
            Event::Ended(status) => {
                if let Some(visit) = held {
                    visit.give_back().await;
                }
                return status.map_or_else(|e| failed(&e.to_string(), 1), exit_number);
            }
            Event::Caught(caught) => {
                if let Some(tree) = &tree
                    && tree.send(caught.signal()) == Reach::Partial
                {
                    say(
                        "cannot signal the command's process tree: passed the signal to the command alone",
                    );
                }
            }
            Event::DogGone => {
                held = None;
                tries = 0;
                say("the dog went away: asking it to count this run again once it is back");
            }
            Event::Rejoin => {
                tries += 1;
                match seat(rejoin.socket, &rejoin.ask).await {
                    Ok(visit) => {
                        held = Some(visit);
                        say("the dog counts this run again");
                    }
                    Err(_) if tries == patience.tries => {
                        say("the dog did not come back, so this run is not counted");
                    }
                    Err(_) => {}
                }
            }
            Event::Overran => {
                end(&mut child, tree.as_ref()).await;
                if let Some(visit) = held {
                    visit.give_back().await;
                }
                say(&format!(
                    "the command ran past its bound of {:?} and was ended, exiting {TIMED_OUT}: its output above shows where it was",
                    limit
                ));
                return TIMED_OUT;
            }
        }
    }
}

enum Event {
    Ended(std::io::Result<std::process::ExitStatus>),
    Caught(Caught),
    DogGone,
    Rejoin,
    Overran,
}

// Ends the child's tree: a request to end, then the kill after `KILL_AFTER`
// whether or not the child has gone, since what it started may hold on.
async fn end(child: &mut tokio::process::Child, tree: Option<&Tree>) {
    let Some(tree) = tree else {
        let _ = child.kill().await;
        return;
    };
    let mut alone = false;
    for signal in [Signal::Terminate, Signal::Kill] {
        if tree.send(signal) == Reach::Partial && !alone {
            say("cannot signal the command's process tree: ending the command alone");
            alone = true;
        }
        if signal == Signal::Terminate {
            // The leader going does not spare what it started: the kill follows.
            let _ = tokio::time::timeout(KILL_AFTER, child.wait()).await;
        }
    }
    let _ = child.wait().await;
}

// Knocks and waits for the grant, knocking again for a while if the dog
// is gone or goes away first. A signal while waiting closes the connection,
// which withdraws the ask, and is handed back.
async fn wait_for_turn(
    socket: &Path,
    ask: &Ask,
    signals: &mut Interrupts,
    patience: Patience,
) -> Result<Result<Visit, Caught>, String> {
    let mut tries = 0;
    loop {
        let lost = match one_turn(socket, ask, signals).await {
            Turn::Granted(visit) => return Ok(Ok(visit)),
            Turn::Caught(caught) => return Ok(Err(caught)),
            Turn::Refused(why) => return Err(why),
            Turn::Lost { why, answered } => {
                // A dog that answered was there; the next one is a fresh wait.
                // A run queued behind hours of work may outlast many restarts,
                // so only a dog that never answers wears the tries down.
                if answered {
                    tries = 0;
                }
                why
            }
        };
        tries += 1;
        if tries > patience.tries {
            return Err(lost);
        }
        if tries == 1 {
            say(&asking_again(&lost, patience));
        }
        tokio::select! {
            () = tokio::time::sleep(patience.every) => {}
            caught = signals.recv() => return Ok(Err(caught)),
        }
    }
}

// What the first failed knock says, so a worker reading it knows how long
// the run holds out before it exits without the lease.
fn asking_again(lost: &str, patience: Patience) -> String {
    format!(
        "{lost}: asking again every {:?}, up to {} times while the dog comes back, then exiting {NO_LEASE}",
        patience.every, patience.tries
    )
}

enum Turn {
    Granted(Visit),
    Caught(Caught),
    // The dog said no, which asking again would not change.
    Refused(String),
    // No dog, or it went away before granting, after answering or not.
    Lost { why: String, answered: bool },
}

async fn one_turn(socket: &Path, ask: &Ask, signals: &mut Interrupts) -> Turn {
    let mut visit = match Visit::knock(socket, ask).await {
        Ok(visit) => visit,
        Err(why) => {
            return Turn::Lost {
                why,
                answered: false,
            };
        }
    };
    let mut first = true;
    let mut answered = false;
    loop {
        // The first answer comes at once from a live dog; a queue's grant can take hours.
        let next = async {
            if !first {
                return visit.answer().await;
            }
            tokio::time::timeout(ANSWER_WITHIN, visit.answer())
                .await
                .map_err(|_| "the dog took the ask and never answered".to_owned())?
        };
        let answer = tokio::select! {
            answer = next => answer,
            caught = signals.recv() => return Turn::Caught(caught),
        };
        first = false;
        match answer {
            Ok(Some(Answer::Granted)) => return Turn::Granted(visit),
            Ok(Some(Answer::Queued(ahead))) => {
                answered = true;
                say(&format!("waiting for {CARGO_TEST}, {ahead} ahead"));
            }
            Ok(Some(Answer::Error(why))) => {
                return Turn::Refused(format!("the dog refused: {why}"));
            }
            // Answers to an e2e, forge or pick ask; a dog never sends them to this one.
            Ok(Some(
                other @ (Answer::Output(_)
                | Answer::Done(_)
                | Answer::Go
                | Answer::Wait(_)
                | Answer::Held(_)
                | Answer::Picks(_)
                | Answer::Fits(_)),
            )) => {
                return Turn::Refused(format!("the dog answered {other:?} to {CARGO_TEST}"));
            }
            Ok(None) => {
                return Turn::Lost {
                    why: format!("the dog closed the door before granting {CARGO_TEST}"),
                    answered,
                };
            }
            Err(why) => return Turn::Lost { why, answered },
        }
    }
}

// A dog asked to count a running command again seats it at once.
async fn seat(socket: &Path, rejoin: &Ask) -> Result<Visit, String> {
    let mut visit = Visit::knock(socket, rejoin).await?;
    match tokio::time::timeout(ANSWER_WITHIN, visit.answer()).await {
        Ok(Ok(Some(Answer::Granted))) => Ok(visit),
        other => Err(format!("the dog answered {other:?}")),
    }
}

// Ends when the dog closes the connection; never without one.
async fn closed(held: Option<&mut Visit>) {
    let Some(visit) = held else {
        return std::future::pending().await;
    };
    while let Ok(Some(_)) = visit.answer().await {}
}

// A worker cannot start the dog, so it is told to go on without the lease.
fn unreached(why: &str) -> String {
    if std::env::var_os(SOCKET_VAR).is_some() {
        format!("{why}: run the tests without the lease, and say so in your final message")
    } else {
        format!("{why}: the adopted kelpie may be down, and `shep enable kelpie` runs it")
    }
}

fn failed(message: &str, code: u8) -> u8 {
    say(message);
    code
}

fn say(message: &str) {
    eprintln!("shep kelpie lease: {message}");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::tree::alive;

    // A command in its own tree, as `run` starts it.
    #[cfg(unix)]
    fn start(script: &str) -> tokio::process::Child {
        tokio::process::Command::new(crate::spawn::sh())
            .args(["-c", script])
            .lead()
            .spawn()
            .unwrap()
    }

    // Tends `child` with no dog and a bound `limit` from now.
    #[cfg(unix)]
    async fn tended(child: tokio::process::Child, limit: Duration) -> u8 {
        // A signal another test sends this process reaches every run alive.
        let _turn = super::super::tests::LISTENING.lock().await;
        let mut signals = listen().unwrap();
        let rejoin = Rejoin {
            socket: Path::new("no.sock"),
            ask: Ask::new(CARGO_TEST, std::process::id(), "test"),
            patience: Patience {
                tries: 0,
                every: Duration::from_millis(10),
            },
        };
        tend(child, None, &rejoin, &mut signals, (Instant::now(), limit)).await
    }

    #[test]
    fn the_first_failed_knock_says_how_long_it_will_ask() {
        assert_eq!(
            asking_again("cannot reach the dog", PATIENCE),
            "cannot reach the dog: asking again every 1s, up to 60 times while the dog comes back, then exiting 75"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_command_past_its_bound_is_ended_with_its_children_and_exits_124() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("sleep.pid");
        let script = format!("sleep 300 & echo $! > {}; wait", pidfile.display());
        let started = std::time::Instant::now();
        let code = tended(start(&script), Duration::from_millis(300)).await;
        assert_eq!(code, TIMED_OUT);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "it waited for the sleep"
        );
        let sleeper: u32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        for _ in 0..100 {
            if !alive(sleeper) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the command's child outlived the run");
    }

    // The run's own signal arrives as a group's, not the child's alone.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_signal_the_run_passes_on_reaches_what_the_command_started() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("sleep.pid");
        let script = format!("sleep 300 & echo $! > {}; wait", pidfile.display());
        let mut child = start(&script);
        let sleeper = loop {
            match std::fs::read_to_string(&pidfile).map(|text| text.trim().parse::<u32>()) {
                Ok(Ok(pid)) => break pid,
                _ => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        };
        assert!(alive(sleeper));
        Tree::of_tokio(&child).unwrap().send(Signal::Terminate);
        child.wait().await.unwrap();
        for _ in 0..100 {
            if !alive(sleeper) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the command's child outlived the signal");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_command_that_ends_in_time_keeps_its_exit_code() {
        assert_eq!(tended(start("exit 3"), Duration::from_secs(30)).await, 3);
        assert_eq!(tended(start("true"), Duration::from_secs(30)).await, 0);
    }
}
