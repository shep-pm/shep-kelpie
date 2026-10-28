//! The verbs a trigger queued while a pass held the runner's lock
//!
//! `add`, `rework`, `adopt`, `triage` and `rule` read the forge, so a trigger that finds the lock
//! held queues them and answers from the posted copy (see [`super::posted`]). The pass takes them
//! up before it does anything else: a verb checks its guards, reads with the lock let go, then
//! takes the lock again, checks the guards once more and applies. A refusal is logged once and
//! stands in `status` until the verb is asked again and goes through.

use std::sync::Mutex;

use super::add::AddError;
use super::adopt::AdoptError;
use super::posted::Queued;
use super::rework::ReworkError;
use super::trigger::lock;
use super::{Answer, Runner};
use crate::ticket::TicketId;

/// Applies every verb queued since the last pass
pub(super) fn take_up(runner: &Mutex<Runner>) {
    let postings = lock(runner).postings();
    for verb in postings.take_queued() {
        let key = verb.to_string();
        let applied = match verb {
            Queued::Add(ticket) => add(runner, ticket),
            Queued::Rework(number) => rework(runner, number),
            Queued::Adopt(number) => adopt(runner, number),
            Queued::Triage(issue) => triage(runner, issue),
            Queued::Rule(id, answer) => rule(runner, id, answer),
        };
        let held = lock(runner);
        match applied {
            Ok(()) => held.notes.ends(&key),
            Err(why) => held
                .notes
                .standing(&key, format!("queued `{key}` refused: {why}")),
        }
    }
}

fn add(runner: &Mutex<Runner>, ticket: TicketId) -> Result<(), String> {
    match read_and_add(runner, ticket) {
        // A work item for the ticket is open, whoever opened it: the add is done.
        Err(AddError::InFlight(_)) if lock(runner).state.item(ticket).is_some() => Ok(()),
        applied => applied.map_err(|e| e.to_string()),
    }
}

fn read_and_add(runner: &Mutex<Runner>, ticket: TicketId) -> Result<(), AddError> {
    let (tickets, remote) = {
        let mut held = lock(runner);
        held.looks.poll_reset();
        held.add_allowed(ticket)?;
        (held.ports.tickets.clone(), held.remote.clone())
    };
    let found = (tickets.issue(&remote, ticket)).map_err(AddError::Forge)?;
    lock(runner).add_found(ticket, found, None).map(drop)
}

fn rework(runner: &Mutex<Runner>, number: u64) -> Result<(), String> {
    let (forge, remote) = {
        let held = lock(runner);
        held.rework_allowed().map_err(|e| e.to_string())?;
        (held.ports.forge.clone(), held.remote.clone())
    };
    let pr = (forge.reviewed(&remote, number))
        .map_err(|e| ReworkError::PullRequest(number, e).to_string())?;
    (lock(runner).rework_found(number, pr))
        .map(drop)
        .map_err(|e| e.to_string())
}

fn adopt(runner: &Mutex<Runner>, number: u64) -> Result<(), String> {
    let (forge, remote) = {
        let held = lock(runner);
        if !held.adopt_wanted(number).map_err(|e| e.to_string())? {
            return Ok(());
        }
        (held.ports.forge.clone(), held.remote.clone())
    };
    let pr = (forge.reviewed(&remote, number))
        .map_err(|e| AdoptError::PullRequest(number, e).to_string())?;
    (lock(runner).adopt_found(number, &pr)).map_err(|e| e.to_string())
}

fn triage(runner: &Mutex<Runner>, issue: u64) -> Result<(), String> {
    let (tickets, remote) = {
        let held = lock(runner);
        held.triage_allowed(issue)?;
        (held.ports.tickets.clone(), held.remote.clone())
    };
    let open =
        (tickets.open_issues(&remote)).map_err(|e| format!("cannot read the open issues: {e}"))?;
    lock(runner).triage_found(issue, &open)
}

