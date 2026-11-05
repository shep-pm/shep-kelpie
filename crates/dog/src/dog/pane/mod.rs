//! The board in lookout: the pane the dog answers `shep.pane` with
//!
//! `on_action` handlers are sync and the dog's client is async, so `Seen` is a
//! cache a task refreshes from each runner's `status`, and the draw only reads
//! it. The two actions the pane offers, [`rule`](ACTIONS) on a ruling's row and
//! `pause`, are done by that task: it looks at the runners afresh, sends the
//! triggers, and the handler waits for what came of them.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use shep_client::ReconnectingClient;
use shep_client::dogs::{Destructive, Pane, Row};
use shep_client::shep_core::protocol::request::{ActionOutcome, Response};
use shep_client::shep_core::protocol::{Request, SelectorSpec};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::MissedTickBehavior;

use crate::shepherd::Connection;

/// The actions the dog answers for the pane, beside the maintainer's triggers
pub const ACTIONS: [&str; 2] = ["rule", "pause"];

/// How often the cache is read again from the runners
const LOOK_TICK: Duration = Duration::from_secs(10);

/// How long a handler waits for the task to say what the runners did
///
/// The handler holds the channel's reader thread while it waits.
const ACT_PATIENCE: Duration = Duration::from_secs(20);

/// What each runner's `status` last said, by project
#[derive(Debug, Default, Clone)]
pub(super) struct Seen {
    runners: BTreeMap<String, Value>,
    /// The kelpie sheep whose `status` could not be read, and why
    unread: BTreeMap<String, String>,
}

/// The cache, shared between the task that fills it and the handlers that read it
pub(super) type Shared = Arc<Mutex<Seen>>;

/// A trigger to send a runner
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Forward {
    project: String,
    action: &'static str,
    params: Option<String>,
}

/// A pane action a handler hands the task, and where the task puts its answer
pub(super) struct Pending {
    action: String,
    params: Option<String>,
    /// When the handler gives up: an action not begun by then is never begun
    by: Instant,
    done: std_mpsc::Sender<String>,
}

/// What a runner made of a trigger: nothing to say, or why it did not take it
type Sent = (String, Result<(), String>);

/// The cache held, whether or not a handler panicked holding it
pub(super) fn lock(seen: &Shared) -> MutexGuard<'_, Seen> {
    seen.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Seen {
    /// Every runner of kelpie's, read for `status` all at once
    ///
    /// # Errors
    ///
    /// A message when the shepherd cannot say which sheep are kelpie's.
    pub(super) async fn look(client: &Arc<dyn Connection>) -> Result<Self, String> {
        let dog = super::NAME.to_owned();
        let reply = client.request(Request::DogSheepSettings { dog }).await;
        let projects = match reply {
            Ok(Response::DogSheepSettings { tables }) => tables,
            Ok(other) => return Err(format!("the sheep's tables came back as {other:?}")),
            Err(e) => return Err(format!("cannot read the sheep's tables: {e}")),
        };
        let asks: Vec<_> = projects
            .into_keys()
            .map(|project| Forward {
                project,
                action: "status",
                params: None,
            })
            .collect();
        let projects: Vec<_> = asks.iter().map(|ask| ask.project.clone()).collect();
        let mut seen = Self::default();
        for (project, said) in projects.into_iter().zip(trigger_all(client, asks).await) {
            match said.and_then(status_in) {
                Ok(status) => seen.runners.insert(project, status).map(drop),
                Err(why) => seen.unread.insert(project, why).map(drop),
            };
        }
        Ok(seen)
    }

    // Every runner the pane knows of, one it could not read too: a drain is tried on each.
    fn projects(&self) -> BTreeSet<&String> {
        self.runners.keys().chain(self.unread.keys()).collect()
    }
}

// A runner that is down, or answers with a refusal, is not a status the pane can draw.
fn status_in(said: ActionOutcome) -> Result<Value, String> {
    let ActionOutcome::Replied { body } = said else {
        return Err(format!("{said:?}"));
    };
    let status: Value = serde_json::from_str(&body).map_err(|_| body.clone())?;
    match (status["error"].as_str(), status["project"].is_string()) {
        (Some(why), _) => Err(why.to_owned()),
        (None, true) => Ok(status),
        (None, false) => Err(format!("answered `status` with {body}")),
    }
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}

fn list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value[key].as_array().map_or(&[], Vec::as_slice)
}

// Whether the runner picks from its board, is finishing, or drains its calls.
fn phase_of(status: &Value) -> &str {
    match (status.get("draining"), status["run"].as_str()) {
        (Some(_), _) => "draining",
        (None, Some(run)) => run,
        (None, None) => "picking",
    }
}

