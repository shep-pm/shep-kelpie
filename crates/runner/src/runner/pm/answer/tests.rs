//! The project manager's answer, read

use super::*;
use crate::test::ticket;

#[test]
fn the_last_object_after_the_reasoning_is_the_answer() {
    let text = "I read the board. {\"pick\": 3} was my first thought.\n\n```json\n\
                {\"pick\": 12, \"hold\": [4, \"#9\", {\"issue\": 5, \"until\": \"#3\"}], \"unstick\": {\"item\": 7, \
                \"action\": \"re-scope\", \"why\": \"split the parser out\"}, \
                \"reply\": null, \"why\": \"#12 touches nothing open.\"}\n```\n";
    let answer = read(text).unwrap();
    assert_eq!(
        answer,
        Answer {
            picks: vec![Picked::of(ticket(12))],
            hold: Some(vec![
                HoldCall {
                    issue: ticket(4),
                    until: None
                },
                HoldCall {
                    issue: ticket(9),
                    until: None
                },
                HoldCall {
                    issue: ticket(5),
                    until: Some(ticket(3))
                },
            ]),
            unstick: vec![Unstick {
                item: ticket(7),
                action: Action::ReScope,
                why: "split the parser out".into(),
            }],
            answer: None,
            follow_up: None,
            red: Vec::new(),
            votes: Vec::new(),
            said: Vec::new(),
            read: Vec::new(),
            standing: None,
            reply: None,
            why: Some("#12 touches nothing open.".into()),
            unread: Vec::new(),
        }
    );
}

#[test]
fn a_said_entry_is_read_and_a_malformed_one_is_named() {
    use super::super::said::SaidHow;
    let answer = read(
        "{\"said\": [{\"thread\": 71, \"call\": \"send-back\", \"note\": \" Rename it. \", \
         \"why\": \"asked\"}, {\"thread\": \"#72\", \"call\": \"none\"}, \
         {\"thread\": 73, \"call\": \"merge\"}, {\"call\": \"park\"}]}",
    )
    .unwrap();

    let calls: Vec<_> = (answer.said.iter())
        .map(|s| (s.thread, s.call, s.note.as_str(), s.why.as_str()))
        .collect();
    assert_eq!(
        calls,
        [
            (71, SaidHow::SendBack, "Rename it.", "asked"),
            (72, SaidHow::Nothing, "", ""),
        ]
    );
    assert_eq!(answer.unread.len(), 2, "{:?}", answer.unread);
    assert!(
        answer.unread[0].contains("\"call\":\"merge\""),
        "{:?}",
        answer.unread
    );
    let one = read("{\"said\": {\"thread\": 5, \"call\": \"settle\"}}").unwrap();
    assert_eq!(one.said[0].call, SaidHow::Settle);
}

#[test]
fn a_read_entry_with_no_item_or_no_more_is_named_and_dropped() {
    let answer = read(
        "{\"read\": [{\"item\": 12, \"more\": true, \"why\": \" the race is real \"}, \
         {\"item\": \"#13\", \"more\": false}, {\"more\": true}, {\"item\": 14}, \
         {\"item\": 15, \"more\": \"yes\"}]}",
    )
    .unwrap();

    let calls: Vec<_> = (answer.read.iter())
        .map(|r| (r.item, r.more, r.why.as_str()))
        .collect();
    assert_eq!(
        calls,
        [
            (ticket(12), true, "the race is real"),
            (ticket(13), false, "")
        ]
    );
    assert_eq!(answer.unread.len(), 3, "{:?}", answer.unread);
    assert!(
        answer.unread[0].contains("{\"more\":true}"),
        "{:?}",
        answer.unread
    );
    let one = read("{\"read\": {\"item\": 5, \"more\": true}}").unwrap();
    assert_eq!(one.read.len(), 1);
}

