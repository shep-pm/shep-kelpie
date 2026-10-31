//! The spam check, through the runner's stand-ins
//!
//! The classifier answers in memory. At or above the threshold an issue gets
//! `possible-spam` and nothing else; below it the pass goes on. A gateway that
//! cannot be asked is told once and turns the check off, while an unreadable
//! answer leaves it on. The maintainer's own issues and `shep kelpie triage`
//! skip the check, and the verb takes off kelpie's own label, not one put on by
//! hand. A record the check makes carries the issue's earlier calls, and each
//! answered check spends no place of a triage cap.

use crate::test::ticket;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;

use serde_json::{Value, json};

use super::limits::DAY;
use super::marker::MARKER;
use super::pick_tests::{MAINTAINER, due, issue, list, rig_with};
use super::research_tests::crashed_with;
use super::research_tests::{
    MADE_AT_START, WORDS, calls, crash, handed, moves, reply, restart, triage,
};
use super::spam::POSSIBLE_SPAM;
#[cfg(unix)]
use crate::github::ApiError;
use crate::ports::{AuthorAssociation, ClassifierError, Clock, Role, Timestamp, Usage};
use crate::runner::{Runner, step};
use crate::test::{Rig, Scripted, SeenAsk};
use crate::usage::{self, CallKind, CallLine, Line};

// The gateway kelpie's own settings hold for these rigs, and the route its
// classifier is served on
const GATEWAY: &str = "gw";
const ROUTE: &str = "/classify";

// `[app.dogs.kelpie.triage.spam]` on that gateway, with the threshold left at its
// default of 0.8
const SPAM: &str = "gateway = \"gw\"\npath = \"/classify\"";

// A rig with `[triage]` set to `triage` and its `[triage.spam]` to `spam`,
// kelpie's own settings holding the gateway `gw`, issues 30 up from it open and
// untouched by each of `authors`, each with its representative saying `WORDS` and
// its triager answering with nothing found
fn rig_triaging(triage: &str, spam: &str, authors: &[&str]) -> Rig {
    let rig = rig_with(&format!(
        "{triage}\n[app.dogs.kelpie.triage.spam]\n{spam}\n"
    ));
    rig.gateway(GATEWAY, "http://gpu:8700");
    for (n, author) in authors.iter().enumerate() {
        let number = 30 + u64::try_from(n).unwrap();
        list(&rig, issue(&rig, number, author, 5 + n as u64));
    }
    let one = authors.len();
    rig.claude
        .script_for(Role::Representative, (0..one).map(|_| Scripted::Say(WORDS)));
    rig.claude
        .script_for(Role::Triager, (0..one).map(|_| reply(json!({}))));
    rig
}

// A rig whose classifier answers `probability`, over issue 30 alone, at the
// default threshold
fn checked(probability: f64) -> (Rig, Mutex<Runner>) {
    let rig = rig_triaging("pick = \"all\"", SPAM, &["newcomer"]);
    rig.classifier.says(probability);
    let runner = rig.open().unwrap();
    (rig, runner)
}

// The record `status` gives issue `number`, or none
fn record(rig: &Rig, runner: &Mutex<Runner>, number: u64) -> Value {
    let issues = triage(rig, runner)["issues"].clone();
    (issues.as_array().unwrap_or(&Vec::new()))
        .iter()
        .find(|r| r["issue"] == json!(number))
        .cloned()
        .unwrap_or(Value::Null)
}

// Every App write on issue `number`, by its path
fn written_on(rig: &Rig, number: u64) -> Vec<String> {
    (rig.github.writes())
        .into_iter()
        .map(|(_, call)| call.path)
        .filter(|path| path.contains(&format!("/issues/{number}")))
        .collect()
}

// The ledger's call lines
fn ledger(rig: &Rig) -> Vec<CallLine> {
    usage::read(&rig.paths().folder.join(usage::FILE))
        .unwrap()
        .into_iter()
        .filter_map(|line| match line {
            Line::Call(call) => Some(call),
            _ => None,
        })
        .collect()
}