/// What lookout draws: each runner's slots and phase, its work items and its rulings
///
/// A row is keyed `<project>#<id>`: two projects can each have an item or a ruling `71`.
pub(super) fn build(seen: &Seen) -> Pane {
    let mut board = Vec::new();
    let (mut items, mut rulings) = (Vec::new(), Vec::new());
    for (project, status) in &seen.runners {
        let slots = format!(
            "{} of {}",
            list(status, "working").len(),
            status["active_items"]
        );
        board.push((format!("{project} slots"), slots));
        board.push((format!("{project} phase"), phase_of(status).to_owned()));
        for item in list(status, "work_items") {
            let issue = text(item, "issue");
            items.push(Row::new(
                format!("{project}#{issue}"),
                [
                    project,
                    issue,
                    text(item, "title"),
                    text(&item["phase"], "state"),
                    text(item, "agent"),
                ],
            ));
        }
        for ruling in list(status, "rulings") {
            let id = text(ruling, "id");
            let question = text(ruling, "question").lines().next().unwrap_or_default();
            rulings.push(Row::new(
                format!("{project}#{id}"),
                [project, id, text(&ruling["kind"], "kind"), question],
            ));
        }
    }
    Pane::new()
        .fields("board", board)
        .table(
            "work items",
            ["PROJECT", "ISSUE", "TITLE", "PHASE", "AGENT"],
            items,
        )
        .table("rulings", ["PROJECT", "ID", "KIND", "QUESTION"], rulings)
        .row_action("rule", "Say yes", Destructive::Yes)
        .action("pause", "Drain every runner", Destructive::Yes)
}

/// Plans a pane action: the JSON body it answers if every trigger is taken, and the triggers
///
/// `rule` takes a row of the rulings table, `{"row": "<project>#<id>"}`, and says yes to
/// that ruling: a row carries no text, so `no`, `rework` and an answer stay on `shep
/// kelpie rule`. `pause` sends every runner seen `drain`. A refusal has no triggers.
pub(super) fn act(seen: &Seen, action: &str, params: Option<&str>) -> (String, Vec<Forward>) {
    let refused = |why: String| (json!({ "error": why }).to_string(), Vec::new());
    match action {
        "rule" => match ruling_of(seen, params) {
            Ok((row, project, id)) => {
                let forward = Forward {
                    project,
                    action: "rule",
                    params: Some(format!("{id} yes")),
                };
                let body = json!({ "rule": row, "answer": "yes" });
                (body.to_string(), vec![forward])
            }
            Err(why) => refused(why),
        },
        "pause" => {
            let projects = seen.projects();
            let forwards = (projects.iter())
                .map(|project| Forward {
                    project: (*project).clone(),
                    action: "drain",
                    params: None,
                })
                .collect::<Vec<_>>();
            let body = json!({ "draining": projects });
            (body.to_string(), forwards)
        }
        other => refused(format!("unknown action `{other}`")),
    }
}

// The row's project and ruling id, when the runner that cache holds lists the ruling:
// an id the runner did not give never reaches it as part of a command.
fn ruling_of(seen: &Seen, params: Option<&str>) -> Result<(String, String, String), String> {
    let unread = || "`rule` takes a row of the rulings table, as `{\"row\": \"<project>#<id>\"}`";
    let params: Value = serde_json::from_str(params.ok_or_else(unread)?).map_err(|_| unread())?;
    let row = params["row"].as_str().ok_or_else(unread)?;
    let (project, id) = row.rsplit_once('#').ok_or_else(unread)?;
    if let Some(why) = seen.unread.get(project) {
        return Err(format!("{project}'s status could not be read: {why}"));
    }
    let status =
        (seen.runners.get(project)).ok_or_else(|| format!("no runner of {project} is seen"))?;
    list(status, "rulings")
        .iter()
        .any(|ruling| text(ruling, "id") == id)
        .then(|| (row.to_owned(), project.to_owned(), id.to_owned()))
        .ok_or_else(|| format!("{project} has no waiting ruling {id}: it may be answered"))
}

/// Sends every trigger at once, and returns what each runner said, in the order given
///
/// Each waits on the shep client's own deadline, so the slowest bounds them all.
async fn trigger_all(
    client: &Arc<dyn Connection>,
    triggers: Vec<Forward>,
) -> Vec<Result<ActionOutcome, String>> {
    let mut sent = JoinSet::new();
    let count = triggers.len();
    for (at, trigger) in triggers.into_iter().enumerate() {
        let client = Arc::clone(client);
        sent.spawn(async move {
            let request = Request::Trigger {
                selector: SelectorSpec::Name(trigger.project),
                action: trigger.action.into(),
                params: trigger.params,
            };
            let said = match client.request(request).await {
                Ok(Response::Triggered(rows)) => (rows.into_iter().next())
                    .map(|row| row.outcome)
                    .ok_or_else(|| "no such runner is running".to_owned()),
                Ok(other) => Err(format!("{other:?}")),
                Err(e) => Err(e.to_string()),
            };
            (at, said)
        });
    }
    let mut said: Vec<Option<Result<ActionOutcome, String>>> = (0..count).map(|_| None).collect();
    while let Some(done) = sent.join_next().await {
        if let Ok((at, one)) = done {
            said[at] = Some(one);
        }
    }
    (said.into_iter())
        .map(|one| one.unwrap_or_else(|| Err("the request ended without an answer".to_owned())))
        .collect()
}

