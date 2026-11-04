use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use super::*;
use crate::settings::{EndpointUrl, GatewayKey};
use crate::test::{StandInEndpoint, Take};

const QUICK: Timing = Timing {
    answer: Duration::from_secs(5),
    release: Duration::from_secs(5),
    renew: Duration::from_millis(20),
    retry: Duration::from_millis(20),
    lapse: Duration::from_secs(5),
};

const LEASES: &str = r#"{"leases":[
    {"id":"L1","client":"a","model":"coder","since":"t1","note":null,"revoked":{"by":"admin","note":"x"}},
    {"id":"L2","client":"b","model":"other","since":"t2","note":"n","revoked":null},
    {"id":"L3","client":"c","model":"coder","since":"t3","note":"mine","revoked":null}
],"waiters":[]}"#;

#[test]
fn the_holder_is_the_live_lease_on_the_model() {
    let holder = holder_in(LEASES, "coder").unwrap().unwrap();
    assert_eq!(
        (
            holder.lease.as_str(),
            holder.client.as_str(),
            holder.what.as_str(),
            holder.since.as_str()
        ),
        ("L3", "c", "mine", "t3")
    );
    assert_eq!(holder_in(LEASES, "idle"), Ok(None));
    assert_eq!(holder_in("{}", "coder"), Ok(None));
    assert!(holder_in("<html>", "coder").is_err());
}

const BUSY: &str = r#"{"error":"busy","reason":"qwen is loading"}"#;

const TURN: &str =
    r#"{"error":"busy","reason":"qwen is serving kelpie #9, next in line","reason_kind":"turn"}"#;

fn gateway(server: &StandInEndpoint) -> Upstream {
    let base = EndpointUrl::try_from(server.url().to_owned()).unwrap();
    Upstream::new(&base)
        .unwrap()
        .with_key(GatewayKey::for_test("pk"))
}

fn calls(server: &StandInEndpoint) -> Vec<String> {
    let seen = server.seen().into_iter();
    seen.filter_map(|l| l.strip_suffix(" HTTP/1.0").map(str::to_owned))
        .collect()
}

#[test]
fn a_200_that_is_not_a_model_list_is_an_error() {
    for body in [
        "{}",
        r#"{"data":"none"}"#,
        r#"{"data":[{"name":"qwen"}]}"#,
        "[]",
    ] {
        let server = StandInEndpoint::start([])
            .like_paddock("pk", &[])
            .with_models_body(body);
        let err = models(&gateway(&server)).unwrap_err();
        assert_eq!(
            err, "`GET /v1/models` answered something other than a model list",
            "{body}"
        );
    }
    let server = StandInEndpoint::start([]).like_paddock("pk", &["qwen"]);
    assert_eq!(models(&gateway(&server)), Ok(vec!["qwen".to_owned()]));
    let empty = StandInEndpoint::start([]).like_paddock("pk", &[]);
    assert_eq!(models(&gateway(&empty)), Ok(vec![]));
}

#[test]
fn a_take_asks_for_a_heartbeat_renews_it_and_releases_it_when_dropped() {
    let server = StandInEndpoint::start([]).like_paddock("pk", &[]);
    let lease = Lease::take(
        &gateway(&server),
        "qwen",
        "kelpie #7 worker",
        &|| false,
        &|_| {},
        &Ending::default(),
        QUICK,
    );
    let lease = lease.unwrap().unwrap();
    let asked = &server.requests()[0];
    let wanted = json!({ "model": "qwen", "hold": "heartbeat", "ttl": "120s",
                         "note": "kelpie #7 worker" });
    assert_eq!(asked, &wanted);
    let until = Instant::now() + Duration::from_secs(5);
    while !calls(&server).contains(&"PUT /paddock/leases/L1".to_owned()) {
        assert!(Instant::now() < until, "no renewal: {:?}", server.seen());
        thread::sleep(Duration::from_millis(10));
    }
    drop(lease);
    assert_eq!(calls(&server).last().unwrap(), "DELETE /paddock/leases/L1");
}

#[test]
fn a_lease_gives_the_id_paddock_granted() {
    let server = StandInEndpoint::start([]).like_paddock("pk", &[]);
    let lease = held(&server, &Ending::default(), QUICK);
    assert_eq!(lease.id(), "L1");
}

