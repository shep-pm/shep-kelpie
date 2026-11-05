//! The board in lookout: the pane the dog answers `shep.pane` with
//!
//! `on_action` handlers are sync and the dog's client is async, so [`Seen`] is a
//! cache a task refreshes from each runner's `status`, and a handler only reads
//! it. The two actions the pane offers, [`rule`](ACTIONS) on a ruling's row and
//! `pause`, forward to the runners as triggers, off the handler's thread.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use shep_client::ReconnectingClient;
use shep_client::dogs::{Destructive, Pane, Row};
use shep_client::shep_core::protocol::request::{ActionOutcome, Response};
use shep_client::shep_core::protocol::{Request, SelectorSpec};
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

use crate::shepherd::Connection;

/// The actions the dog answers for the pane, beside the maintainer's triggers
pub const ACTIONS: [&str; 2] = ["rule", "pause"];

/// How often the cache is read again from the runners
const LOOK_TICK: Duration = Duration::from_secs(10);

/// What each runner's `status` last said, by project
#[derive(Debug, Default, Clone)]
pub(super) struct Seen {
    runners: BTreeMap<String, Value>,
}

/// The cache, shared between the task that fills it and the handlers that read it
pub(super) type Shared = Arc<Mutex<Seen>>;

/// A trigger to send a runner, for a handler that cannot wait on the shepherd
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Forward {
    project: String,
    action: &'static str,
    params: Option<String>,
}

/// The cache held, whether or not a handler panicked holding it
pub(super) fn lock(seen: &Shared) -> MutexGuard<'_, Seen> {
    seen.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Seen {
    /// Every runner of kelpie's that answers `status` now
    ///
    /// # Errors
    ///
    /// A message when the shepherd cannot say which sheep are kelpie's.
    pub(super) async fn look(client: &dyn Connection) -> Result<Self, String> {
        let dog = super::NAME.to_owned();
        let reply = client.request(Request::DogSheepSettings { dog }).await;
        let projects = match reply {
            Ok(Response::DogSheepSettings { tables }) => tables,
            Ok(other) => return Err(format!("the sheep's tables came back as {other:?}")),
            Err(e) => return Err(format!("cannot read the sheep's tables: {e}")),
        };
        let mut runners = BTreeMap::new();
        for project in projects.into_keys() {
            if let Some(status) = status_of(client, &project).await {
                runners.insert(project, status);
            }
        }
        Ok(Self { runners })
    }
}

// A runner that is down, or answers with a refusal, is left out of the pane.
async fn status_of(client: &dyn Connection, project: &str) -> Option<Value> {
    let request = Request::Trigger {
        selector: SelectorSpec::Name(project.to_owned()),
        action: "status".into(),
        params: None,
    };
    let Ok(Response::Triggered(rows)) = client.request(request).await else {
        return None;
    };
    let Some(ActionOutcome::Replied { body }) = rows.into_iter().next().map(|row| row.outcome)
    else {
        return None;
    };
    let status: Value = serde_json::from_str(&body).ok()?;
    status["project"].is_string().then_some(status)
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

/// Answers a pane action with a JSON body, and the triggers it sends the runners
///
/// `rule` takes a row of the rulings table, `{"row": "<project>#<id>"}`, and says yes to
/// that ruling: a row carries no text, so `no`, `rework` and an answer stay on `shep
/// kelpie rule`. `pause` sends every runner seen `drain`.
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
            let forwards = (seen.runners.keys())
                .map(|project| Forward {
                    project: project.clone(),
                    action: "drain",
                    params: None,
                })
                .collect::<Vec<_>>();
            let body = json!({ "draining": seen.runners.keys().collect::<Vec<_>>() });
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
    let status =
        (seen.runners.get(project)).ok_or_else(|| format!("no runner of {project} is seen"))?;
    list(status, "rulings")
        .iter()
        .any(|ruling| text(ruling, "id") == id)
        .then(|| (row.to_owned(), project.to_owned(), id.to_owned()))
        .ok_or_else(|| format!("{project} has no waiting ruling {id}: it may be answered"))
}

/// Sends each trigger, and says what came back of it
pub(super) async fn forward(client: &dyn Connection, forwards: Vec<Forward>) {
    for Forward {
        project,
        action,
        params,
    } in forwards
    {
        let request = Request::Trigger {
            selector: SelectorSpec::Name(project.clone()),
            action: action.into(),
            params,
        };
        let outcome = match client.request(request).await {
            Ok(Response::Triggered(rows)) => format!("{:?}", rows.first().map(|r| &r.outcome)),
            Ok(other) => format!("{other:?}"),
            Err(e) => e.to_string(),
        };
        println!("pane: sent {action} to {project}: {outcome}");
    }
}

/// Starts the task that keeps `seen` fresh and sends what the handlers forward
///
/// The cache is read again every [`LOOK_TICK`], and at once after a forward, so the
/// pane shows what the runner then did.
pub(super) fn begin(
    client: Arc<ReconnectingClient>,
    seen: Shared,
) -> mpsc::UnboundedSender<Forward> {
    let (send, mut sent) = mpsc::unbounded_channel::<Forward>();
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(LOOK_TICK);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = ticks.tick() => {}
                Some(first) = sent.recv() => {
                    let mut forwards = vec![first];
                    while let Ok(more) = sent.try_recv() {
                        forwards.push(more);
                    }
                    forward(&*client, forwards).await;
                }
            }
            match Seen::look(&*client).await {
                Ok(looked) => *lock(&seen) = looked,
                Err(e) => println!("pane: {e}"),
            }
        }
    });
    send
}

#[cfg(test)]
mod tests;
