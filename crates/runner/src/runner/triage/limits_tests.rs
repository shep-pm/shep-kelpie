//! The limits kelpie puts on triage, through the runner's stand-ins
//!
//! None calls a model. The day's cap holds an issue in the due list; the author's
//! cap and the account's age end it at `needs-triage` with no call or comment; the
//! body limit cuts what the call is given. The maintainer's own issues and one
//! `shep kelpie triage` asked for skip all four.
//!
//! The caps count the calls the records save, so an author's round spends places
//! beside the first pass's call, and a round call past a cap waits for a free
//! place. A call counts when it reached a model, whatever its reply came to.

use std::sync::Mutex;

use serde_json::{Value, json};

use super::ask_tests::{A_WORDS, asked as one_question, filed, settled, wrote};
use super::limits::DAY;
use super::marker::MARKER;
use super::pick_tests::{MAINTAINER, due, issue, list, rig_with, status, triaging};
use super::research_tests::{WORDS, calls, handed, moves, reply, restart, triage};
use crate::board::TRIAGE;
use crate::ports::{AuthorAssociation, Clock, OpenIssue, Role, Timestamp};
use crate::runner::{Runner, step};
use crate::state::{StateStore, TRIAGED_KEPT};
use crate::test::{Rig, Scripted};

// A rig with `[triage]` set to `lines`, its representative saying `WORDS` and
// its triager answering with nothing found
fn limits_on(lines: &str) -> (Rig, Mutex<Runner>) {
    let rig = rig_with(lines);
    let runner = rig.open().unwrap();
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);
    (rig, runner)
}

// Records saved to the state, as triages that already happened leave them: one
// per `(author, when its call ran)`, numbered from 11, none held by a limit.
// The runner must be closed first, as a restart is.
fn seeded(rig: &Rig, records: &[(&str, u64)]) {
    let kept: Vec<(&str, u64, u32)> = records
        .iter()
        .map(|(author, at)| (*author, *at, 1))
        .collect();
    seeded_as(rig, &kept);
}

// Records saved to the state as `kept` gives them: `(author, when the record was
// made, how many calls it holds)`, numbered from 11, in the order given, which is
// the order the state keeps them in and the order a prune drops them in.
fn seeded_as(rig: &Rig, kept: &[(&str, u64, u32)]) {
    write_records(rig, kept, false);
}

// [`seeded_as`], adding to what the state already has instead of setting it
fn seeded_more(rig: &Rig, kept: &[(&str, u64, u32)]) {
    write_records(rig, kept, true);
}