#[test]
fn a_granted_id_that_cannot_go_in_a_header_fails_the_take() {
    for body in [r#"{"id":"L1\r\nx-evil: 1"}"#, r#"{"id":"L 1"}"#] {
        let server = StandInEndpoint::start([])
            .like_paddock("pk", &[])
            .with_takes([Take::Status(200, body)]);
        let err = Lease::take(
            &gateway(&server),
            "qwen",
            "n",
            &|| false,
            &|_| {},
            &Ending::default(),
            QUICK,
        )
        .unwrap_err();
        assert!(err.contains("`POST /paddock/leases`"), "{err}");
        assert!(err.contains("refused"), "{err}");
        assert_eq!(calls(&server), ["POST /paddock/leases"], "{body}");
    }
}

#[test]
fn a_resumed_lease_is_renewed_at_once_and_taken_and_released_never_twice() {
    let server = StandInEndpoint::start([]).like_paddock("pk", &[]);
    // Renewals every minute: any made in the test is the resume's own first.
    let slow = Timing {
        renew: Duration::from_secs(60),
        ..QUICK
    };
    let lease = Lease::resume(&gateway(&server), "L1", &Ending::default(), slow).unwrap();
    until("renewal", &server, || {
        calls(&server).contains(&"PUT /paddock/leases/L1".to_owned())
    });
    assert_eq!(calls(&server), ["PUT /paddock/leases/L1"], "no second take");
    drop(lease);
    assert_eq!(
        calls(&server),
        ["PUT /paddock/leases/L1", "DELETE /paddock/leases/L1"]
    );
}

#[test]
fn a_resumed_lease_the_gateway_lost_in_between_ends_the_call() {
    let server = StandInEndpoint::start([])
        .like_paddock("pk", &[])
        .refusing_renewals(404);
    let ending = Ending::default();
    let mut lease = Lease::resume(&gateway(&server), "L1", &ending, QUICK).unwrap();
    until("end", &server, || ending.asked());
    let lost = lease.finish().unwrap();
    assert!(lost.contains("L1") && lost.contains("HTTP 404"), "{lost}");
}

#[test]
fn an_id_that_cannot_go_in_a_path_is_not_resumed() {
    let server = StandInEndpoint::start([]).like_paddock("pk", &[]);
    let err = Lease::resume(&gateway(&server), "L1/../x", &Ending::default(), QUICK);
    let err = err.unwrap_err();
    assert!(err.contains("carried lease id"), "{err}");
    assert!(!err.contains("POST"), "no take was made: {err}");
    assert!(server.seen().is_empty());
}

fn held(server: &StandInEndpoint, ending: &Ending, timing: Timing) -> Lease {
    let taken = Lease::take(
        &gateway(server),
        "qwen",
        "n",
        &|| false,
        &|_| {},
        ending,
        timing,
    );
    taken.unwrap().unwrap()
}

fn until(what: &str, server: &StandInEndpoint, done: impl Fn() -> bool) {
    let until = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < until, "no {what}: {:?}", server.seen());
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_renewal_the_gateway_refuses_ends_the_turn_with_a_failure() {
    let server = StandInEndpoint::start([])
        .like_paddock("pk", &[])
        .refusing_renewals(404);
    let ending = Ending::default();
    let mut lease = held(&server, &ending, QUICK);
    until("end", &server, || ending.asked());
    let lost = lease.finish().unwrap();
    assert!(lost.contains("L1") && lost.contains("HTTP 404"), "{lost}");
}

#[test]
fn renewals_the_gateway_leaves_unanswered_end_the_turn_once_the_ttl_has_run_out() {
    let server = StandInEndpoint::start([])
        .like_paddock("pk", &[])
        .hanging_renewals();
    let timing = Timing {
        answer: Duration::from_millis(50),
        lapse: Duration::from_millis(300),
        ..QUICK
    };
    let ending = Ending::default();
    let mut lease = held(&server, &ending, timing);
    until("end", &server, || ending.asked());
    assert!(lease.finish().unwrap().contains("did not answer"));
}