/// Sends each trigger, and says for each what the runner made of it
pub(super) async fn forward(client: &Arc<dyn Connection>, forwards: Vec<Forward>) -> Vec<Sent> {
    let named: Vec<_> = (forwards.iter())
        .map(|f| (f.project.clone(), f.action))
        .collect();
    let said = trigger_all(client, forwards).await;
    let mut sent = Vec::new();
    for ((project, action), said) in named.into_iter().zip(said) {
        let taken = said.and_then(taken);
        if let Err(why) = &taken {
            println!("pane: {action} to {project} was not taken: {why}");
        }
        sent.push((project, taken));
    }
    sent
}

// A runner refuses with `{"error": ...}`; the shepherd says when it had no one to ask.
fn taken(outcome: ActionOutcome) -> Result<(), String> {
    let ActionOutcome::Replied { body } = outcome else {
        return Err(format!("{outcome:?}"));
    };
    match serde_json::from_str::<Value>(&body) {
        Ok(reply) => match reply["error"].as_str() {
            Some(why) => Err(why.to_owned()),
            None => Ok(()),
        },
        Err(_) => Err(body),
    }
}

// The body to answer once the triggers are sent: `body` as planned only if each was taken.
fn settle(action: &str, body: String, sent: Vec<Sent>) -> String {
    let refused = |why: String| json!({ "error": why }).to_string();
    match (action, sent.as_slice()) {
        ("rule", [(project, Err(why))]) => {
            refused(format!("{project} did not take the yes: {why}"))
        }
        ("pause", _) => {
            let (taken, failed): (Vec<_>, Vec<_>) = sent.into_iter().partition(|(_, r)| r.is_ok());
            let mut answer =
                json!({ "draining": taken.into_iter().map(|(p, _)| p).collect::<Vec<_>>() });
            if !failed.is_empty() {
                let why = failed
                    .into_iter()
                    .filter_map(|(p, r)| r.err().map(|e| (p, Value::from(e))));
                answer["failed"] = why.collect::<serde_json::Map<_, _>>().into();
            }
            answer.to_string()
        }
        _ => body,
    }
}

/// Does a pane action against the runners as they are now, and says what came of it
///
/// The runners are read afresh first, so a runner started since the last look is
/// drained too, and a ruling answered since is refused by name. Nothing is sent once
/// `by` has passed, since the handler that asked has given up and said so.
pub(super) async fn perform(
    client: &Arc<dyn Connection>,
    action: &str,
    params: Option<&str>,
    by: Instant,
) -> String {
    let late = || {
        let why = format!("`{action}` was not begun in time, and was not sent");
        json!({ "error": why }).to_string()
    };
    if Instant::now() >= by {
        return late();
    }
    let seen = match Seen::look(client).await {
        Ok(seen) => seen,
        Err(why) => return json!({ "error": why }).to_string(),
    };
    if Instant::now() >= by {
        return late();
    }
    let (body, forwards) = act(&seen, action, params);
    settle(action, body, forward(client, forwards).await)
}

/// What a handler answers for a pane action: the task's word, or that it gave none in time
pub(super) fn ask(
    asks: &mpsc::UnboundedSender<Pending>,
    action: &str,
    params: Option<&str>,
) -> String {
    let (done, answered) = std_mpsc::channel();
    let pending = Pending {
        action: action.to_owned(),
        params: params.map(str::to_owned),
        by: Instant::now() + ACT_PATIENCE,
        done,
    };
    let said = asks
        .send(pending)
        .ok()
        .and_then(|()| answered.recv_timeout(ACT_PATIENCE).ok());
    said.unwrap_or_else(|| {
        let why = format!(
            "the runners did not answer in {}s: `{action}` may still go through",
            ACT_PATIENCE.as_secs()
        );
        json!({ "error": why }).to_string()
    })
}

// The cache is replaced by a fresh look, or left as it was when the look fails.
async fn refresh(client: &Arc<dyn Connection>, seen: &Shared) {
    match Seen::look(client).await {
        Ok(looked) => *lock(seen) = looked,
        Err(e) => println!("pane: {e}"),
    }
}

/// Starts the two tasks of the pane: one keeps `seen` fresh, one does what the handlers ask
///
/// They are apart, so a slow read of the runners never holds an action back. The cache
/// is read again every [`LOOK_TICK`], and at once after an action, so the pane shows
/// what the runner then did.
pub(super) fn begin(
    client: Arc<ReconnectingClient>,
    seen: Shared,
) -> mpsc::UnboundedSender<Pending> {
    let client: Arc<dyn Connection> = client;
    let (send, mut asked) = mpsc::unbounded_channel::<Pending>();
    let (reading, shared) = (Arc::clone(&client), Arc::clone(&seen));
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(LOOK_TICK);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            refresh(&reading, &shared).await;
        }
    });
    tokio::spawn(async move {
        while let Some(Pending {
            action,
            params,
            by,
            done,
        }) = asked.recv().await
        {
            let said = perform(&client, &action, params.as_deref(), by).await;
            let _ = done.send(said);
            refresh(&client, &seen).await;
        }
    });
    send
}

#[cfg(test)]
mod tests;