fn write_records(rig: &Rig, kept: &[(&str, u64, u32)], append: bool) {
    let store = StateStore::new(rig.paths().state);
    let mut saved = serde_json::to_value(store.load().unwrap().unwrap()).unwrap();
    let first = (saved["triage"].get("issues"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let first = u64::try_from(first).unwrap();
    let records: Vec<Value> = kept
        .iter()
        .enumerate()
        .map(|(n, (author, at, calls))| {
            json!({
                "issue": 11 + first + n as u64,
                "comment": 11 + first + n as u64,
                "step": "done",
                "at": at,
                "author": author,
                "calls": (0..*calls).map(|c| at + u64::from(c)).collect::<Vec<_>>(),
            })
        })
        .collect();
    if append {
        saved["triage"]["issues"]
            .as_array_mut()
            .unwrap()
            .extend(records);
    } else {
        saved["triage"]["issues"] = json!(records);
    }
    let state = serde_json::from_value(saved).unwrap();
    store.save(&state).unwrap();
}

// Takes the author and the calls off the saved record of `number`, as a file
// saved before either existed leaves it
fn stripped(rig: &Rig, number: u64) {
    let store = StateStore::new(rig.paths().state);
    let mut saved = serde_json::to_value(store.load().unwrap().unwrap()).unwrap();
    let records = saved["triage"]["issues"].as_array_mut().unwrap();
    let found = (records.iter_mut())
        .find(|r| r["issue"] == json!(number))
        .unwrap();
    found["author"] = json!("");
    found["calls"] = json!([]);
    let state = serde_json::from_value(saved).unwrap();
    store.save(&state).unwrap();
}

// The record `status` gives issue `number`, or none
fn record(rig: &Rig, runner: &Mutex<Runner>, number: u64) -> Value {
    let issues = triage(rig, runner)["issues"].clone();
    let found = (issues.as_array().unwrap_or(&Vec::new()))
        .iter()
        .find(|r| r["issue"] == json!(number))
        .cloned();
    found.unwrap_or(Value::Null)
}

// What the triager was asked, which holds the issue fenced as data
fn asked(rig: &Rig) -> String {
    let mut seen = calls(rig, Role::Triager);
    assert_eq!(seen.len(), 1, "one research call");
    seen.remove(0).call.prompt
}

// An issue by an author whose account the forge counts as no history at all
fn fresh(rig: &Rig, number: u64, author: &str, after: u64) -> OpenIssue {
    let mut fresh = issue(rig, number, author, after);
    fresh.author_association = Some(AuthorAssociation::None);
    fresh
}

// The author's round on issue 30, driven as the ask tests drive it: their
// answers, with the second triager call scripted and not yet run.
fn answered(rig: &Rig) {
    wrote(rig, "newcomer", "Q1: Linux, on start.");
    rig.claude.script_for(Role::Triager, [reply(settled())]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
}

// How many comments the App put on issue 30
fn posted(rig: &Rig) -> Vec<u64> {
    (rig.github.comments())
        .into_iter()
        .map(|(n, _)| n)
        .collect()
}

// The issues kelpie put `label` on, in the order it did
fn labelled(rig: &Rig, label: &str) -> Vec<u64> {
    (rig.github.writes())
        .into_iter()
        .filter(|(_, call)| call.path.ends_with("/labels"))
        .filter_map(|(_, call)| {
            let body: Value = serde_json::from_str(call.body.as_deref()?).ok()?;
            if body["labels"][0].as_str()? != label {
                return None;
            }
            let parts: Vec<&str> = call.path.split('/').collect();
            let at = (parts.iter()).position(|p| *p == "issues")?;
            parts.get(at + 1)?.parse().ok()
        })
        .collect()
}

#[test]
fn the_eleventh_issue_in_a_day_waits_and_is_triaged_once_a_place_opens() {
    let (rig, runner) = triaging("all");
    let now = rig.clock.now().0;
    drop(runner);
    // The day's ten calls, the oldest made now
    seeded(&rig, &vec![("someone", now); 10]);
    let runner = rig.open().unwrap();
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);
    list(&rig, issue(&rig, 30, "newcomer", 5));

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 0, "no eleventh call");
    assert_eq!(rig.github.comments(), [], "nothing is posted");
    assert_eq!(due(&rig, &runner), [30], "it waits in the due list");
    let status = status(&rig, &runner);
    assert_eq!(
        status["triage"]["due"][0]["author_association"], "contributor",
        "the listing says how the author stands to the repo, which is what the \
         account check reads"
    );
    let daily = triage(&rig, &runner)["daily"].clone();
    assert_eq!(daily["used"], 10);
    assert_eq!(daily["cap"], 10);
    assert_eq!(daily["opens"], now + DAY, "when the next place opens");
    assert_eq!(daily["wait"], DAY, "and how long from now");
    assert_eq!(record(&rig, &runner, 30), Value::Null, "no record");

    // A place opens when the oldest of the day's calls is a day old, and the
    // issue that was waiting takes it.
    rig.clock.advance(DAY + 1);
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 1);
    assert_eq!(rig.github.comments(), [(30, handed(WORDS))]);
    assert_eq!(triage(&rig, &runner)["daily"]["used"], 1);
}