// How many of the ledger's lines are checks
fn checks(rig: &Rig) -> Vec<CallLine> {
    (ledger(rig))
        .into_iter()
        .filter(|call| call.kind == CallKind::SpamCheck)
        .collect()
}

// Every label the App made, put on an issue or took off one, in order
#[cfg(unix)]
fn label_moves(rig: &Rig) -> Vec<String> {
    (rig.github.writes())
        .into_iter()
        .filter_map(|(_, call)| {
            let name = call.path.rsplit('/').next()?;
            let body: Value = serde_json::from_str(call.body.as_deref().unwrap_or("{}")).unwrap();
            match (call.path.contains("/issues/"), name) {
                (true, "labels") => Some(format!("label {}", body["labels"][0].as_str()?)),
                (true, _) if call.path.contains("/labels/") => Some(format!("unlabel {name}")),
                (false, "labels") => Some(format!("make {}", body["name"].as_str()?)),
                _ => None,
            }
        })
        .filter(|m| {
            !MADE_AT_START
                .iter()
                .any(|l| m.starts_with(&format!("make {l}")))
        })
        .collect()
}

// Runs one step with the state folder shut, so nothing can be saved
#[cfg(unix)]
fn step_with_the_state_shut(rig: &Rig, runner: &Mutex<Runner>) {
    let folder = rig.paths().state.parent().unwrap().to_owned();
    let mode = |m| std::fs::set_permissions(&folder, PermissionsExt::from_mode(m)).unwrap();
    mode(0o555);
    let _ = step(runner);
    mode(0o755);
}

// What kelpie told the log
fn told(runner: &Mutex<Runner>) -> Vec<String> {
    runner.lock().unwrap().take_notes()
}

#[test]
fn an_issue_over_the_threshold_gets_possible_spam_and_no_comment_or_call() {
    let (rig, runner) = checked(0.9);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(rig.classifier.count(), 1);
    assert_eq!(calls(&rig, Role::Triager).len(), 0, "no research call");
    assert_eq!(rig.github.comments(), [], "no comment");
    assert_eq!(
        moves(&rig),
        [
            format!("make {POSSIBLE_SPAM}"),
            format!("label {POSSIBLE_SPAM}")
        ]
    );
    let on_it = written_on(&rig, 30);
    assert_eq!(on_it.len(), 1, "{on_it:?}");
    assert!(
        on_it[0].ends_with("/labels"),
        "only the label was written on it: {on_it:?}"
    );
    let record = record(&rig, &runner, 30);
    assert_eq!(
        record["spam"], 0.9,
        "the record says spam and the probability"
    );
    assert_eq!(record["step"], "done");
    assert_eq!(record["comment"], 0, "no comment was posted");
    assert_eq!(record["author"], "newcomer");
    assert_eq!(record.get("held"), None, "and it is no limit's hold");
    assert_eq!(
        record.get("calls"),
        None,
        "a spam check spends no place of a triage cap"
    );
    assert_eq!(due(&rig, &runner), Vec::<u64>::new(), "the pass is over it");
    assert_eq!(triage(&rig, &runner)["daily"]["used"], 0);
    assert!(
        told(&runner)
            .iter()
            .any(|n| n.contains("#30") && n.contains(POSSIBLE_SPAM) && n.contains("0.9")),
        "{:?}",
        told(&runner)
    );

    // The label keeps it out of the pass, and nothing comes back to it.
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.classifier.count(), 1, "it is not asked about again");
    assert_eq!(calls(&rig, Role::Triager).len(), 0);
}

