//! `lease run cargo-test`: one command run under the counted lease
//!
//! The command's process tree inherits the connection, so the lease stays
//! held until the last of it exits, however this process ends. When the
//! command ends, this process shuts the connection for every holder. A dog
//! that goes away is asked again once it is back: a waiting run asks
//! afresh, and a running one asks to be counted. A connection made on such
//! a rejoin is this process's alone, since the command already runs.

use std::path::Path;
use std::time::Duration;

use super::{Caught, Signals, exit_number, signal_process};
use crate::lease::counted::CARGO_TEST;
use crate::lease::door::{Answer, Ask, SOCKET_VAR, Visit};

/// What a run exits with when it could not have the lease, as `EX_TEMPFAIL`
pub(crate) const NO_LEASE: u8 = 75;

/// What a run exits with when its command could not start, as a shell does
pub(crate) const NOT_RUN: u8 = 127;

/// How often a run whose dog went away knocks again; a restart takes seconds
const REJOIN_EVERY: Duration = Duration::from_secs(1);

/// How many knocks before a run stops asking again, waiting or running
const REJOIN_TRIES: u32 = 60;

/// How long a dog has to answer an ask; a live one answers at once
const ANSWER_WITHIN: Duration = Duration::from_secs(5);

/// Runs `command` once the door at `socket` grants `cargo-test`, and
/// returns the number to exit with: the command's own, or [`NO_LEASE`],
/// [`NOT_RUN`], or a caught signal's
pub(crate) async fn run(socket: &Path, command: &[&str]) -> u8 {
    run_with_patience(socket, command, REJOIN_TRIES).await
}

/// [`run`], knocking at most `tries` more times when the dog is gone
pub(crate) async fn run_with_patience(socket: &Path, command: &[&str], tries: u32) -> u8 {
    let patience = tries;
    let Some((program, args)) = command.split_first() else {
        return NOT_RUN;
    };
    let mut signals = match Signals::new() {
        Ok(signals) => signals,
        Err(e) => return failed(&e, NO_LEASE),
    };
    let ask = Ask {
        take: CARGO_TEST.into(),
        pid: std::process::id(),
        what: command.join(" "),
        running: false,
    };
    let visit = match wait_for_turn(socket, &ask, &mut signals, patience).await {
        Ok(Ok(visit)) => visit,
        Ok(Err(caught)) => return caught.number(),
        Err(e) => return failed(&unreached(&e), NO_LEASE),
    };
    // A signal before the start reached nobody else, so it ends the run.
    tokio::select! {
        biased;
        caught = signals.recv() => {
            visit.give_back().await;
            return caught.number();
        }
        () = std::future::ready(()) => {}
    }
    if let Err(e) = visit.inheritable(true) {
        say(&format!("{e}: the lease is held by this process alone"));
    }
    let spawned = tokio::process::Command::from(crate::spawn::command(program))
        .args(args)
        .spawn();
    let _ = visit.inheritable(false);
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) => {
            visit.give_back().await;
            return failed(&format!("cannot run {program}: {e}"), NOT_RUN);
        }
    };
    let rejoin = Ask {
        running: true,
        ..ask
    };
    let mut held = Some(visit);
    let mut tries = 0;
    loop {
        let rejoining = held.is_none() && tries < patience;
        let event = tokio::select! {
            status = child.wait() => Event::Ended(status),
            caught = signals.recv() => Event::Caught(caught),
            () = closed(held.as_mut()) => Event::DogGone,
            () = tokio::time::sleep(REJOIN_EVERY), if rejoining => Event::Rejoin,
        };
        match event {
            Event::Ended(status) => {
                if let Some(visit) = held {
                    visit.give_back().await;
                }
                return status.map_or_else(|e| failed(&e.to_string(), 1), exit_number);
            }
            Event::Caught(caught) => {
                if let (Some(pid), Some(name)) = (child.id(), caught.forward()) {
                    signal_process(pid, name);
                }
            }
            Event::DogGone => {
                held = None;
                tries = 0;
                say("the dog went away: asking it to count this run again once it is back");
            }
            Event::Rejoin => {
                tries += 1;
                match seat(socket, &rejoin).await {
                    Ok(visit) => {
                        held = Some(visit);
                        say("the dog counts this run again");
                    }
                    Err(_) if tries == patience => {
                        say("the dog did not come back, so this run is not counted");
                    }
                    Err(_) => {}
                }
            }
        }
    }
}

enum Event {
    Ended(std::io::Result<std::process::ExitStatus>),
    Caught(Caught),
    DogGone,
    Rejoin,
}

// Knocks and waits for the grant, knocking again for a while if the dog
// is gone or goes away first. A signal while waiting closes the connection,
// which withdraws the ask, and is handed back.
async fn wait_for_turn(
    socket: &Path,
    ask: &Ask,
    signals: &mut Signals,
    patience: u32,
) -> Result<Result<Visit, Caught>, String> {
    let mut tries = 0;
    loop {
        let lost = match one_turn(socket, ask, signals).await {
            Turn::Granted(visit) => return Ok(Ok(visit)),
            Turn::Caught(caught) => return Ok(Err(caught)),
            Turn::Refused(why) => return Err(why),
            Turn::Lost(why) => why,
        };
        tries += 1;
        if tries > patience {
            return Err(lost);
        }
        if tries == 1 {
            say(&format!("{lost}: asking again while the dog comes back"));
        }
        tokio::select! {
            () = tokio::time::sleep(REJOIN_EVERY) => {}
            caught = signals.recv() => return Ok(Err(caught)),
        }
    }
}

enum Turn {
    Granted(Visit),
    Caught(Caught),
    // The dog said no, which asking again would not change.
    Refused(String),
    // No dog, or it went away before granting.
    Lost(String),
}

async fn one_turn(socket: &Path, ask: &Ask, signals: &mut Signals) -> Turn {
    let mut visit = match Visit::knock(socket, ask).await {
        Ok(visit) => visit,
        Err(e) => return Turn::Lost(e),
    };
    let mut first = true;
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
                say(&format!("waiting for {CARGO_TEST}, {ahead} ahead"));
            }
            Ok(Some(Answer::Error(why))) => {
                return Turn::Refused(format!("the dog refused: {why}"));
            }
            Ok(None) => {
                return Turn::Lost(format!(
                    "the dog closed the door before granting {CARGO_TEST}"
                ));
            }
            Err(e) => return Turn::Lost(e),
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