#[test]
fn an_authors_third_issue_in_a_day_gets_needs_triage_with_no_call_and_no_comment() {
    let (rig, runner) = triaging("all");
    let now = rig.clock.now().0;
    drop(runner);
    // Two calls already spent on this author's issues, under two spellings of
    // the login, and one on someone else's
    seeded(
        &rig,
        &[("Newcomer", now), ("newcomer", now), ("someone", now)],
    );
    let runner = rig.open().unwrap();
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);
    list(&rig, issue(&rig, 30, "newcomer", 5));
    list(&rig, issue(&rig, 31, "someone", 6));

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        calls(&rig, Role::Triager).len(),
        1,
        "only the other author's issue is called"
    );
    assert_eq!(
        rig.github.comments(),
        [(31, handed(WORDS))],
        "no comment on the held issue"
    );
    assert_eq!(
        labelled(&rig, TRIAGE),
        [30, 31],
        "the held issue is labelled, and nothing else is written on it"
    );
    let held = record(&rig, &runner, 30);
    assert_eq!(held["step"], "done");
    assert_eq!(held["comment"], 0, "no comment was posted");
    assert_eq!(held["held"], "author-cap");
    assert_eq!(held["author"], "newcomer");
    assert_eq!(due(&rig, &runner), Vec::<u64>::new(), "the pass is over it");

    // It stays out of the pass, whatever the next poll lists.
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(calls(&rig, Role::Triager).len(), 1);
    assert_eq!(rig.github.comments().len(), 1);
}

#[test]
fn a_new_account_with_no_history_gets_needs_triage_with_no_call() {
    let (rig, runner) = triaging("all");
    let now = rig.clock.now().0;
    rig.forge
        .set_account_created("newcomer", Timestamp(now - 5 * DAY));
    list(&rig, fresh(&rig, 30, "newcomer", 5));

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 0);
    assert_eq!(rig.github.comments(), []);
    assert_eq!(
        moves(&rig),
        [format!("make {TRIAGE}"), format!("label {TRIAGE}")]
    );
    let held = record(&rig, &runner, 30);
    assert_eq!(held["held"], "new-account");
    assert_eq!(held["comment"], 0);
    assert_eq!(held["step"], "done");
    assert_eq!(due(&rig, &runner), Vec::<u64>::new());
}

#[test]
fn a_new_account_that_has_contributed_before_is_triaged() {
    let (rig, runner) = triaging("all");
    let now = rig.clock.now().0;
    // Both accounts are five days old: only the one with no history is held
    rig.forge
        .set_account_created("newcomer", Timestamp(now - 5 * DAY));
    rig.forge
        .set_account_created("regular", Timestamp(now - 5 * DAY));
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);
    list(&rig, fresh(&rig, 30, "newcomer", 5));
    list(&rig, issue(&rig, 31, "regular", 6));

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 1);
    assert_eq!(rig.github.comments(), [(31, handed(WORDS))]);
    assert_eq!(record(&rig, &runner, 30)["held"], "new-account");
    assert_eq!(record(&rig, &runner, 31).get("held"), None);

    // The hold is final for the issue. Its label keeps it out of the pass even
    // once the account is old enough, and only the verb reaches it again.
    rig.clock.advance(30 * DAY);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(calls(&rig, Role::Triager).len(), 1, "it was labelled over");
}

#[test]
fn a_long_body_reaches_the_call_cut_with_a_note_and_the_title_whole() {
    let rig = rig_with("pick = \"all\"");
    let runner = rig.open().unwrap();
    let mut crash = issue(&rig, 30, "newcomer", 5);
    crash.title = "Crash on start".to_owned();
    crash.body = format!(
        "HEAD-OF-BODY\n{}",
        "zz ".repeat(10_000) // 30 KB of it, and the tail past the cut
    );
    crash.body.push_str("END-OF-BODY");
    list(&rig, crash.clone());
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);

    assert_eq!(step(&runner).unwrap(), None);

    let prompt = asked(&rig);
    assert!(prompt.contains("# Crash on start"), "the title is whole");
    assert!(
        prompt.contains(&crash.body[..20 * 1024]),
        "the body is cut at 20 KB"
    );
    assert!(
        !prompt.contains(&crash.body[20 * 1024..]),
        "nothing past the cut is given"
    );
    assert!(
        prompt.contains("it is 30 KB"),
        "the note says how long it was"
    );
    assert!(
        prompt.contains("at most 20 KB"),
        "and the size it was cut to"
    );
}

