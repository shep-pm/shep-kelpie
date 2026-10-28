//! The project manager's answer: the last JSON object in its reply
//!
//! Each field is read on its own, so a malformed one is dropped and named
//! while the rest still count. Nothing here knows the board: checking an
//! answer against it is the runner's.

use serde_json::{Map, Value};

use super::picked::{PICK, Picked, picked};
use crate::board::{READY, TRIAGE};
use crate::ticket::TicketId;

/// The keys an answer takes, an object with none of them being none: `why`
/// is left out, since an unstick inside an answer has one of its own
const KEYS: [&str; 9] = [
    "pick",
    "hold",
    "unstick",
    "answer",
    "follow_up",
    "red",
    "votes",
    "standing",
    "reply",
];

/// The most of a reply or a why kept, in characters
const LONGEST: usize = 2_000;

/// What a `red` entry holds, as a dropped one names it
const RED: &str = "`workflow`, a `call` of fix with a `title` and a `body`, or ask with the \
                   `log` lines you read, and `why`";

/// What an `unstick` entry holds, as a dropped one names it
const UNSTICK: &str = "`item`, an `action` of retry, re-scope, ask or none, and `why`";

/// What the project manager answered, field by field
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Answer {
    /// The ready issues to start, in order, or none to start nothing now
    pub(super) picks: Vec<Picked>,
    /// The ready issues to hold, each until the open work item it names closes
    /// or, naming none, until any closes. None leaves the holds as they stand,
    /// and an empty list clears them.
    pub(super) hold: Option<Vec<HoldCall>>,
    /// What to do about each stuck work item the wake carried
    pub(super) unstick: Vec<Unstick>,
    /// What to do about one worker's question
    pub(super) answer: Option<Settle>,
    /// What to do about one follow-up ruling's findings
    pub(super) follow_up: Option<Decide>,
    /// What to do about each workflow still red on `main` after its rerun
    pub(super) red: Vec<RedCall>,
    /// Its answers to the rulings left to it: each ruling, and the object it
    /// answered with. The key keeps its old name for the project manager's
    /// session.
    pub(super) votes: Vec<(TicketId, Map<String, Value>)>,
    /// The whole of its standing notes, which replaces the file kelpie holds.
    /// None leaves the file as it is, and empty text clears it.
    pub(super) standing: Option<String>,
    /// A message for the maintainer
    pub(super) reply: Option<String>,
    /// Why, in its own words
    pub(super) why: Option<String>,
    /// What could not be read, each named, to log
    pub(super) unread: Vec<String>,
}

/// What a `hold` entry holds, as a dropped one names it
const HOLD: &str =
    "a ticket id, or an object with an `issue` and the open work item it waits on as `until`";

/// The project manager's call to hold one ready issue back
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HoldCall {
    /// The ready issue
    pub(super) issue: TicketId,
    /// The open work item it waits on, or none for any work item closing
    pub(super) until: Option<TicketId>,
}

/// The project manager's call on one workflow red on `main` after its rerun
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RedCall {
    /// The workflow's name
    pub(super) workflow: String,
    /// Whether it opens a fix or asks the maintainer
    pub(super) how: RedHow,
    /// Why, in its own words
    pub(super) why: String,
}

/// What the project manager does about a workflow red after its rerun
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RedHow {
    /// Opens a fix work item at the front of the board: the issue's title and
    /// body, which name the failing check, the log lines and the suspect merge
    Fix { title: String, body: String },
    /// Asks the maintainer, because it cannot tell a flake from a break, with
    /// the log lines it read
    Ask { log: String },
}

/// The project manager's call on one stuck work item
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Unstick {
    /// The work item's ticket
    pub(super) item: TicketId,
    /// What to do
    pub(super) action: Action,
    /// Why, which a ruling it raises carries
    pub(super) why: String,
}

/// The project manager's call on one worker's question
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Settle {
    /// The work item's ticket, parked on the question's ruling
    pub(super) item: TicketId,
    /// Whether it answers or passes the question on
    pub(super) how: How,
    /// Why, in its own words
    pub(super) why: String,
    /// The problem in its own plain words, which replaces the worker's when it
    /// passes the question on
    pub(super) problem: Option<String>,
    /// What the board shows of picking an option, when it passes the question on
    pub(super) board: Option<String>,
    /// Candidate answers it drafts for a question that lists fewer than two
    /// options, each on one line, when it passes the question on
    pub(super) candidates: Vec<String>,
}