#[test]
fn an_issue_at_the_threshold_is_spam_and_one_just_below_is_not() {
    let rig = rig_triaging(
        "pick = \"all\"\nper_day = 0",
        SPAM,
        &["newcomer", "someone"],
    );
    rig.classifier.answers(&[0.8, 0.799]);
    let runner = rig.open().unwrap();

    for _ in 0..2 {
        assert_eq!(step(&runner).unwrap(), None);
        rig.next_look();
    }

    assert_eq!(
        calls(&rig, Role::Triager).len(),
        1,
        "at the threshold is spam, and one below is not"
    );
    assert_eq!(record(&rig, &runner, 30)["spam"], 0.8);
    assert_eq!(record(&rig, &runner, 31).get("spam"), None);
    assert_eq!(rig.github.comments(), [(31, handed(WORDS))]);
}

#[test]
fn an_issue_under_the_threshold_is_researched() {
    let (rig, runner) = checked(0.2);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(rig.classifier.count(), 1);
    assert_eq!(calls(&rig, Role::Triager).len(), 1);
    assert_eq!(
        rig.github.comments(),
        [(30, handed(WORDS))],
        "the pass went on as it was"
    );
    assert_eq!(
        moves(&rig),
        [
            "comment",
            "label priority: P3",
            "make needs-triage",
            "label needs-triage"
        ],
        "and nothing was labelled possible-spam"
    );
    assert_eq!(record(&rig, &runner, 30).get("spam"), None);
}

#[test]
fn no_spam_table_means_no_check_at_all() {
    let rig = rig_with("pick = \"all\"");
    let runner = rig.open().unwrap();
    list(&rig, crash(&rig, "It crashes when I run it."));
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        rig.classifier.count(),
        0,
        "no gateway, so no question asked"
    );
    assert_eq!(calls(&rig, Role::Triager).len(), 1);
    assert_eq!(checks(&rig), Vec::new(), "and no line on the ledger");
    assert_eq!(rig.github.comments(), [(30, handed(WORDS))]);
}

#[test]
fn an_unreachable_classifier_is_logged_once_and_the_issue_is_researched() {
    let rig = rig_triaging("pick = \"all\"", SPAM, &["newcomer", "someone"]);
    rig.classifier.fails(ClassifierError::Unreachable(
        "connection refused".to_owned(),
    ));
    let runner = rig.open().unwrap();

    for _ in 0..2 {
        assert_eq!(step(&runner).unwrap(), None);
        rig.next_look();
    }

    assert_eq!(
        calls(&rig, Role::Triager).len(),
        2,
        "both issues had their research call all the same"
    );
    assert_eq!(
        rig.github.comments(),
        [(30, handed(WORDS)), (31, handed(WORDS)),]
    );
    let notes: Vec<String> = (told(&runner))
        .into_iter()
        .filter(|n| n.contains("spam check is off"))
        .collect();
    assert_eq!(notes.len(), 1, "told once: {notes:?}");
    assert!(notes[0].contains("connection refused"), "{}", notes[0]);
    assert_eq!(
        rig.classifier.count(),
        1,
        "and the gateway is not asked again for the rest of the run"
    );
    assert_eq!(
        checks(&rig),
        Vec::new(),
        "a check that got no answer is not on the ledger"
    );

    // The limits still apply while the check is off, so an outage opens no flood.
    let rig = rig_triaging(
        "pick = \"all\"\nper_author_per_day = 1",
        SPAM,
        &["newcomer", "newcomer"],
    );
    rig.classifier.fails(ClassifierError::Refused(
        "`POST /classify` answered HTTP 503".to_owned(),
    ));
    let runner = rig.open().unwrap();

    for _ in 0..2 {
        assert_eq!(step(&runner).unwrap(), None);
        rig.next_look();
    }

    assert_eq!(
        calls(&rig, Role::Triager).len(),
        1,
        "the author's cap still holds their second issue"
    );
    assert_eq!(record(&rig, &runner, 31)["held"], "author-cap");
    assert_eq!(record(&rig, &runner, 30).get("spam"), None);
    assert!(
        (moves(&rig)).iter().all(|m| !m.contains(POSSIBLE_SPAM)),
        "and nothing was labelled possible-spam: {:?}",
        moves(&rig)
    );
}