#[test]
fn a_cut_body_never_ends_mid_character() {
    let rig = rig_with("pick = \"all\"");
    let runner = rig.open().unwrap();
    let mut crash = issue(&rig, 30, "newcomer", 5);
    crash.title = "Crash on start".to_owned();
    // The 20 KB boundary falls inside the two bytes of `é`
    crash.body = format!("HEAD-OF-BODY{}éEND-OF-BODY", "z".repeat(20_467));
    list(&rig, crash.clone());
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);

    assert_eq!(step(&runner).unwrap(), None);

    let prompt = asked(&rig);
    assert!(
        prompt.contains(&crash.body[..20 * 1024 - 1]),
        "the cut steps back to the character before the boundary"
    );
    assert!(!prompt.contains("é"), "no half of a character is given");
    assert!(!prompt.contains("END-OF-BODY"));
}

#[test]
fn the_maintainers_issues_and_the_verb_skip_every_limit() {
    // The maintainer's own issue, with every limit already spent on it
    let rig = rig_with("pick = \"all\"");
    let runner = rig.open().unwrap();
    let now = rig.clock.now().0;
    rig.forge
        .set_account_created(MAINTAINER, Timestamp(now - DAY));
    drop(runner);
    seeded(
        &rig,
        &[("someone", now)]
            .repeat(8)
            .into_iter()
            .chain([("Boss", now), ("BOSS", now)])
            .collect::<Vec<_>>(),
    );
    let runner = rig.open().unwrap();
    let mut own = fresh(&rig, 30, MAINTAINER, 5);
    own.body = "HEAD-OF-BODY".to_owned() + &"zz ".repeat(10_000);
    own.body.push_str("END-OF-BODY");
    list(&rig, own.clone());
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        calls(&rig, Role::Triager).len(),
        1,
        "the day's cap is spent and it is triaged anyway"
    );
    assert_eq!(
        rig.github.comments(),
        [(30, format!("{WORDS}\n\n{MARKER}"))]
    );
    let prompt = asked(&rig);
    assert!(prompt.contains("END-OF-BODY"), "its body is not cut");
    assert_eq!(record(&rig, &runner, 30).get("held"), None);

    // An issue the verb asked for, from a new account with no history, on a day
    // whose calls are all spent
    let rig = rig_with("pick = \"all\"");
    let runner = rig.open().unwrap();
    let now = rig.clock.now().0;
    rig.forge
        .set_account_created("newcomer", Timestamp(now - DAY));
    drop(runner);
    seeded(&rig, &vec![("someone", now); 10]);
    let runner = rig.open().unwrap();
    list(&rig, fresh(&rig, 30, "newcomer", 5));
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);

    let reply = rig.ask(&runner, "triage", Some("30"));
    assert_eq!(reply.get("error"), None, "{reply}");
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 1);
    assert_eq!(rig.github.comments(), [(30, handed(WORDS))]);
    assert_eq!(record(&rig, &runner, 30).get("held"), None);
}