/// The project manager's call on one follow-up ruling
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Decide {
    /// The work item's ticket, parked on the follow-up ruling
    pub(super) item: TicketId,
    /// Whether the findings are filed or dropped
    pub(super) call: Call,
    /// Why, in its own words
    pub(super) why: String,
}

/// What the project manager decides about a follow-up ruling's findings
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Call {
    /// Filed as issues, labelled `needs-triage` if `unread` and `ready-for-agent` if not
    File { unread: bool },
    /// Dropped, and nothing is filed
    Drop,
}

/// How the project manager settles a worker's question
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum How {
    /// The worker's next turn, which the maintainer's answer would have been
    Text(String),
    /// The maintainer decides, with this choice recommended if it names one
    Escalate(Option<String>),
}

/// What the project manager does about a stuck work item
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    /// Its turn runs again as it is
    Retry,
    /// The maintainer is asked to change what it asks
    ReScope,
    /// The maintainer decides
    Ask,
    /// Left to resolve itself
    Leave,
}

impl Action {
    /// The action as the answer spells it
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::ReScope => "re-scope",
            Self::Ask => "ask",
            Self::Leave => "none",
        }
    }
}

/// The answer in `text`: its last JSON object with any of the answer's keys
pub(super) fn read(text: &str) -> Option<Answer> {
    let fields = last_object(text)?;
    let mut answer = Answer::default();
    for (key, value) in fields {
        let unread = |what: &str| format!("`{key}`: {what}, not {value}");
        match key.as_str() {
            "pick" => match &value {
                Value::Null => {}
                // One issue is a ticket; several, a list of them in the order to start.
                Value::Array(items) => {
                    for item in items {
                        match picked(item) {
                            Some(p) if answer.picks.iter().any(|q| q.issue == p.issue) => answer
                                .unread
                                .push(format!("`pick`: {} twice", p.issue.shown())),
                            Some(p) => answer.picks.push(p),
                            None => answer.unread.push(format!("`pick`: {PICK}, not {item}")),
                        }
                    }
                }
                value => match picked(value) {
                    Some(p) => answer.picks.push(p),
                    None => answer
                        .unread
                        .push(unread("a ticket id, a list of them or null")),
                },
            },
            "hold" => match &value {
                Value::Null => {}
                Value::Array(items) => {
                    let holds = answer.hold.get_or_insert_with(Vec::new);
                    for item in items {
                        match hold(item) {
                            Some(h) => holds.push(h),
                            None => answer.unread.push(format!("`hold`: {HOLD}, not {item}")),
                        }
                    }
                }
                _ => answer.unread.push(unread("a list of holds")),
            },
            "unstick" => match &value {
                Value::Null => {}
                // One stuck item is one object; several are a list of them.
                Value::Array(items) => {
                    for item in items {
                        match unstick(item) {
                            Some(u) => answer.unstick.push(u),
                            None => answer
                                .unread
                                .push(format!("`unstick`: {UNSTICK}, not {item}")),
                        }
                    }
                }
                value => match unstick(value) {
                    Some(u) => answer.unstick.push(u),
                    None => answer.unread.push(unread(UNSTICK)),
                },
            },
            "answer" => match &value {
                Value::Null => {}
                value => match settle(value) {
                    Some(s) => answer.answer = Some(s),
                    None => answer
                        .unread
                        .push(unread("`item`, then `text` or `escalate`, and `why`")),
                },
            },
            "follow_up" => match &value {
                Value::Null => {}
                value => match decide(value) {
                    Some(d) => answer.follow_up = Some(d),
                    None => answer.unread.push(unread(
                        "`item`, a `decision` of yes or no, a `label` of ready-for-agent or \
                         needs-triage with a yes, and `why`",
                    )),
                },
            },
            "red" => match &value {
                Value::Null => {}
                // One workflow is one object; several are a list of them.
                Value::Array(items) => {
                    for item in items {
                        match red(item) {
                            Some(r) => answer.red.push(r),
                            None => answer.unread.push(format!("`red`: {RED}, not {item}")),
                        }
                    }
                }
                value => match red(value) {
                    Some(r) => answer.red.push(r),
                    None => answer.unread.push(unread(RED)),
                },
            },
            "votes" => match &value {
                Value::Null => {}
                Value::Array(items) => {
                    for item in items {
                        let vote = item.as_object().and_then(|fields| {
                            Some((ticket(fields.get("ruling")?)?, fields.clone()))
                        });
                        match vote {
                            Some(vote) => answer.votes.push(vote),
                            None => answer
                                .unread
                                .push(format!("`votes`: a `ruling` and its vote, not {item}")),
                        }
                    }
                }
                _ => answer.unread.push(unread("a list of votes")),
            },
            "standing" => match value {
                Value::Null => {}
                Value::String(text) => answer.standing = Some(plain(text.trim())),
                _ => answer.unread.push(unread("text or null")),
            },
            "reply" | "why" => match value {
                Value::Null => {}
                Value::String(text) if text.trim().is_empty() => {}
                Value::String(text) => {
                    let text = Some(printable(text.trim()));
                    match key.as_str() {
                        "reply" => answer.reply = text,
                        _ => answer.why = text,
                    }
                }
                _ => answer.unread.push(unread("text or null")),
            },
            _ => answer
                .unread
                .push(format!("`{key}` is not a field kelpie acts on")),
        }
    }
    Some(answer)
}