#[test]
fn an_answer_kelpie_cannot_read_is_told_once_and_leaves_the_check_on() {
    // A gateway that answers the wrong thing is a broken route and not an outage:
    // the issue keeps its research call, and the next issue is still asked of the
    // classifier.
    let rig = rig_triaging("pick = \"all\"", SPAM, &["newcomer", "someone"]);
    rig.classifier.fails(ClassifierError::Unreadable(
        "no number in the answer".to_owned(),
    ));
    let runner = rig.open().unwrap();

    for _ in 0..2 {
        assert_eq!(step(&runner).unwrap(), None);
        rig.next_look();
    }

    assert_eq!(rig.classifier.count(), 2, "the check is asked again");
    assert_eq!(
        calls(&rig, Role::Triager).len(),
        2,
        "and both issues had their research call"
    );
    assert!(
        (moves(&rig)).iter().all(|m| !m.contains(POSSIBLE_SPAM)),
        "and nothing was labelled possible-spam: {:?}",
        moves(&rig)
    );
    assert_eq!(
        checks(&rig),
        Vec::new(),
        "a check that got no answer is not on the ledger"
    );
    let notes: Vec<String> = (told(&runner))
        .into_iter()
        .filter(|n| n.contains("cannot read"))
        .collect();
    assert_eq!(notes.len(), 1, "told once: {notes:?}");
    assert!(notes[0].contains("no number in the answer"), "{}", notes[0]);
    assert_eq!(record(&rig, &runner, 30).get("spam"), None);
}

#[test]
fn the_request_carries_the_cut_body_the_association_and_the_account_age() {
    let rig = rig_triaging("pick = \"all\"", SPAM, &[]);
    rig.classifier.says(0.1);
    let runner = rig.open().unwrap();
    let now = rig.clock.now().0;
    // Old enough that the account limit leaves it to the classifier
    rig.forge
        .set_account_created("newcomer", Timestamp(now - 40 * DAY));
    let mut crash = issue(&rig, 30, "newcomer", 5);
    crash.title = "Crash on start".to_owned();
    crash.author_association = Some(AuthorAssociation::FirstTimer);
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

    let asks: Vec<SeenAsk> = rig.classifier.asks();
    assert_eq!(asks.len(), 1);
    let asked = &asks[0];
    assert_eq!(asked.title, "Crash on start", "the title is never cut");
    assert!(asked.body.starts_with("HEAD-OF-BODY"));
    assert!(
        asked.body.len() <= 20 * 1024,
        "the body is cut to the size the call is given: {}",
        asked.body.len()
    );
    assert!(
        !asked.body.contains("END-OF-BODY"),
        "and nothing past the cut reaches it"
    );
    assert_eq!(
        asked.author_association,
        Some(AuthorAssociation::FirstTimer)
    );
    assert_eq!(
        asked.account_age_days,
        Some(40),
        "the account's age in days"
    );
    assert_eq!(asked.address, "gpu:8700", "the gateway the settings name");
    assert_eq!(asked.path, ROUTE, "on the route they name");

    // The classifier and the triager read the same issue.
    let prompt = (calls(&rig, Role::Triager)).remove(0).call.prompt;
    assert!(prompt.contains("HEAD-OF-BODY"));
    assert!(!prompt.contains("END-OF-BODY"));

    // An association and an age the forge cannot say are given as none, and the
    // issue is researched all the same.
    let rig = rig_triaging("pick = \"all\"", SPAM, &[]);
    rig.classifier.says(0.1);
    let runner = rig.open().unwrap();
    rig.forge.set_accounts_down(true);
    let mut unknown = issue(&rig, 30, "newcomer", 5);
    unknown.author_association = None;
    list(&rig, unknown);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);

    assert_eq!(step(&runner).unwrap(), None);

    let asks: Vec<SeenAsk> = rig.classifier.asks();
    assert_eq!(asks[0].author_association, None);
    assert_eq!(asks[0].account_age_days, None);
    assert_eq!(calls(&rig, Role::Triager).len(), 1, "and it is researched");
}