#[test]
fn an_answer_takes_text_or_an_escalation_and_nothing_else() {
    let text = read("{\"answer\": {\"item\": \"#7\", \"text\": \" Use it. \", \"why\": \"r\"}}");
    assert_eq!(
        text.unwrap().answer,
        Some(Settle {
            item: ticket(7),
            how: How::Text("Use it.".into()),
            why: "r".into(),
            problem: None,
            board: None,
            candidates: Vec::new(),
        })
    );
    let passed = read(
        "{\"answer\": {\"item\": 7, \"escalate\": \"--check\", \"problem\": \" The flag is \
         unnamed. \", \"board\": \"#9 waits behind it.\", \"why\": \"r\"}}",
    );
    let passed = passed.unwrap().answer.unwrap();
    assert_eq!(passed.problem.as_deref(), Some("The flag is unnamed."));
    assert_eq!(passed.board.as_deref(), Some("#9 waits behind it."));
    let escalate = read("{\"answer\": {\"item\": 7, \"escalate\": \"--check\", \"why\": \"r\"}}");
    assert_eq!(
        escalate.unwrap().answer.map(|a| a.how),
        Some(How::Escalate(Some("--check".into())))
    );
    let bare = read("{\"answer\": {\"item\": 7, \"escalate\": true}}");
    assert_eq!(
        bare.unwrap().answer.map(|a| a.how),
        Some(How::Escalate(None))
    );
    for bad in [
        "{\"answer\": {\"item\": 7, \"text\": \"a\", \"escalate\": \"b\"}}",
        "{\"answer\": {\"item\": 7}}",
        "{\"answer\": {\"item\": 7, \"text\": \"  \"}}",
        "{\"answer\": {\"text\": \"a\"}}",
    ] {
        let read = read(bad).unwrap();
        assert_eq!((read.answer, read.unread.len()), (None, 1), "{bad}");
    }
}

#[test]
fn candidates_pass_a_question_on_with_or_without_a_recommendation() {
    let settle = |text: &str| read(text).unwrap().answer.unwrap();
    let drafted = settle(
        "{\"answer\": {\"item\": 7, \"escalate\": true, \"candidates\": [\" Use it. \", \"\", 4, \
         \"Ask the maintainer\"]}}",
    );
    assert_eq!(drafted.how, How::Escalate(None));
    assert_eq!(drafted.candidates, ["Use it.", "Ask the maintainer"]);
    let alone = settle("{\"answer\": {\"item\": 7, \"candidates\": [\"a\", \"b\"]}}");
    assert_eq!(alone.how, How::Escalate(None));
    let text =
        settle("{\"answer\": {\"item\": 7, \"text\": \"Go.\", \"candidates\": [\"a\", \"b\"]}}");
    assert_eq!(text.how, How::Text("Go.".into()));
    let none = read("{\"answer\": {\"item\": 7, \"candidates\": []}}").unwrap();
    assert_eq!((none.answer, none.unread.len()), (None, 1));
}

#[test]
fn a_follow_up_answer_is_a_yes_or_a_no_and_a_yes_may_name_its_label() {
    let call = |text: &str| {
        read(text)
            .unwrap()
            .follow_up
            .map(|d| (d.item, d.call, d.why))
    };
    assert_eq!(
        call("{\"follow_up\": {\"item\": \"#7\", \"decision\": \" Yes \", \"why\": \" r \"}}"),
        Some((ticket(7), Call::File { unread: false }, "r".into()))
    );
    assert_eq!(
        call("{\"follow_up\": {\"item\": 7, \"decision\": \"yes\", \"label\": \"needs-triage\"}}"),
        Some((ticket(7), Call::File { unread: true }, String::new()))
    );
    assert_eq!(
        call(
            "{\"follow_up\": {\"item\": 7, \"decision\": \"yes\", \"label\": \"ready-for-agent\"}}"
        ),
        Some((ticket(7), Call::File { unread: false }, String::new()))
    );
    assert_eq!(
        call("{\"follow_up\": {\"item\": 7, \"decision\": \"no\", \"why\": \"fixed\"}}"),
        Some((ticket(7), Call::Drop, "fixed".into()))
    );
    for bad in [
        "{\"follow_up\": {\"item\": 7, \"decision\": \"maybe\"}}",
        "{\"follow_up\": {\"item\": 7}}",
        "{\"follow_up\": {\"decision\": \"yes\"}}",
        "{\"follow_up\": {\"item\": 7, \"decision\": \"yes\", \"label\": \"bug\"}}",
        "{\"follow_up\": {\"item\": 7, \"decision\": \"no\", \"label\": \"needs-triage\"}}",
    ] {
        let read = read(bad).unwrap();
        assert_eq!((read.follow_up, read.unread.len()), (None, 1), "{bad}");
    }
}

#[test]
fn braces_inside_the_answers_text_are_not_an_answer_of_their_own() {
    let text = "{\"pick\": 12, \"why\": \"not {\\\"pick\\\": 3} as before\"} {unfinished";
    assert_eq!(read(text).unwrap().picks, [Picked::of(ticket(12))]);
    let inner = "{\"reply\": \"{\\\"pick\\\": 3}\", \"pick\": null}";
    assert_eq!(read(inner).unwrap().picks, Vec::new());
}