#[test]
fn zero_turns_each_limit_off() {
    let rig = rig_with(
        "pick = \"all\"\nper_day = 0\nper_author_per_day = 0\nmin_account_days = 0\nmax_body_kb = 0",
    );
    let runner = rig.open().unwrap();
    let now = rig.clock.now().0;
    rig.forge
        .set_account_created("newcomer", Timestamp(now - 10));
    drop(runner);
    // What each limit would have held it for, on a day full of calls by its own
    // author and by others
    seeded(
        &rig,
        &[("newcomer", now)]
            .repeat(2)
            .into_iter()
            .chain(vec![("someone", now); 8])
            .collect::<Vec<_>>(),
    );
    let runner = rig.open().unwrap();
    for number in [30, 31, 32] {
        let mut long = fresh(&rig, number, "newcomer", 5 + number);
        long.body = "HEAD-OF-BODY".to_owned() + &"zz ".repeat(10_000);
        long.body.push_str("END-OF-BODY");
        list(&rig, long);
    }
    rig.claude
        .script_for(Role::Representative, (0..3).map(|_| Scripted::Say(WORDS)));
    rig.claude.script_for(
        Role::Triager,
        [reply(json!({})), reply(json!({})), reply(json!({}))],
    );

    for _ in 0..3 {
        assert_eq!(step(&runner).unwrap(), None);
        rig.next_look();
    }

    assert_eq!(calls(&rig, Role::Triager).len(), 3, "all three are called");
    let prompts = (calls(&rig, Role::Triager))
        .into_iter()
        .map(|seen| seen.call.prompt)
        .collect::<Vec<_>>();
    assert!(
        prompts.iter().all(|p| p.contains("END-OF-BODY")),
        "no body is cut"
    );
    assert_eq!(rig.github.comments().len(), 3, "and each gets its comment");
    assert_eq!(
        triage(&rig, &runner).get("daily"),
        None,
        "no daily cap to show"
    );
}

#[test]
fn an_accounts_age_is_read_once_per_author() {
    let (rig, runner) = limits_on("pick = \"all\"\nper_author_per_day = 0");
    let now = rig.clock.now().0;
    for login in ["newcomer", "other"] {
        rig.forge.set_account_created(login, Timestamp(now - 3));
    }
    for (number, author) in [
        (30, "newcomer"),
        (31, "Newcomer"),
        (32, "NEWCOMER"),
        (33, "other"),
    ] {
        list(&rig, fresh(&rig, number, author, 5 + number));
    }

    for _ in 0..4 {
        assert_eq!(step(&runner).unwrap(), None);
        rig.next_look();
    }

    assert_eq!(calls(&rig, Role::Triager).len(), 0, "all four are held");
    assert_eq!(
        rig.forge.account_reads("newcomer"),
        1,
        "once for three issues"
    );
    assert_eq!(rig.forge.account_reads("other"), 1);

    // An age the forge cannot say holds nothing, and is not asked again
    rig.forge.set_accounts_down(true);
    list(&rig, fresh(&rig, 34, "another", 60));
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(
        calls(&rig, Role::Triager).len(),
        1,
        "an unreadable age holds nothing"
    );
    assert_eq!(rig.forge.account_reads("another"), 1);
}

#[test]
fn the_day_counts_every_call_on_one_issue_and_not_the_issue_once() {
    let (rig, runner) = filed("pick = \"all\"", "newcomer", one_question());

    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(calls(&rig, Role::Triager).len(), 1, "the first pass");

    answered(&rig);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        calls(&rig, Role::Triager).len(),
        2,
        "the author's answers had a call of their own"
    );
    let daily = triage(&rig, &runner)["daily"].clone();
    assert_eq!(daily["used"], 2, "one issue spends more than one place");
    assert_eq!(
        record(&rig, &runner, 30)["calls"].as_array().unwrap().len(),
        2,
        "and the record keeps the moment of each"
    );
}

#[test]
fn an_authors_answers_call_past_the_days_cap_waits_and_is_made_once_a_place_opens() {
    let (rig, runner) = filed("pick = \"all\"\nper_day = 1", "newcomer", one_question());

    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(
        calls(&rig, Role::Triager).len(),
        1,
        "the first pass takes the day's one place"
    );

    answered(&rig);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        calls(&rig, Role::Triager).len(),
        1,
        "the answers call waits on the day's cap"
    );
    assert_eq!(posted(&rig).len(), 1, "only the triage comment");
    assert_eq!(triage(&rig, &runner)["daily"]["used"], 1);

    // A round is read from the records at every poll, so a call that waited is
    // made as soon as a place frees. It is not held by a limit and not labelled
    // over: the issue is already triaged.
    rig.clock.advance(DAY + 1);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 2, "the call is made");
    assert_eq!(posted(&rig).len(), 2, "and its comment goes up");
    assert_eq!(record(&rig, &runner, 30).get("held"), None);
}

