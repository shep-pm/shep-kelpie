//! Throwaway dog side of transport test L.
//!
//! Subscribes to `channel.*` and `process.*` on a shepherd, watches runners
//! raise a `wants.gpu` running total, and grants one GPU lease at a time by
//! triggering `status` and then `grant gpu` on the runner. A runner's
//! `releases.gpu` total, or its exit, frees the lease for the next waiter.
//! Every step prints one JSON line with milliseconds since start.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::Instant;

use serde_json::{Value, json};
use shep_client::Client;
use shep_core::protocol::{Request, SelectorSpec};

struct Lease {
    holder: Option<u32>,
    queue: VecDeque<u32>,
    served: HashMap<u32, f64>,
    released: HashMap<u32, f64>,
    last_metric: HashMap<(u32, String), f64>,
}

fn log(start: Instant, event: &str, fields: Value) {
    let mut line = json!({ "t_ms": start.elapsed().as_millis() as u64, "event": event });
    if let (Some(line), Some(fields)) = (line.as_object_mut(), fields.as_object()) {
        for (k, v) in fields {
            line.insert(k.clone(), v.clone());
        }
    }
    println!("{line}");
}

async fn trigger(client: &Client, id: u32, action: &str, params: Option<&str>) -> (u128, String) {
    let t0 = Instant::now();
    let reply = client
        .request(Request::Trigger {
            selector: SelectorSpec::Id(id),
            action: action.to_owned(),
            params: params.map(str::to_owned),
        })
        .await;
    (t0.elapsed().as_millis(), format!("{reply:?}"))
}

async fn try_grant(client: &Client, lease: &mut Lease, start: Instant) {
    if lease.holder.is_some() {
        return;
    }
    let Some(id) = lease.queue.pop_front() else { return };
    let (status_ms, status) = trigger(client, id, "status", None).await;
    log(start, "status", json!({ "id": id, "rtt_ms": status_ms as u64, "reply": status }));
    let (grant_ms, grant) = trigger(client, id, "grant", Some("gpu")).await;
    lease.holder = Some(id);
    log(start, "granted", json!({ "id": id, "rtt_ms": grant_ms as u64, "reply": grant }));
}

/// Finds the first object anywhere in `v` that has every key in `keys`.
fn find<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a serde_json::Map<String, Value>> {
    match v {
        Value::Object(map) => {
            if keys.iter().all(|k| map.contains_key(*k)) {
                return Some(map);
            }
            map.values().find_map(|x| find(x, keys))
        }
        Value::Array(items) => items.iter().find_map(|x| find(x, keys)),
        _ => None,
    }
}

#[tokio::main]
async fn main() {
    let start = Instant::now();
    let home = std::env::var("SHEP_HOME").expect("SHEP_HOME is set");
    let socket = PathBuf::from(home).join("run/shep.sock");
    let events_client = Client::connect(&socket).await.expect("connect for events");
    let client = Client::connect(&socket).await.expect("connect for requests");
    let mut events = events_client
        .subscribe(vec!["channel.*".to_owned(), "process.*".to_owned()])
        .await
        .expect("subscribe");
    log(start, "subscribed", json!({}));

    let mut lease = Lease {
        holder: None,
        queue: VecDeque::new(),
        served: HashMap::new(),
        released: HashMap::new(),
        last_metric: HashMap::new(),
    };

    while let Some(item) = events.next().await {
        let event = match item {
            Ok(event) => event,
            Err(lagged) => {
                log(start, "lagged", json!({ "detail": format!("{lagged:?}") }));
                continue;
            }
        };
        let value = serde_json::to_value(&event).unwrap_or(Value::Null);
        let text = value.to_string();

        if text.contains("\"count\"") && text.to_lowercase().contains("dropped") {
            log(start, "bus_dropped", json!({ "raw": value }));
            continue;
        }

        // A runner's metric: channel event carrying { name, value }.
        if let Some(metric) = find(&value, &["name", "value"]) {
            let id = find(&value, &["id"]).and_then(|m| m["id"].as_u64()).unwrap_or(0) as u32;
            let name = metric["name"].as_str().unwrap_or("").to_owned();
            let v = metric["value"].as_f64().unwrap_or(0.0);
            lease.last_metric.insert((id, name.clone()), v);
            if name.starts_with("flood.") {
                if v as u64 % 1000 == 0 {
                    log(start, "flood_seen", json!({ "id": id, "value": v }));
                }
                continue;
            }
            log(start, "metric", json!({ "id": id, "name": name, "value": v }));
            if name == "wants.gpu" && v > *lease.served.get(&id).unwrap_or(&0.0) {
                lease.served.insert(id, v);
                if lease.holder != Some(id) && !lease.queue.contains(&id) {
                    lease.queue.push_back(id);
                }
                try_grant(&client, &mut lease, start).await;
            } else if name == "releases.gpu" && v > *lease.released.get(&id).unwrap_or(&0.0) {
                lease.released.insert(id, v);
                if lease.holder == Some(id) {
                    lease.holder = None;
                    log(start, "released", json!({ "id": id }));
                    try_grant(&client, &mut lease, start).await;
                }
            }
            continue;
        }

        // A process event: {"event":"process","data":{"event":<kind>,"info":{..}}}.
        if value["event"] == "process" {
            let kind = value["data"]["event"].as_str().unwrap_or("?").to_owned();
            let id = value["data"]["info"]["id"].as_u64().map(|x| x as u32);
            log(start, "process", json!({ "kind": kind, "id": id,
                "name": value["data"]["info"]["name"] }));
            match (kind.as_str(), id) {
                // A runner that starts again reports its totals from zero, so
                // its bookkeeping starts again too.
                ("start" | "restart" | "online", Some(id)) => {
                    lease.served.remove(&id);
                    lease.released.remove(&id);
                }
                ("exit" | "stop", Some(id)) if lease.holder == Some(id) => {
                    lease.holder = None;
                    log(start, "reclaimed", json!({ "id": id }));
                    try_grant(&client, &mut lease, start).await;
                }
                _ => {}
            }
        } else if value["event"] != "channel" {
            log(start, "other", json!({ "raw": text.chars().take(200).collect::<String>() }));
        }
    }
    log(start, "stream_ended", json!({ "flood_last": lease.last_metric.iter()
        .filter(|((_, n), _)| n.starts_with("flood."))
        .map(|((id, n), v)| json!({ "id": id, "name": n, "value": v }))
        .collect::<Vec<_>>() }));
}