#[test]
fn a_pick_is_one_ticket_or_a_list_of_them_in_order() {
    let issues = |answer: Answer| {
        answer
            .picks
            .into_iter()
            .map(|p| p.issue)
            .collect::<Vec<_>>()
    };
    let list = read("{\"pick\": [14, \"#12\", 9]}").unwrap();
    assert_eq!(issues(list), [ticket(14), ticket(12), ticket(9)]);
    let one = read("{\"pick\": [12]}").unwrap();
    assert_eq!(issues(one), [ticket(12)]);
    assert_eq!(read("{\"pick\": []}").unwrap().picks, Vec::new());

    let bad = read("{\"pick\": [12, true, 12]}").unwrap();
    assert_eq!(
        bad.unread,
        [
            format!("`pick`: {PICK}, not true"),
            "`pick`: #12 twice".into()
        ]
    );
    assert_eq!(issues(bad), [ticket(12)]);
}

#[test]
fn a_reply_loses_its_control_characters_and_keeps_its_lines() {
    let text = "{\"reply\": \"\\u001b[31mRed\\u0007\\nDone.\"}";
    assert_eq!(read(text).unwrap().reply.as_deref(), Some("[31mRed\nDone."));
}

#[test]
fn a_reply_with_no_answer_object_has_no_answer() {
    assert_eq!(read("I would pick #12."), None);
    assert_eq!(read("{\"item\": 7}"), None);
    assert_eq!(read("{\"pick\": 12"), None);
}

#[test]
fn a_red_call_is_a_fix_or_an_ask_and_a_bad_one_is_named_while_the_rest_count() {
    let text = "{\"pick\": null, \"red\": [{\"workflow\": \"test\", \"call\": \"fix\", \
                \"title\": \"Fix e2e\", \"body\": \"It fails.\", \"why\": \"a break\"}, \
                {\"workflow\": \"lint\", \"call\": \"ask\", \"log\": \"error: x\", \
                \"why\": \"unclear\"}, {\"workflow\": \"coverage\", \"call\": \"fix\"}]}";
    let answer = read(text).unwrap();
    assert_eq!(
        answer.red,
        [
            RedCall {
                workflow: "test".into(),
                how: RedHow::Fix {
                    title: "Fix e2e".into(),
                    body: "It fails.".into(),
                },
                why: "a break".into(),
            },
            RedCall {
                workflow: "lint".into(),
                how: RedHow::Ask {
                    log: "error: x".into(),
                },
                why: "unclear".into(),
            },
        ]
    );
    assert_eq!(answer.unread.len(), 1);
    assert!(
        answer.unread[0].starts_with("`red`:"),
        "{:?}",
        answer.unread
    );
}

#[test]
fn votes_name_their_ruling_and_a_bad_one_is_named_while_the_rest_count() {
    let text = "{\"pick\": null, \"votes\": [{\"ruling\": 4, \"choice\": \"A\", \
                \"why\": \"CI is green\", \"design\": false}, {\"ruling\": \"#5\", \
                \"choice\": \"B\"}, {\"choice\": \"A\"}, 7]}";
    let answer = read(text).unwrap();
    let rulings: Vec<TicketId> = answer.votes.iter().map(|(id, _)| *id).collect();
    assert_eq!(rulings, [ticket(4), ticket(5)]);
    assert_eq!(
        crate::pm_answer::from_fields(&answer.votes[0].1, 2).map(|v| v.choice),
        Ok('A')
    );
    assert_eq!(answer.unread.len(), 2, "{:?}", answer.unread);
    let bad = read("{\"votes\": \"A\"}").unwrap();
    assert_eq!(bad.unread, ["`votes`: a list of votes, not \"A\""]);
}

#[test]
fn unstick_takes_one_object_or_a_list_of_them_and_a_bad_entry_is_named_alone() {
    let one = read("{\"unstick\": {\"item\": 7, \"action\": \"retry\", \"why\": \"an outage\"}}")
        .unwrap();
    let many = read(
        "{\"unstick\": [{\"item\": 7, \"action\": \"retry\", \"why\": \"an outage\"}, \
         {\"item\": 9, \"action\": \"none\"}, {\"item\": 11, \"action\": \"reboot\"}]}",
    )
    .unwrap();
    let items: Vec<TicketId> = (many.unstick.iter()).map(|u| u.item).collect();
    assert_eq!(items, [ticket(7), ticket(9)]);
    assert_eq!(
        many.unstick[1],
        Unstick {
            item: ticket(9),
            action: Action::Leave,
            why: String::new(),
        }
    );
    assert_eq!(
        many.unread,
        [
            "`unstick`: `item`, an `action` of retry, re-scope, ask or none, and `why`, not \
         {\"action\":\"reboot\",\"item\":11}"
        ]
    );
    assert_eq!(one.unstick.len(), 1, "one object alone is read");
    assert_eq!(read("{\"unstick\": []}").unwrap().unstick, Vec::new());
    assert_eq!(read("{\"unstick\": null}").unwrap().unstick, Vec::new());
}