#[test]
fn a_call_whose_reply_cannot_be_read_still_spends_a_place_of_the_day() {
    let rig = rig_with("pick = \"all\"\nper_day = 2");
    let runner = rig.open().unwrap();
    list(&rig, issue(&rig, 30, "newcomer", 5));
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    rig.claude.script_for(
        Role::Triager,
        [
            Scripted::Text("not a triage reply"),
            Scripted::Text("still not a triage reply"),
            reply(json!({})),
        ],
    );

    // The call ran and its reply could not be read: nothing is posted and the
    // issue is tried again, but the model was called and its place is spent.
    assert_eq!(step(&runner).unwrap(), None);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 2);
    assert_eq!(
        triage(&rig, &runner)["daily"]["used"],
        2,
        "an unreadable reply spends a place beside a readable one"
    );
    assert_eq!(rig.github.comments(), [], "nothing is posted");
    assert_eq!(
        record(&rig, &runner, 30),
        Value::Null,
        "and it has no record"
    );

    // The day's two places are gone, so the issue waits for the next one
    // instead of being tried a third time.
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(calls(&rig, Role::Triager).len(), 2, "no third call");
    assert_eq!(due(&rig, &runner), [30], "it waits in the due list");

    // A place opens when the oldest of the day's calls is a day old.
    rig.clock.advance(DAY + 1);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 3, "and it takes the next");
    assert_eq!(rig.github.comments(), [(30, handed(WORDS))]);
}

#[test]
fn a_round_call_spends_its_place_before_the_next_issue_is_called() {
    // The day has two places, and the issue's own round takes both.
    let (rig, runner) = filed("pick = \"all\"\nper_day = 2", "newcomer", one_question());

    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(calls(&rig, Role::Triager).len(), 1, "the first pass");

    // The author answers and kelpie restarts before the second pass runs, so it
    // owes the call for the answers.
    wrote(&rig, "newcomer", "Q1: Linux, on start.");
    let runner = restart(&rig, runner);
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);

    assert_eq!(step(&runner).unwrap(), None, "the answers are heard");
    rig.next_look();
    assert_eq!(
        step(&runner).unwrap(),
        None,
        "and the second pass is called"
    );

    assert_eq!(calls(&rig, Role::Triager).len(), 2, "the answers call");
    assert_eq!(triage(&rig, &runner)["daily"]["used"], 2, "the day is full");
    assert_eq!(
        record(&rig, &runner, 30)["calls"].as_array().unwrap().len(),
        2,
        "and the record carries the moment of each"
    );

    // The next issue waits for a place: the round's call was counted before it
    // was admitted, and not only when its own comment was recorded.
    list(&rig, issue(&rig, 31, "someone", 7));
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(calls(&rig, Role::Triager).len(), 2, "no third call");
    assert_eq!(due(&rig, &runner), [31], "it waits in the due list");
}

#[test]
fn an_issue_whose_author_deleted_their_account_counts_toward_no_authors_cap() {
    let (rig, runner) = triaging("all");
    let now = rig.clock.now().0;
    drop(runner);
    // Two calls on issues whose author's account is gone since: the listing
    // gives no login for either, so they share no bucket and cannot spend each
    // other's allowance
    seeded(&rig, &[("", now), ("", now)]);
    let runner = rig.open().unwrap();
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);
    list(&rig, issue(&rig, 30, "", 5));

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        calls(&rig, Role::Triager).len(),
        1,
        "no author's cap reaches an issue with no author to count"
    );
    assert_eq!(rig.github.comments(), [(30, handed(WORDS))]);
    assert_eq!(record(&rig, &runner, 30).get("held"), None);
}

