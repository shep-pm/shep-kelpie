//! A Codex turn's context size and the compaction kelpie asks of it
//!
//! `fixtures/codex-exec-before-compaction.jsonl`, `fixtures/codex-exec-compacted.jsonl`
//! and `fixtures/codex-rollout-compacted.jsonl` are recorded on codex-cli 0.159.3 (#771).

use super::tests::{FRESH, THREAD, WORKER_ID, World, id, stand_in, strings};
use super::*;
use crate::ports::{COMPACT, Role};

/// A session's first turn, which ran three model requests
const BEFORE: &str = include_str!("../../../../../fixtures/codex-exec-before-compaction.jsonl");
/// That session resumed with a limit under its context, so it compacted first
const COMPACTED: &str = include_str!("../../../../../fixtures/codex-exec-compacted.jsonl");
/// The session's rollout after that turn: each request's `token_usage_record`
/// and `token_count`, and the `compacted` line between the turns
const ROLLOUT: &str = include_str!("../../../../../fixtures/codex-rollout-compacted.jsonl");

// Puts `rollout` where Codex keeps the rollout of `THREAD` in the call's home.
fn rollout(w: &World, call: &AgentCall, rollout: &str) {
    let folder = Files::of(call).home.join("sessions/2026/10/13");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join(format!("rollout-2026-10-13T09-00-00-{THREAD}.jsonl")),
        rollout,
    )
    .unwrap();
    assert!(w.path("worker").is_dir());
}

#[test]
fn a_turn_s_context_is_the_last_token_count_events_last_request_input() {
    let w = World::new();
    let cli = stand_in(&w, COMPACTED);
    let call = w.call(Role::Worker, Session::New(id(WORKER_ID)));
    cli.prepare(&call).unwrap();
    rollout(&w, &call, ROLLOUT);

    let reply = cli.run(&call, &Ending::default()).unwrap();

    // The turn's own request after the compaction, not the session's running total.
    assert_eq!(reply.context, Some(19_478));
}

#[test]
fn a_rollout_with_no_token_count_event_or_no_file_reports_no_context() {
    let w = World::new();
    let cli = stand_in(&w, COMPACTED);
    let call = w.call(Role::Worker, Session::New(id(WORKER_ID)));
    cli.prepare(&call).unwrap();

    let no_file = cli.run(&call, &Ending::default()).unwrap();
    assert_eq!(no_file.context, None);

    // A rollout that held only a session's start, its usage records and the compaction.
    let events = ROLLOUT
        .lines()
        .filter(|l| !l.contains("\"last_token_usage\""))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(events.lines().count(), 7, "{events}");
    rollout(&w, &call, &events);
    let again = w.call(Role::Worker, Session::Resume(id(WORKER_ID)));
    let no_event = cli.run(&again, &Ending::default()).unwrap();
    assert_eq!(no_event.context, None);
}

#[test]
fn a_compact_window_sets_the_auto_compact_limit_and_scope_and_zero_leaves_them_out() {
    let w = World::new();
    let mut call = w.fenced(Path::new("/k/kelpie"), Session::New(id(WORKER_ID)));
    call.compact_window = std::num::NonZeroU32::new(200_000);

    let with = strings(&call, None, None);

    assert!(
        with.windows(2)
            .any(|a| a == ["-c", "model_auto_compact_token_limit=200000"]),
        "{with:?}"
    );
    assert!(
        with.windows(2)
            .any(|a| a == ["-c", "model_auto_compact_token_limit_scope=\"total\""]),
        "{with:?}"
    );

    // The runner passes no window as zero, which the call holds as `None`.
    call.compact_window = std::num::NonZeroU32::new(0);
    let without = strings(&call, None, None);
    assert!(
        !without.iter().any(|a| a.contains("auto_compact")),
        "{without:?}"
    );
}

// The session `FRESH` began, with `ROLLOUT` as the rollout its last turn left.
fn after_a_turn(w: &World, cli: &CodexCli) -> AgentCall {
    let first = w.call(Role::Worker, Session::New(id(WORKER_ID)));
    cli.prepare(&first).unwrap();
    rollout(w, &first, ROLLOUT);
    cli.run(&first, &Ending::default()).unwrap();
    std::fs::remove_file(w.path("argv")).unwrap();
    w.call(Role::Worker, Session::Resume(id(WORKER_ID)))
}

#[test]
fn a_compact_call_runs_no_codex_and_records_the_context_it_read() {
    let w = World::new();
    let cli = stand_in(&w, FRESH);
    let mut call = after_a_turn(&w, &cli);
    call.prompt = COMPACT.into();

    let reply = cli.run(&call, &Ending::default()).unwrap();

    assert!(!w.path("argv").exists(), "codex ran");
    assert_eq!(reply.text, "");
    assert_eq!(reply.usage, Usage::default());
    assert_eq!(reply.context, None);
    let recorded = thread(&Files::of(&call), &id(WORKER_ID)).unwrap().unwrap();
    assert_eq!(recorded.context, Some(19_478));
}

