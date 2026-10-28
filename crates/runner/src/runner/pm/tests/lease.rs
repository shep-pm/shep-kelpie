//! A long wait for a gateway lease, told to the project manager as a wait and
//! not as a stuck worker

use std::sync::Mutex;

use crate::runner::Runner;
use crate::runner::flight::tests::Driven;
use crate::runner::step;
use crate::test::{CALL_PATIENCE, Hold, Rig, Scripted, StandInEndpoint};

use super::{answered, name_pm, past_the_window};

// An implementer on a model behind a gateway, which queues for its lease
const BEHIND_PADDOCK: &str = "---\nrole: implementer\nharness: pi\nmodel: coder\neffort: low\n\
                              gateway: paddock\ncontext: 65536\n---\n";

/// `rig`'s runner, with a project manager, a `mine` implementer behind a
/// gateway at `url`, and issue 7 open on it
fn behind(rig: &Rig, url: &str) -> Mutex<Runner> {
    rig.write_agent("mine", BEHIND_PADDOCK);
    // `name_pm` and `implementers` each rewrite the example's `implementers`
    // line, so the one that checks it goes first.
    name_pm(rig);
    rig.implementers(&["sonnet-high", "mine"]);
    let settings = std::fs::read_to_string(rig.kelpie_settings_file()).unwrap();
    let gateway = format!(
        "[gateways.paddock]\nurl = \"{}\"\nkey_env = \"CARGO_PKG_NAME\"\n",
        url
    );
    rig.set_kelpie_settings(&format!("{settings}{gateway}"));
    let runner = rig.open().unwrap();
    rig.agent_board.ranks(&["mine"]);
    rig.ask(&runner, "add", Some("7"));
    rig.agent_board.ranks_listed();
    runner
}

#[test]
fn a_long_lease_wait_wakes_it_naming_who_holds_the_lease() {
    let server = StandInEndpoint::start([])
        .like_paddock(env!("CARGO_PKG_NAME"), &["coder"])
        .with_paddock_status(
            r#"{"leases":[
                {"id":"L4","client":"maintainer","model":"coder","since":"2026-10-13T09:30:00Z","note":"shep kelpie lease run: cargo build","revoked":null}
            ],"waiters":[]}"#,
        );
    let rig = Rig::new("rotom");
    let runner = behind(&rig, server.host());
    let queue = Hold::default();
    let work = Hold::default();
    rig.claude.script([
        Scripted::Queued(queue.clone(), Box::new(Scripted::Hold(work.clone()))),
        Scripted::Say("{\"pick\": null}"),
    ]);
    let driven = Driven::new(&runner);
    driven.started();
    assert!(
        queue.entered(CALL_PATIENCE),
        "the turn never queued for its lease"
    );
    assert_eq!(driven.heard(), None, "the wait is no report");
    rig.clock.advance(11 * 60);
    // The board tells the wait on this pass, which opens its window.
    assert_eq!(step(&runner).unwrap(), None, "the wait is no report");
    past_the_window(&rig);

    let (woke_for, acted, dropped) = answered(step(&runner).unwrap());
    assert_eq!(
        woke_for,
        [
            "#7 has waited 11m for a lease on its model, which `maintainer` holds for `shep \
             kelpie lease run: cargo build`. Its worker is not stuck: it has had no model to \
             show anything through, and kelpie leaves its turn running. Reply to the \
             maintainer if the lease should be released."
        ]
    );
    assert_eq!((acted, dropped), (vec![], vec![]));
    assert_eq!(
        rig.ask(&runner, "status", None)["work_items"][0]["waiting"]["why"],
        "lease",
        "the turn still waits, and is not parked"
    );
    queue.release();
    assert_eq!(driven.heard(), None, "the grant is no report");
    assert!(work.entered(CALL_PATIENCE), "the turn never began");
    work.release();
    assert!(work.answered(CALL_PATIENCE), "the turn never answered");
}

/// A gateway that answers busy turned the call away for a reason other than
/// another lease's turn, so no client is in the way and none is named
#[test]
fn a_gateway_that_answers_busy_wakes_it_as_a_wait_and_names_no_holder() {
    let rig = Rig::new("rotom");
    rig.lease_held_by("maintainer", "shep kelpie lease run: cargo build");
    let runner = behind(&rig, "http://127.0.0.1:8799");
    let busy = Hold::default();
    let work = Hold::default();
    rig.claude.script([
        Scripted::Busy(busy.clone(), Box::new(Scripted::Hold(work.clone()))),
        Scripted::Say("{\"pick\": null}"),
    ]);
    let driven = Driven::new(&runner);
    driven.started();
    assert!(
        busy.entered(CALL_PATIENCE),
        "the turn never said its gateway was busy"
    );
    assert_eq!(driven.heard(), None, "the wait is no report");
    rig.clock.advance(11 * 60);
    // The board tells the wait on this pass, which opens its window.
    assert_eq!(step(&runner).unwrap(), None, "the wait is no report");
    past_the_window(&rig);

    let (woke_for, acted, dropped) = answered(step(&runner).unwrap());
    assert_eq!(
        woke_for,
        [
            "#7 has waited 11m for its gateway, which answered busy. Its worker is not stuck: \
             it has had no model to show anything through, and kelpie leaves its turn running."
        ],
        "a busy gateway names no holder, and asks no lease to be released"
    );
    assert_eq!((acted, dropped), (vec![], vec![]));
    assert_eq!(
        rig.ask(&runner, "status", None)["work_items"][0]["waiting"]["why"],
        "busy",
        "the turn still waits, and is not parked"
    );
    busy.release();
    assert_eq!(driven.heard(), None, "the gateway coming back is no report");
    assert!(work.entered(CALL_PATIENCE), "the turn never began");
    work.release();
    assert!(work.answered(CALL_PATIENCE), "the turn never answered");
}

/// The other side of the same rule: a wait for a lease names the holder the
/// gateway gave
#[test]
fn a_lease_wait_names_the_holder_the_gateway_gave() {
    let rig = Rig::new("rotom");
    rig.lease_held_by("maintainer", "shep kelpie lease run: cargo build");
    let runner = behind(&rig, "http://127.0.0.1:8799");
    let queue = Hold::default();
    let work = Hold::default();
    rig.claude.script([
        Scripted::Queued(queue.clone(), Box::new(Scripted::Hold(work.clone()))),
        Scripted::Say("{\"pick\": null}"),
    ]);
    let driven = Driven::new(&runner);
    driven.started();
    assert!(
        queue.entered(CALL_PATIENCE),
        "the turn never queued for its lease"
    );
    assert_eq!(driven.heard(), None, "the wait is no report");
    rig.clock.advance(11 * 60);
    // The board tells the wait on this pass, which opens its window.
    assert_eq!(step(&runner).unwrap(), None, "the wait is no report");
    past_the_window(&rig);

    let (woke_for, acted, dropped) = answered(step(&runner).unwrap());
    assert_eq!(
        woke_for,
        [
            "#7 has waited 11m for a lease on its model, which `maintainer` holds for `shep \
             kelpie lease run: cargo build`. Its worker is not stuck: it has had no model to \
             show anything through, and kelpie leaves its turn running. Reply to the maintainer \
             if the lease should be released."
        ]
    );
    assert_eq!((acted, dropped), (vec![], vec![]));
    queue.release();
    assert_eq!(driven.heard(), None, "the grant is no report");
    assert!(work.entered(CALL_PATIENCE), "the turn never began");
    work.release();
    assert!(work.answered(CALL_PATIENCE), "the turn never answered");
}