#[test]
fn unanswered_renewals_are_tried_until_the_ttl_has_run_out_and_not_before() {
    let server = StandInEndpoint::start([])
        .like_paddock("pk", &[])
        .hanging_renewals();
    let timing = Timing {
        answer: Duration::from_millis(20),
        renew: Duration::from_millis(100),
        lapse: Duration::from_millis(400),
        ..QUICK
    };
    let ending = Ending::default();
    let started = Instant::now();
    let _lease = held(&server, &ending, timing);
    until("end", &server, || ending.asked());
    assert!(started.elapsed() >= timing.lapse, "{:?}", started.elapsed());
}

#[test]
fn a_lease_let_go_of_with_a_renewal_in_flight_never_ends_the_call() {
    let server = StandInEndpoint::start([])
        .like_paddock("pk", &[])
        .hanging_renewals();
    let timing = Timing {
        answer: Duration::from_secs(2),
        lapse: Duration::from_millis(500),
        ..QUICK
    };
    let ending = Ending::default();
    let mut lease = held(&server, &ending, timing);
    until("renewal", &server, || {
        calls(&server).contains(&"PUT /paddock/leases/L1".to_owned())
    });
    assert_eq!(lease.finish(), None);
    // The renewal in flight is cut short at the lapse, after the lease was let go of.
    thread::sleep(Duration::from_millis(900));
    assert!(!ending.asked());
}

#[test]
fn a_healthy_lease_is_not_lost() {
    let server = StandInEndpoint::start([]).like_paddock("pk", &[]);
    let ending = Ending::default();
    let mut lease = held(&server, &ending, QUICK);
    until("renewal", &server, || {
        calls(&server).contains(&"PUT /paddock/leases/L1".to_owned())
    });
    assert!(!ending.asked());
    assert_eq!(lease.finish(), None);
}

#[test]
fn dropping_a_lease_with_a_renewal_in_flight_releases_it_at_once() {
    let server = StandInEndpoint::start([])
        .like_paddock("pk", &[])
        .hanging_renewals();
    let lease = held(&server, &Ending::default(), QUICK);
    until("renewal", &server, || {
        calls(&server).contains(&"PUT /paddock/leases/L1".to_owned())
    });
    let dropped = Instant::now();
    drop(lease);
    assert!(dropped.elapsed() < Duration::from_secs(1));
    assert_eq!(calls(&server).last().unwrap(), "DELETE /paddock/leases/L1");
}

#[test]
fn a_busy_take_is_asked_again_and_a_refused_one_names_why() {
    let server = StandInEndpoint::start([])
        .like_paddock("pk", &[])
        .with_takes([Take::Status(503, BUSY)]);
    let busy = AtomicUsize::new(0);
    let told = |_| {
        busy.fetch_add(1, Ordering::SeqCst);
    };
    let lease = Lease::take(
        &gateway(&server),
        "qwen",
        "n",
        &|| false,
        &told,
        &Ending::default(),
        QUICK,
    )
    .unwrap();
    assert!(lease.is_some());
    assert_eq!(busy.load(Ordering::SeqCst), 1, "each busy answer is told");
    let takes = calls(&server)
        .iter()
        .filter(|l| l.starts_with("POST"))
        .count();
    assert_eq!(takes, 2);

    let wrong = gateway(&server).with_key(GatewayKey::for_test("other"));
    let err = Lease::take(
        &wrong,
        "qwen",
        "n",
        &|| false,
        &|_| {},
        &Ending::default(),
        QUICK,
    )
    .unwrap_err();
    assert_eq!(
        err,
        "`POST /paddock/leases` answered HTTP 401, unauthorized"
    );
    let bad = r#"{"error":"bad_lease_request","detail":"ttl is at most 1h"}"#;
    let server = StandInEndpoint::start([])
        .like_paddock("pk", &[])
        .with_takes([Take::Status(400, bad)]);
    let err = Lease::take(
        &gateway(&server),
        "qwen",
        "n",
        &|| false,
        &|_| {},
        &Ending::default(),
        QUICK,
    )
    .unwrap_err();
    assert!(
        err.ends_with("bad_lease_request: ttl is at most 1h"),
        "{err}"
    );
}