// The reads an answer makes sit inside `Runner::rule`, so they hold the lock.
fn rule(runner: &Mutex<Runner>, id: TicketId, answer: Answer) -> Result<(), String> {
    (lock(runner).rule(id, answer)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::runner::Answer;
    use crate::runner::posted::Queued;
    use crate::runner::trigger::lock;
    use crate::test::{Rig, ticket};

    fn ask(runner: &Mutex<Runner>, verb: Queued) {
        lock(runner).postings().ask_queued(verb);
    }

    #[test]
    fn a_queued_verb_is_applied_once_when_the_pass_takes_it() {
        let rig = Rig::new("koji");
        let runner = rig.open().unwrap();
        ask(&runner, Queued::Add(ticket(7)));

        take_up(&runner);
        take_up(&runner);

        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["work_item"]["issue"], "7");
        assert_eq!(status["work_items"].as_array().map(Vec::len), Some(1));
        assert_eq!(status.get("standing"), None, "nothing was refused");
    }

    #[test]
    fn a_queued_add_for_a_closed_issue_is_refused_in_notes_and_status() {
        let rig = Rig::new("koji");
        rig.forge.close_issue(7);
        let runner = rig.open().unwrap();
        ask(&runner, Queued::Add(ticket(7)));

        take_up(&runner);

        let refused = "queued `add 7` refused: #7 is closed";
        assert_eq!(lock(&runner).notes.take(), [refused]);
        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["work_item"], json!(null));
        assert_eq!(status["standing"], json!([refused]));
    }

    #[test]
    fn a_refusal_stops_standing_when_the_verb_goes_through_with_the_lock_free() {
        let rig = Rig::new("koji");
        rig.forge.close_issue(7);
        let runner = rig.open().unwrap();
        ask(&runner, Queued::Add(ticket(7)));
        take_up(&runner);
        assert_eq!(
            rig.ask(&runner, "status", None)["standing"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );

        rig.forge.reopen_issue(7);
        let status = rig.ask(&runner, "add", Some("7"));

        assert!(status["work_item"].is_object(), "{status}");
        assert_eq!(status["standing"], json!(null), "nothing stands");
    }

    #[test]
    fn a_queued_add_for_a_ticket_already_open_is_not_a_refusal() {
        let rig = Rig::new("koji");
        let runner = rig.open().unwrap();
        ask(&runner, Queued::Add(ticket(7)));
        ask(&runner, Queued::Add(ticket(7)));

        take_up(&runner);

        assert!(lock(&runner).notes.take().is_empty());
        let status = rig.ask(&runner, "status", None);
        assert!(status["work_item"].is_object(), "{status}");
        assert_eq!(status["standing"], json!(null), "nothing stands");
    }

    #[test]
    fn a_queued_adopt_for_a_pull_request_closed_during_the_read_is_refused() {
        let rig = Rig::new("shep");
        rig.push_by_hand("fix/timeline", "work.txt");
        rig.forge.open_pull_request(80, "fix/timeline", &[5]);
        let runner = Arc::new(rig.open().unwrap());
        ask(&runner, Queued::Adopt(80));

        let read = rig.forge.hold_next_call();
        let pass = {
            let runner = Arc::clone(&runner);
            thread::spawn(move || take_up(&runner))
        };
        assert!(read.waiting(Duration::from_secs(5)), "the pass reads it");
        rig.forge.close_open_pull_request(80).unwrap();
        drop(read);
        pass.join().unwrap();

        let refused = "queued `adopt 80` refused: pull request #80 is closed";
        assert_eq!(lock(&runner).notes.take(), [refused]);
        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["adopted"], json!([]));
        assert_eq!(status["standing"], json!([refused]));
    }

    #[test]
    fn a_queued_rule_reaches_the_item_it_named() {
        let (rig, runner, _) = Rig::parked("rotom");
        ask(
            &runner,
            Queued::Rule(ticket(71), Answer::No("not yet".into())),
        );

        take_up(&runner);

        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["rulings"], json!([]));
        assert_eq!(status["work_item"]["issue"], "7");
        assert_eq!(status["work_item"]["phase"]["state"], "implement");
        assert_eq!(status.get("standing"), None);
    }

    #[test]
    fn a_trigger_is_answered_while_a_queued_rule_reads_the_forge() {
        let (rig, runner, _) = Rig::parked("rotom");
        let runner = Arc::new(runner);
        let postings = lock(&runner).postings();
        rig.ask(&runner, "status", None);
        ask(
            &runner,
            Queued::Rule(ticket(71), Answer::No("not yet".into())),
        );

        let read = rig.forge.hold_next_call();
        let pass = {
            let runner = Arc::clone(&runner);
            thread::spawn(move || take_up(&runner))
        };
        assert!(
            read.waiting(Duration::from_secs(5)),
            "the rule reads the forge"
        );
        let (sent, reply) = std::sync::mpsc::channel();
        {
            let runner = Arc::clone(&runner);
            thread::spawn(move || {
                let _ = sent.send(crate::runner::trigger::answer(
                    &runner, &postings, "status", None,
                ));
            });
        }
        let reply = reply.recv_timeout(Duration::from_secs(5));
        drop(read);
        pass.join().unwrap();

        let reply: serde_json::Value =
            serde_json::from_str(&reply.expect("status waited for the lock")).unwrap();
        assert_eq!(reply["project"], "rotom");
    }
}