#[test]
fn a_compaction_reports_the_sessions_cost_so_far() {
    let w = World::new();
    let cli = stand_in(&w, FRESH);
    let first = w.call(Role::Worker, Session::New(id(WORKER_ID)));
    cli.prepare(&first).unwrap();
    let turn = cli.run(&first, &Ending::default()).unwrap();
    let mut call = w.call(Role::Worker, Session::Resume(id(WORKER_ID)));
    call.prompt = COMPACT.into();

    let reply = cli.run(&call, &Ending::default()).unwrap();

    // A priced call that adds nothing: the session's cost stands where the turn left it.
    assert_eq!(reply.session_cost, Some(Cost(15_433_200)));
    assert_eq!(reply.session_cost, turn.session_cost);
}

#[test]
fn the_turn_after_a_compact_call_passes_the_lower_of_the_window_and_the_recorded_context_less_one()
{
    let w = World::new();
    let cli = stand_in(&w, FRESH);
    let mut compact = after_a_turn(&w, &cli);
    compact.prompt = COMPACT.into();
    cli.run(&compact, &Ending::default()).unwrap();
    let limit = |w: &World| {
        std::fs::read_to_string(w.path("argv"))
            .unwrap()
            .lines()
            .find_map(|a| {
                a.strip_prefix("model_auto_compact_token_limit=")
                    .map(str::to_owned)
            })
    };

    // The session held 19,478 tokens, so a limit of 19,477 is already reached.
    let mut next = w.call(Role::Worker, Session::Resume(id(WORKER_ID)));
    next.compact_window = std::num::NonZeroU32::new(200_000);
    cli.run(&next, &Ending::default()).unwrap();
    assert_eq!(limit(&w).as_deref(), Some("19477"));

    // That turn ran, so the one after it holds only the window.
    cli.run(&next, &Ending::default()).unwrap();
    assert_eq!(limit(&w).as_deref(), Some("200000"));

    // A window under the context is the lower, and a session not compacted has the window.
    cli.run(&compact, &Ending::default()).unwrap();
    next.compact_window = std::num::NonZeroU32::new(1_000);
    cli.run(&next, &Ending::default()).unwrap();
    assert_eq!(limit(&w).as_deref(), Some("1000"));
}

// The rollout as the session's first turn left it: before the compaction's lines.
fn first_turn_rollout() -> String {
    ROLLOUT.lines().take(7).collect::<Vec<_>>().join("\n")
}

// The session `BEFORE` began, with `rollout` as the rollout left by the turn
// that `COMPACTED` is the output of, which a resumed call reads.
fn compacted_turn(w: &World, first: &str, rollout_after: &str) -> AgentReply {
    let cli = stand_in(w, BEFORE);
    let call = w.call(Role::Worker, Session::New(id(WORKER_ID)));
    cli.prepare(&call).unwrap();
    rollout(w, &call, first);
    cli.run(&call, &Ending::default()).unwrap();
    rollout(w, &call, rollout_after);
    let resumed = w.call(Role::Worker, Session::Resume(id(WORKER_ID)));
    stand_in(w, COMPACTED)
        .run(&resumed, &Ending::default())
        .unwrap()
}

// `turn.completed` counts the turn's own request alone: 87,318 input and 249
// output. The compaction's request is only in the rollout's last
// `token_usage_record`, so the turn reports that record's `turn_token_usage`:
// 48,865 input, 40,832 of it cached, and 240 output.
#[test]
fn a_turn_that_compacted_reports_its_last_token_usage_records_turn_usage() {
    let w = World::new();

    let reply = compacted_turn(&w, &first_turn_rollout(), ROLLOUT);

    assert_eq!(
        reply.usage,
        Usage {
            input: 8033,
            cache_write: 0,
            cache_write_5m: 0,
            cache_read: 40_832,
            output: 240,
        }
    );
}

fn without_records(rollout: &str) -> String {
    rollout
        .lines()
        .filter(|l| !l.contains("\"type\":\"token_usage_record\""))
        .collect::<Vec<_>>()
        .join("\n")
}

// What `turn.completed` reports: the turn's own request, with no compaction in it.
const OWN_REQUEST: Usage = Usage {
    input: 7702,
    cache_write: 0,
    cache_write_5m: 0,
    cache_read: 11_776,
    output: 20,
};

#[test]
fn a_turn_whose_rollout_has_no_token_usage_record_reports_the_turn_s_own_usage() {
    let w = World::new();

    let reply = compacted_turn(
        &w,
        &without_records(&first_turn_rollout()),
        &without_records(ROLLOUT),
    );

    assert_eq!(reply.usage, OWN_REQUEST);
}

#[test]
fn a_token_usage_record_from_an_earlier_turn_leaves_the_turn_s_own_report() {
    let w = World::new();

    // The turn wrote no record: the rollout still ends on the first turn's.
    let reply = compacted_turn(&w, &first_turn_rollout(), &first_turn_rollout());

    assert_eq!(reply.usage, OWN_REQUEST);
}