// The last top-level object in `text` that has an answer's key. Read from
// the start, each whole JSON value is skipped past, so nothing inside one,
// a string's braces included, is ever taken for an answer of its own.
fn last_object(text: &str) -> Option<Map<String, Value>> {
    let mut found = None;
    let mut at = 0;
    while let Some(offset) = text[at..].find('{') {
        let start = at + offset;
        let mut values = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
        match values.next() {
            Some(Ok(value)) => {
                at = start + values.byte_offset();
                if let Value::Object(fields) = value
                    && KEYS.iter().any(|k| fields.contains_key(*k))
                {
                    found = Some(fields);
                }
            }
            _ => at = start + 1,
        }
    }
    found
}

// `text` with every control character but a newline taken out, cut past
// [`LONGEST`] characters, since it is printed and posted as it is.
fn printable(text: &str) -> String {
    let kept = plain(text);
    match kept.char_indices().nth(LONGEST) {
        Some((at, _)) => format!("{} …", &kept[..at]),
        None => kept,
    }
}

// `text` with every control character but a newline taken out, whole, since
// the standing notes are checked against a limit of their own.
fn plain(text: &str) -> String {
    text.chars()
        .filter(|&c| c == '\n' || !c.is_control())
        .collect()
}

// A ticket id, as a JSON number or as text: `9`, `#9` or `k12`.
pub(super) fn ticket(value: &Value) -> Option<TicketId> {
    match value {
        Value::Number(n) => TicketId::from_issue(n.as_u64()?),
        Value::String(text) => text.trim().trim_start_matches('#').parse::<TicketId>().ok(),
        _ => None,
    }
}

// A bare ticket id holds until any work item closes, as answers always read.
fn hold(value: &Value) -> Option<HoldCall> {
    if let Some(issue) = ticket(value) {
        return Some(HoldCall { issue, until: None });
    }
    let issue = ticket(value.get("issue")?)?;
    let until = match value.get("until") {
        None | Some(Value::Null) => None,
        Some(until) => Some(ticket(until)?),
    };
    Some(HoldCall { issue, until })
}

fn unstick(value: &Value) -> Option<Unstick> {
    let item = ticket(value.get("item")?)?;
    let action = match value.get("action")?.as_str()? {
        "retry" => Action::Retry,
        "re-scope" => Action::ReScope,
        "ask" => Action::Ask,
        "none" => Action::Leave,
        _ => return None,
    };
    let why = value.get("why").and_then(Value::as_str).unwrap_or("");
    Some(Unstick {
        item,
        action,
        why: why.trim().to_owned(),
    })
}