#[test]
fn the_maintainers_issues_and_the_verb_skip_the_check() {
    // The maintainer's own issue, which the classifier would read as spam
    let rig = rig_triaging(
        "pick = \"all\"",
        &format!("{SPAM}\nthreshold = 0.5"),
        &[MAINTAINER],
    );
    rig.classifier.says(0.99);
    let runner = rig.open().unwrap();

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        rig.classifier.count(),
        0,
        "no question about the maintainer's issue"
    );
    assert_eq!(calls(&rig, Role::Triager).len(), 1);
    assert_eq!(
        rig.github.comments(),
        [(30, format!("{WORDS}\n\n{MARKER}"))]
    );
    assert_eq!(record(&rig, &runner, 30).get("spam"), None);

    // An issue the verb asked for, from an author the classifier would read as
    // spam
    let rig = rig_triaging("pick = \"all\"", SPAM, &["newcomer"]);
    rig.classifier.says(0.99);
    let runner = rig.open().unwrap();

    let answer = rig.ask(&runner, "triage", Some("30"));
    assert_eq!(answer.get("error"), None, "{answer}");
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        rig.classifier.count(),
        0,
        "and the verb's issue is not asked about"
    );
    assert_eq!(calls(&rig, Role::Triager).len(), 1);
    assert_eq!(rig.github.comments(), [(30, handed(WORDS))]);
}

#[test]
fn an_issue_labelled_possible_spam_is_triaged_when_the_verb_asks() {
    let (rig, runner) = checked(0.99);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(record(&rig, &runner, 30)["spam"], 0.99);

    // The maintainer overrules the label by asking for the issue. Its pass skips
    // the check, the label kelpie put on comes off, and the triage it posts
    // replaces the record.
    let answer = rig.ask(&runner, "triage", Some("30"));
    assert_eq!(answer.get("error"), None, "{answer}");
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(rig.classifier.count(), 1, "the check is not asked again");
    assert_eq!(calls(&rig, Role::Triager).len(), 1);
    assert_eq!(rig.github.comments(), [(30, handed(WORDS))]);
    assert_eq!(record(&rig, &runner, 30).get("spam"), None);
    assert!(
        (written_on(&rig, 30))
            .iter()
            .any(|path| path.ends_with(&format!("/labels/{POSSIBLE_SPAM}"))),
        "the label kelpie read the issue as is taken off: {:?}",
        written_on(&rig, 30)
    );
}

#[test]
fn a_possible_spam_no_record_of_kelpies_says_it_read_stands_when_the_verb_asks() {
    // A `possible-spam` someone put on by hand is not kelpie's read to take off:
    // no record of kelpie's says it read the issue as spam.
    let rig = rig_triaging("pick = \"all\"", SPAM, &[]);
    let mut hand = issue(&rig, 30, "someone", 5);
    hand.labels = vec![POSSIBLE_SPAM.to_owned()];
    list(&rig, hand);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);
    let runner = rig.open().unwrap();

    let answer = rig.ask(&runner, "triage", Some("30"));
    assert_eq!(answer.get("error"), None, "{answer}");
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(rig.classifier.count(), 0, "the override skips the check");
    assert_eq!(
        calls(&rig, Role::Triager).len(),
        1,
        "and the issue is triaged"
    );
    assert_eq!(rig.github.comments(), [(30, handed(WORDS))]);
    assert!(
        !(written_on(&rig, 30))
            .iter()
            .any(|path| path.ends_with(&format!("/labels/{POSSIBLE_SPAM}"))),
        "and kelpie took nothing off: {:?}",
        written_on(&rig, 30)
    );
}