#[test]
fn a_take_paddock_keeps_queued_or_busy_ends_when_the_turn_gives_up() {
    for take in [Take::Hang, Take::Status(503, BUSY)] {
        let server = StandInEndpoint::start([])
            .like_paddock("pk", &[])
            .with_takes([take.clone(), take]);
        let looks = AtomicUsize::new(0);
        let give_up = || looks.fetch_add(1, Ordering::SeqCst) >= 3;
        let started = Instant::now();
        let lease = Lease::take(
            &gateway(&server),
            "qwen",
            "n",
            &give_up,
            &|_| {},
            &Ending::default(),
            QUICK,
        );
        assert!(matches!(lease, Ok(None)), "{lease:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!calls(&server).iter().any(|l| l.starts_with("DELETE")));
    }
}

fn round_calls(server: &StandInEndpoint) -> Vec<String> {
    let all = calls(server).into_iter();
    all.filter(|l| l.starts_with("POST") || l.starts_with("DELETE"))
        .collect()
}

fn around<T>(
    server: &StandInEndpoint,
    ending: &Ending,
    give_up: &dyn Fn() -> bool,
    told: &Mutex<Vec<Stand>>,
    call: impl FnOnce() -> T,
) -> Result<T, Unheld> {
    let tell = |stand| told.lock().unwrap().push(stand);
    let taking = Taking {
        model: "qwen",
        note: "kelpie local round 1",
        give_up,
        tell: &tell,
        ending,
    };
    let paddock = Paddock { timing: QUICK };
    let port: &dyn ModelGateway = &paddock;
    port.around(&gateway(server), &taking, call)
}

#[test]
fn a_reviewer_round_takes_a_lease_before_the_call_and_returns_it_after() {
    let server = StandInEndpoint::start([]).like_paddock("pk", &[]);
    let told = Mutex::new(Vec::new());
    let during = around(&server, &Ending::default(), &|| false, &told, || {
        round_calls(&server)
    });
    assert_eq!(during, Ok(vec!["POST /paddock/leases".to_owned()]));
    assert_eq!(
        round_calls(&server),
        ["POST /paddock/leases", "DELETE /paddock/leases/L1"]
    );
    assert_eq!(*told.lock().unwrap(), [Stand::Asking, Stand::Granted]);
}

#[test]
fn a_turn_reason_is_told_as_a_turn_and_any_other_busy_answer_as_busy() {
    for (body, wanted) in [(TURN, Stand::Turn), (BUSY, Stand::Busy), ("", Stand::Busy)] {
        let server = StandInEndpoint::start([])
            .like_paddock("pk", &[])
            .with_takes([Take::Status(503, body)]);
        let told = Mutex::new(Vec::new());
        around(&server, &Ending::default(), &|| false, &told, || ()).unwrap();
        let told = told.into_inner().unwrap();
        assert_eq!(told, [Stand::Asking, wanted, Stand::Granted], "{body}");
    }
}

#[test]
fn a_lease_lost_during_the_call_fails_it_whatever_the_call_returned() {
    let server = StandInEndpoint::start([])
        .like_paddock("pk", &[])
        .refusing_renewals(404);
    let ending = Ending::default();
    let lost = around(&server, &ending, &|| false, &Mutex::default(), || {
        until("end", &server, || ending.asked());
        "finished anyway"
    });
    let Err(Unheld::Lost(why)) = lost else {
        panic!("{lost:?}");
    };
    assert!(why.contains("L1") && why.contains("HTTP 404"), "{why}");
}

#[test]
fn a_round_that_gives_up_the_wait_never_runs_and_one_refused_names_why() {
    let server = StandInEndpoint::start([])
        .like_paddock("pk", &[])
        .with_takes([Take::Hang]);
    let ran = AtomicUsize::new(0);
    let call = || ran.fetch_add(1, Ordering::SeqCst);
    let told = Mutex::default();
    let waited = around(&server, &Ending::default(), &|| true, &told, call);
    assert_eq!(waited, Err(Unheld::GaveUp));
    assert_eq!(ran.load(Ordering::SeqCst), 0);

    let bad = r#"{"error":"bad_lease_request","detail":"ttl is at most 1h"}"#;
    let server = StandInEndpoint::start([])
        .like_paddock("pk", &[])
        .with_takes([Take::Status(400, bad)]);
    let refused = around(&server, &Ending::default(), &|| false, &told, call);
    let Err(Unheld::Refused(why)) = refused else {
        panic!("{refused:?}");
    };
    assert!(
        why.ends_with("bad_lease_request: ttl is at most 1h"),
        "{why}"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 0);
}

// What the lease `around_leased` holds says it used, on a paddock that answers its release with `headers`.
fn metered_with(headers: &[(&str, &str)]) -> (Option<Metered>, Vec<String>) {
    let server = StandInEndpoint::start([])
        .like_paddock("pk", &[])
        .releasing_with(headers);
    let tell = |_| {};
    let ending = Ending::default();
    let taking = Taking {
        model: "qwen",
        note: "kelpie #7 worker",
        give_up: &|| false,
        tell: &tell,
        ending: &ending,
    };
    let paddock = Paddock { timing: QUICK };
    let port: &dyn ModelGateway = &paddock;
    let (ran, metered) = port
        .around_leased(&gateway(&server), &taking, |lease| lease.is_some())
        .unwrap();
    assert!(ran);
    (metered, round_calls(&server))
}

#[test]
fn a_release_reads_its_energy_headers() {
    let (metered, calls) = metered_with(&[
        ("Paddock-Energy-Wh", "12.345"),
        ("Paddock-Energy-Usd", "0.0042"),
        ("Paddock-Energy-Source", "wall"),
    ]);
    let wanted = Metered {
        wh: 12.345,
        cost: Some(Cost(4_200_000)),
        source: MeterSource::Wall,
    };
    assert_eq!(metered, Some(wanted));
    assert_eq!(
        calls,
        ["POST /paddock/leases", "DELETE /paddock/leases/L1"],
        "the release read is the release, not a second one"
    );
}

#[test]
fn a_release_without_energy_headers_is_unmetered() {
    assert_eq!(metered_with(&[]).0, None);
    for unreadable in [
        [
            ("Paddock-Energy-Wh", "lots"),
            ("Paddock-Energy-Source", "wall"),
        ],
        [
            ("Paddock-Energy-Wh", "-1"),
            ("Paddock-Energy-Source", "wall"),
        ],
        [
            ("Paddock-Energy-Wh", "NaN"),
            ("Paddock-Energy-Source", "wall"),
        ],
        [
            ("Paddock-Energy-Wh", "3.5"),
            ("Paddock-Energy-Source", "tea leaves"),
        ],
    ] {
        assert_eq!(metered_with(&unreadable).0, None, "{unreadable:?}");
    }
    assert_eq!(
        metered_with(&[("Paddock-Energy-Source", "wall")]).0,
        None,
        "a source with no watt-hours"
    );
}

#[test]
fn an_unpriced_release_has_watt_hours_and_no_cost() {
    let headers = [
        ("Paddock-Energy-Wh", "0.5"),
        ("Paddock-Energy-Source", "parts"),
    ];
    let wanted = Metered {
        wh: 0.5,
        cost: None,
        source: MeterSource::Parts,
    };
    assert_eq!(metered_with(&headers).0, Some(wanted));
    // A price that cannot be read leaves the watt-hours standing.
    let garbled = [("Paddock-Energy-Usd", "free"), headers[0], headers[1]];
    assert_eq!(metered_with(&garbled).0, Some(wanted));
}

#[test]
fn a_lease_dropped_mid_call_is_still_released() {
    let server = StandInEndpoint::start([])
        .like_paddock("pk", &[])
        .releasing_with(&[
            ("Paddock-Energy-Wh", "1"),
            ("Paddock-Energy-Source", "wall"),
        ]);
    let ending = Ending::default();
    let taking = Taking {
        model: "qwen",
        note: "kelpie #7 worker",
        give_up: &|| false,
        tell: &|_| {},
        ending: &ending,
    };
    let paddock = Paddock { timing: QUICK };
    let port: &dyn ModelGateway = &paddock;
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = port.around_leased(&gateway(&server), &taking, |_| panic!("the call died"));
    }));
    assert!(ran.is_err());
    assert_eq!(
        round_calls(&server),
        ["POST /paddock/leases", "DELETE /paddock/leases/L1"]
    );
}