fn red(value: &Value) -> Option<RedCall> {
    let words = |key: &str| {
        let words = value.get(key)?.as_str()?.trim();
        (!words.is_empty()).then(|| words.to_owned())
    };
    let workflow = words("workflow")?;
    let how = match value.get("call")?.as_str()?.trim() {
        "fix" => RedHow::Fix {
            title: plain_line(&words("title")?),
            body: printable(&words("body")?),
        },
        "ask" => RedHow::Ask {
            log: printable(&words("log")?),
        },
        _ => return None,
    };
    let why = value.get("why").and_then(Value::as_str).unwrap_or("");
    Some(RedCall {
        workflow,
        how,
        why: why.trim().to_owned(),
    })
}

// `text` on one line, since it is an issue's title
fn plain_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn settle(value: &Value) -> Option<Settle> {
    let item = ticket(value.get("item")?)?;
    let text = value.get("text").filter(|v| !v.is_null());
    let escalate = value.get("escalate").filter(|v| !v.is_null());
    let candidates: Vec<String> = match value.get("candidates") {
        Some(Value::Array(items)) => (items.iter())
            .filter_map(Value::as_str)
            .map(|c| printable(c.trim()))
            .filter(|c| !c.is_empty())
            .collect(),
        _ => Vec::new(),
    };
    let how = match (text, escalate) {
        (Some(Value::String(text)), None) => match printable(text.trim()) {
            text if text.is_empty() => return None,
            text => How::Text(text),
        },
        (None, Some(Value::Bool(true))) => How::Escalate(None),
        (None, Some(Value::String(choice))) => {
            How::Escalate(Some(choice.trim().to_owned()).filter(|c| !c.is_empty()))
        }
        // Candidates alone pass the question on, with nothing recommended.
        (None, None) if !candidates.is_empty() => How::Escalate(None),
        _ => return None,
    };
    let why = value.get("why").and_then(Value::as_str).unwrap_or("");
    let words = |key: &str| {
        let words = value.get(key).and_then(Value::as_str)?.trim();
        (!words.is_empty()).then(|| words.to_owned())
    };
    Some(Settle {
        item,
        how,
        why: why.trim().to_owned(),
        problem: words("problem"),
        board: words("board"),
        candidates,
    })
}

fn decide(value: &Value) -> Option<Decide> {
    let item = ticket(value.get("item")?)?;
    let label = match value.get("label") {
        None | Some(Value::Null) => None,
        Some(label) => Some(label.as_str()?.trim().to_ascii_lowercase()),
    };
    let call = match value.get("decision")?.as_str()?.trim() {
        yes if yes.eq_ignore_ascii_case("yes") => match label.as_deref() {
            None | Some(READY) => Call::File { unread: false },
            Some(TRIAGE) => Call::File { unread: true },
            Some(_) => return None,
        },
        no if no.eq_ignore_ascii_case("no") && label.is_none() => Call::Drop,
        _ => return None,
    };
    let why = value.get("why").and_then(Value::as_str).unwrap_or("");
    Some(Decide {
        item,
        call,
        why: why.trim().to_owned(),
    })
}

#[cfg(test)]
mod tests {
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
                standing: None,
                reply: None,
                why: Some("#12 touches nothing open.".into()),
                unread: Vec::new(),
            }
        );
    }

    #[test]
    fn an_answer_takes_text_or_an_escalation_and_nothing_else() {
        let text =
            read("{\"answer\": {\"item\": \"#7\", \"text\": \" Use it. \", \"why\": \"r\"}}");
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
        let escalate =
            read("{\"answer\": {\"item\": 7, \"escalate\": \"--check\", \"why\": \"r\"}}");
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
        let text = settle(
            "{\"answer\": {\"item\": 7, \"text\": \"Go.\", \"candidates\": [\"a\", \"b\"]}}",
        );
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
            call(
                "{\"follow_up\": {\"item\": 7, \"decision\": \"yes\", \"label\": \"needs-triage\"}}"
            ),
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
        let one =
            read("{\"unstick\": {\"item\": 7, \"action\": \"retry\", \"why\": \"an outage\"}}")
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
}