#[test]
fn a_burst_of_held_issues_does_not_free_the_authors_cap_partway_through() {
    // The day's cap is set wide enough not to bind, so `status` shows what the
    // day has spent and the author's cap is what holds their issues.
    let rig = rig_with("pick = \"all\"\nper_day = 20\nper_author_per_day = 2");
    let runner = rig.open().unwrap();
    let now = rig.clock.now().0;
    drop(runner);
    // A state already at its cap of finished records: the author's two issues
    // triaged today, each holding its call, then the rest of the cap from issues
    // a limit held, which hold no call of their own. Each new hold pushes one
    // record out, and the ones that hold calls are what the caps count.
    let mut kept: Vec<(&str, u64, u32)> = vec![("newcomer", now, 1), ("newcomer", now, 1)];
    kept.extend((0..TRIAGED_KEPT - 2).map(|_| ("someone", 0, 0)));
    seeded_as(&rig, &kept);
    let runner = rig.open().unwrap();
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);
    // Above the numbers the seeded records hold, which run from 11
    for number in 300..306 {
        list(&rig, issue(&rig, number, "newcomer", 5));
    }

    for _ in 0..3 {
        assert_eq!(step(&runner).unwrap(), None);
        rig.next_look();
    }

    assert_eq!(
        calls(&rig, Role::Triager).len(),
        0,
        "every one of their issues is held, and none is called"
    );
    assert_eq!(rig.github.comments(), [], "nothing is posted");
    assert_eq!(
        labelled(&rig, TRIAGE),
        (300..306).collect::<Vec<_>>(),
        "each got needs-triage with no call"
    );
    assert_eq!(
        triage(&rig, &runner)["daily"]["used"],
        2,
        "and the day still counts the author's two calls"
    );
}

#[test]
fn a_first_pass_call_that_never_posted_spends_its_place_across_a_restart() {
    let rig = rig_with("pick = \"all\"\nper_day = 1");
    let runner = rig.open().unwrap();
    list(&rig, issue(&rig, 30, "newcomer", 5));
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    rig.claude.script_for(
        Role::Triager,
        [Scripted::Text("not a triage reply"), reply(json!({}))],
    );

    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(calls(&rig, Role::Triager).len(), 1, "the call ran");
    assert_eq!(
        record(&rig, &runner, 30),
        Value::Null,
        "and its reply left no record"
    );

    // A restart loses the run's own memory of the call, so the place it spent is
    // saved in the state at the moment it ended: the issue waits for the next
    // place instead of being called again the same day.
    let runner = restart(&rig, runner);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 1, "no second call");
    assert_eq!(
        triage(&rig, &runner)["daily"]["used"],
        1,
        "the restart still counts it"
    );
    assert_eq!(due(&rig, &runner), [30], "it waits in the due list");

    // A place opens when that call is a day old, and the issue takes it.
    rig.clock.advance(DAY + 1);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 2, "the next place");
    assert_eq!(rig.github.comments(), [(30, handed(WORDS))]);
}

#[test]
fn the_maintainers_own_round_is_called_when_its_record_names_no_author() {
    let (rig, runner) = filed("pick = \"all\"\nper_day = 1", MAINTAINER, one_question());

    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(calls(&rig, Role::Triager).len(), 1, "the first pass");

    // The author answers. The record is as a file saved before the caps
    // existed leaves it: no author, no calls. The day's one place is spent on
    // someone else's issue.
    wrote(&rig, MAINTAINER, "Q1: Linux, on start.");
    drop(runner);
    stripped(&rig, 30);
    seeded_more(&rig, &[("someone", rig.clock.now().0, 1)]);
    let runner = rig.open().unwrap();
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);

    assert_eq!(step(&runner).unwrap(), None, "the answers are heard");
    assert_eq!(
        record(&rig, &runner, 30)["author"],
        MAINTAINER,
        "and the listing fills in the author the record did not name"
    );
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None, "the round's call");

    assert_eq!(
        calls(&rig, Role::Triager).len(),
        2,
        "the maintainer's own round skips the day's cap"
    );
    assert_eq!(record(&rig, &runner, 30).get("held"), None);
}