#[test]
fn an_answer_just_under_the_threshold_is_researched_and_not_labelled() {
    // 0.7996 read to the thousandth stands at 0.8 and would be spam at the
    // default threshold. It is held down to the millionth at or below it, so the
    // issue gets its research call and its comment.
    let rig = rig_triaging("pick = \"all\"", SPAM, &["newcomer"]);
    rig.classifier.says(0.7996);
    let runner = rig.open().unwrap();

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(rig.classifier.count(), 1);
    assert_eq!(
        calls(&rig, Role::Triager).len(),
        1,
        "the issue had its research call"
    );
    assert_eq!(
        rig.github.comments(),
        [(30, handed(WORDS))],
        "and its comment"
    );
    assert_eq!(record(&rig, &runner, 30).get("spam"), None);
    assert!(
        (written_on(&rig, 30))
            .iter()
            .all(|path| !path.contains(POSSIBLE_SPAM)),
        "and nothing was labelled possible-spam: {:?}",
        written_on(&rig, 30)
    );
}

#[test]
fn a_call_the_issue_made_before_it_was_read_as_spam_keeps_its_place() {
    // A research call that came to nothing is saved against the issue until a
    // record carries it. The spam record has to take it, or the caps lose the
    // place it spent.
    let rig = rig_triaging("pick = \"all\"", SPAM, &[]);
    list(&rig, issue(&rig, 30, "newcomer", 5));
    rig.classifier.answers(&[0.1, 0.9]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    rig.claude
        .script_for(Role::Triager, [Scripted::Text("not a triage reply")]);
    let runner = rig.open().unwrap();

    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(calls(&rig, Role::Triager).len(), 1, "the call ran");
    assert_eq!(record(&rig, &runner, 30), Value::Null, "and left no record");
    assert_eq!(triage(&rig, &runner)["daily"]["used"], 1);

    // A restart, and the classifier now reads the issue as spam.
    let runner = restart(&rig, runner);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(rig.classifier.count(), 2, "the check is asked again");
    let made = record(&rig, &runner, 30);
    assert_eq!(made["spam"], 0.9);
    assert_eq!(
        made["calls"].as_array().unwrap().len(),
        1,
        "the call it made before went into the record: {made}"
    );
    assert_eq!(
        triage(&rig, &runner)["daily"]["used"],
        1,
        "and the day's cap still counts it"
    );
    assert_eq!(
        calls(&rig, Role::Triager).len(),
        1,
        "while the check itself made no call"
    );
}

#[test]
fn each_check_is_on_the_ledger_as_a_spam_check() {
    let rig = rig_triaging("pick = \"all\"", SPAM, &["newcomer", "someone"]);
    rig.classifier.answers(&[0.9, 0.1]);
    let runner = rig.open().unwrap();

    for _ in 0..2 {
        assert_eq!(step(&runner).unwrap(), None);
        rig.next_look();
    }

    let lines = ledger(&rig);
    let checks: Vec<&CallLine> = lines
        .iter()
        .filter(|call| call.kind == CallKind::SpamCheck)
        .collect();
    assert_eq!(checks.len(), 2, "one line for each check");
    assert_eq!(
        checks.iter().map(|c| c.issue).collect::<Vec<_>>(),
        [Some(ticket(30)), Some(ticket(31))],
        "the issue it asked about, which is what a triager line names"
    );
    assert_eq!(
        (lines.iter())
            .filter(|c| c.kind == CallKind::Triage)
            .map(|c| c.issue)
            .collect::<Vec<_>>(),
        [Some(ticket(31))],
        "the research line of the issue that was researched names its own the same way"
    );
    assert!(
        checks.iter().all(|c| c.role == Role::Triager),
        "role `triager`"
    );
    assert!(
        checks
            .iter()
            .all(|c| c.units == 0 && c.usage == Usage::default()),
        "no tokens"
    );
    assert!(
        checks.iter().all(|c| c.cost_usd.is_none() && !c.unpriced),
        "and no cost"
    );
    assert!(
        checks
            .iter()
            .all(|c| c.session.is_none() && c.harness.is_none()),
        "which is no session and no harness"
    );
    assert_eq!(
        (lines.iter())
            .filter(|c| c.kind == CallKind::Triage)
            .count(),
        1,
        "the issue that was researched has its own line"
    );
    let text = std::fs::read_to_string(rig.paths().folder.join(usage::FILE)).unwrap();
    assert!(
        text.contains("\"kind\":\"spam-check\""),
        "the ledger writes it as `spam-check`"
    );
}

#[test]
fn a_check_costs_no_triage_place_of_the_day() {
    // The day has one place, and the issue read as spam never takes it.
    let rig = rig_triaging(
        "pick = \"all\"\nper_day = 1",
        &format!("{SPAM}\nthreshold = 0.5"),
        &["newcomer", "someone"],
    );
    rig.classifier.answers(&[0.5, 0.1]);
    let runner = rig.open().unwrap();

    for _ in 0..2 {
        assert_eq!(step(&runner).unwrap(), None);
        rig.next_look();
    }

    assert_eq!(
        calls(&rig, Role::Triager).len(),
        1,
        "only the issue under the threshold is called"
    );
    assert_eq!(triage(&rig, &runner)["daily"]["used"], 1);
    assert_eq!(record(&rig, &runner, 30)["spam"], 0.5);
    assert_eq!(due(&rig, &runner), Vec::<u64>::new());
}

#[test]
fn a_triage_limit_holds_the_issue_before_the_classifier_is_asked() {
    // The author's cap is spent, so their issue ends at `needs-triage` and the
    // classifier is never asked about it.
    let rig = rig_triaging(
        "pick = \"all\"\nper_author_per_day = 1",
        SPAM,
        &["newcomer"],
    );
    rig.classifier.says(0.99);
    let runner = rig.open().unwrap();
    let now = rig.clock.now().0;
    drop(runner);
    crashed_with(
        &rig,
        json!({
            "issue": 11,
            "comment": 11,
            "step": "done",
            "at": now,
            "author": "newcomer",
            "calls": [now],
        }),
    );
    let runner = rig.open().unwrap();

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(rig.classifier.count(), 0, "no question about a held issue");
    assert_eq!(record(&rig, &runner, 30)["held"], "author-cap");
    assert_eq!(record(&rig, &runner, 30).get("spam"), None);
    assert_eq!(moves(&rig), ["make needs-triage", "label needs-triage"]);
}

#[test]
fn a_spam_gateway_kelpie_does_not_have_keeps_the_runner_from_starting() {
    let rig = rig_with(&format!(
        "pick = \"all\"\n[app.dogs.kelpie.triage.spam]\n{SPAM}\n"
    ));
    rig.set_kelpie_settings(&format!(
        "[webhook]\nkind = \"ntfy\"\nurl = \"{}\"\n",
        Rig::WEBHOOK_URL
    ));
    let err = rig.open().unwrap_err().to_string();
    assert!(
        err.contains("triage.spam.gateway") && err.contains(GATEWAY),
        "{err}"
    );
}

#[test]
fn a_spam_gateway_whose_key_is_unset_starts_and_leaves_the_check_off() {
    // A gateway kelpie cannot ask is a broken check and not a broken runner: the
    // settings check lets it through, the issue goes on to its research call, and
    // it is told once.
    let rig = rig_with(
        "pick = \"all\"\n[app.dogs.kelpie.triage.spam]\ngateway = \"gw\"\npath = \"/classify\"\n",
    );
    rig.set_kelpie_settings(&format!(
        "[webhook]\nkind = \"ntfy\"\nurl = \"{}\"\n\n[gateways.gw]\nurl = \"http://gpu:8700\"\nkey_env = \"KELPIIE_NO_SUCH_KEY\"\n",
        Rig::WEBHOOK_URL
    ));
    list(&rig, issue(&rig, 30, "newcomer", 5));
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);
    rig.classifier.says(0.99);
    let runner = rig.open().unwrap();

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        rig.classifier.count(),
        0,
        "with no key there is nothing to ask"
    );
    assert_eq!(
        calls(&rig, Role::Triager).len(),
        1,
        "the issue still reaches its research call"
    );
    assert_eq!(record(&rig, &runner, 30).get("spam"), None);
    assert!(
        (moves(&rig)).iter().all(|m| !m.contains(POSSIBLE_SPAM)),
        "and nothing was labelled possible-spam: {:?}",
        moves(&rig)
    );
    let notes: Vec<String> = (told(&runner))
        .into_iter()
        .filter(|n| n.contains("spam check is off"))
        .collect();
    assert_eq!(notes.len(), 1, "told once: {notes:?}");
    assert!(notes[0].contains("KELPIIE_NO_SUCH_KEY"), "{}", notes[0]);
}