#[test]
fn a_malformed_field_is_named_and_the_rest_still_count() {
    let text = "{\"pick\": \"soon\", \"hold\": [5, true], \"unstick\": {\"item\": 7, \
                \"action\": \"reboot\"}, \"merge_order\": [71], \"reply\": \"On it.\"}";
    let answer = read(text).unwrap();
    assert_eq!(
        (
            &answer.picks,
            &answer.hold,
            &answer.unstick,
            answer.reply.as_deref()
        ),
        (
            &Vec::new(),
            &Some(vec![HoldCall {
                issue: ticket(5),
                until: None
            }]),
            &Vec::new(),
            Some("On it.")
        )
    );
    assert_eq!(
        answer.unread,
        [
            "`hold`: a ticket id, or an object with an `issue` and the open work item it waits on as \
             `until`, not true",
            "`merge_order` is not a field kelpie acts on",
            "`pick`: a ticket id, a list of them or null, not \"soon\"",
            "`unstick`: `item`, an `action` of retry, re-scope, ask or none, and `why`, \
             not {\"action\":\"reboot\",\"item\":7}",
        ]
    );
}

#[test]
fn a_hold_is_a_ticket_or_an_issue_with_what_it_waits_on() {
    let text = "{\"hold\": [7, {\"issue\": 8, \"until\": 3}, {\"issue\": 9, \"until\": null}, \
                {\"issue\": 10}, {\"issue\": 11, \"until\": \"soon\"}, {\"until\": 3}]}";
    let answer = read(text).unwrap();
    let hold = |issue: u64, until: Option<u64>| HoldCall {
        issue: ticket(issue),
        until: until.map(ticket),
    };
    assert_eq!(
        answer.hold,
        Some(vec![
            hold(7, None),
            hold(8, Some(3)),
            hold(9, None),
            hold(10, None)
        ])
    );
    assert_eq!(answer.unread.len(), 2, "{:?}", answer.unread);
    assert!(answer.unread[0].ends_with("not {\"issue\":11,\"until\":\"soon\"}"));
}

#[test]
fn no_hold_leaves_the_holds_and_an_empty_list_clears_them() {
    assert_eq!(read("{\"pick\": 12}").unwrap().hold, None);
    assert_eq!(read("{\"pick\": 12, \"hold\": null}").unwrap().hold, None);
    assert_eq!(read("{\"hold\": []}").unwrap().hold, Some(Vec::new()));
}

#[test]
fn a_standing_answer_is_the_whole_file_or_null_and_an_unknown_key_is_still_named() {
    let set = read("{\"standing\": \"Hold #12\\nuntil the release.  \"}").unwrap();
    assert_eq!(
        set.standing.as_deref(),
        Some("Hold #12\nuntil the release.")
    );
    assert_eq!(set.unread, Vec::<String>::new());

    let left = read("{\"pick\": null, \"standing\": null}").unwrap();
    assert_eq!(
        (left.picks, left.standing, left.unread),
        (vec![], None, vec![])
    );

    let long = "x".repeat(5_000);
    let over = read(&format!("{{\"standing\": \"{long}\"}}")).unwrap();
    assert_eq!(
        over.standing.as_deref(),
        Some(long.as_str()),
        "the whole text reaches the file's own limit"
    );

    let cleared = read("{\"standing\": \"  \\n \"}").unwrap();
    assert_eq!(
        cleared.standing.as_deref(),
        Some(""),
        "empty text is a whole file with nothing in it"
    );

    let bad = read("{\"pick\": 3, \"standing\": 4, \"merge_order\": [1]}").unwrap();
    assert_eq!(
        (bad.picks, bad.standing),
        (vec![Picked::of(ticket(3))], None)
    );
    assert_eq!(
        bad.unread,
        [
            "`merge_order` is not a field kelpie acts on",
            "`standing`: text or null, not 4",
        ]
    );
}