#[test]
fn a_check_that_failed_is_asked_again_at_the_next_start() {
    // The check is off for the run, not for good: a restart reads the gateway again.
    let rig = rig_triaging("pick = \"all\"", SPAM, &["newcomer"]);
    rig.classifier.fails(ClassifierError::Unreachable(
        "connection refused".to_owned(),
    ));
    let runner = rig.open().unwrap();

    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.classifier.count(), 1);

    let runner = restart(&rig, runner);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);
    list(&rig, issue(&rig, 31, "someone", 40));
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(rig.classifier.count(), 2, "the next start asks again");
    assert_eq!(calls(&rig, Role::Triager).len(), 2);
}

#[cfg(unix)]
#[test]
fn a_failed_save_takes_possible_spam_off_and_the_issue_is_read_again() {
    let (rig, runner) = checked(0.9);

    step_with_the_state_shut(&rig, &runner);

    assert_eq!(rig.classifier.count(), 1);
    assert_eq!(
        label_moves(&rig),
        [
            format!("make {POSSIBLE_SPAM}"),
            format!("label {POSSIBLE_SPAM}"),
            format!("unlabel {POSSIBLE_SPAM}"),
        ],
        "the label that no record backs came off"
    );
    assert_eq!(
        record(&rig, &runner, 30),
        Value::Null,
        "and no record was kept"
    );

    // The issue is untouched again, so the next look asks the classifier again.
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        rig.classifier.count(),
        2,
        "the classifier is asked a second time"
    );
    assert_eq!(
        label_moves(&rig)[3..],
        [
            format!("make {POSSIBLE_SPAM}"),
            format!("label {POSSIBLE_SPAM}")
        ],
        "and the issue is labelled"
    );
    assert_eq!(record(&rig, &runner, 30)["spam"], 0.9);
    assert_eq!(
        calls(&rig, Role::Triager).len(),
        0,
        "still no research call"
    );
}

#[cfg(unix)]
#[test]
fn a_failed_save_and_a_failed_unlabel_is_told_in_a_note() {
    let (rig, runner) = checked(0.9);
    rig.github.refuse_label_removals(ApiError::Refused(403));

    step_with_the_state_shut(&rig, &runner);

    assert_eq!(
        label_moves(&rig).last().map(String::as_str),
        Some("unlabel possible-spam"),
        "taking it off was tried"
    );
    assert_eq!(record(&rig, &runner, 30), Value::Null);
    let notes: Vec<String> = told(&runner)
        .into_iter()
        .filter(|n| n.contains("#30") && n.contains(POSSIBLE_SPAM))
        .collect();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].contains("403"),
        "the unlabel's error: {}",
        notes[0]
    );
}
