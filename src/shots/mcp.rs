//! `kelpie shots-mcp <tools> <job>`: the worker's shots tool, as an MCP server
//!
//! Claude Code starts it from the worker's `--mcp-config`, outside the
//! sandbox, which Chromium cannot start inside. It speaks MCP's stdio
//! transport: one JSON-RPC message a line. Its one tool takes the routes the
//! worker touched, keeps them for kelpie's own runs, and runs the job on them.

use std::io::{BufRead, Write};
use std::path::Path;

use serde_json::{Value, json};

use super::{ShotsJob, name_routes};
use crate::ports::Shots;
use crate::preview::Route;

/// The tool's name, which Claude Code shows as `mcp__kelpie__shots`
pub const TOOL: &str = "shots";

/// Where the worker's own shots go, under the job's folder
const WORKER_OUT: &str = "worker";

/// Answers MCP requests from `input` on `output` until `input` ends
///
/// # Errors
///
/// The reason, when the job file cannot be read or `output` cannot be written.
pub fn serve(
    job: &Path,
    shots: &dyn Shots,
    input: impl BufRead,
    mut output: impl Write,
) -> Result<(), String> {
    let text = std::fs::read_to_string(job)
        .map_err(|e| format!("cannot read {}: {}", job.display(), e.kind()))?;
    let job: ShotsJob =
        serde_json::from_str(&text).map_err(|e| format!("cannot parse {}: {e}", job.display()))?;
    for line in input.lines() {
        let line = line.map_err(|e| format!("cannot read a request: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let Some(reply) = answer(&job, shots, &line) else {
            continue;
        };
        writeln!(output, "{reply}")
            .and_then(|()| output.flush())
            .map_err(|e| format!("cannot answer: {e}"))?;
    }
    Ok(())
}

// The reply to one message, or none for a notification
fn answer(job: &ShotsJob, shots: &dyn Shots, line: &str) -> Option<Value> {
    let Ok(message) = serde_json::from_str::<Value>(line) else {
        return Some(error(Value::Null, -32700, "not JSON"));
    };
    let id = message.get("id").cloned()?;
    let params = &message["params"];
    let result = match message["method"].as_str().unwrap_or_default() {
        "initialize" => json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "kelpie", "version": env!("CARGO_PKG_VERSION") },
        }),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": [tool()] }),
        "tools/call" if params["name"] == TOOL => call(job, shots, &params["arguments"]),
        "tools/call" => return Some(error(id, -32602, "no such tool")),
        _ => return Some(error(id, -32601, "no such method")),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn tool() -> Value {
    json!({
        "name": TOOL,
        "description": "Starts the launch file's dev server and captures each route at a \
            phone and a desktop width, light and dark. Returns the PNG paths to open \
            with Read, and whatever failed on each page, such as a request to a domain \
            the project does not list. Name every route your change touched: kelpie \
            captures them again before each review.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "routes": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Paths on the dev server, each starting with /, such as /events",
                },
            },
            "required": ["routes"],
        },
    })
}

fn call(job: &ShotsJob, shots: &dyn Shots, arguments: &Value) -> Value {
    let routes: Result<Vec<Route>, String> = arguments["routes"]
        .as_array()
        .map(|all| {
            all.iter()
                .map(|r| {
                    let r = r.as_str().unwrap_or_default().to_owned();
                    Route::try_from(r).map_err(|e| e.to_string())
                })
                .collect()
        })
        .unwrap_or_else(|| Err("`routes` must be a list of paths".into()));
    let routes = match routes {
        Ok(routes) if !routes.is_empty() => routes,
        Ok(_) => return failed("name at least one route"),
        Err(e) => return failed(&e),
    };
    if let Err(e) = name_routes(&job.out, &routes) {
        return failed(&e);
    }
    let out = job.out.join(WORKER_OUT);
    let _ = std::fs::remove_dir_all(&out);
    let run = shots.take(&ShotsJob {
        out,
        routes,
        ..job.clone()
    });
    json!({
        "content": [{ "type": "text", "text": run.text() }],
        "isError": run.failed.is_some(),
    })
}

fn failed(reason: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": reason }], "isError": true })
}

fn error(id: Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use super::*;
    use crate::shots::{ShotsRun, named_routes, plan};

    #[derive(Debug, Default)]
    struct Recorder(Mutex<Vec<ShotsJob>>);

    impl Shots for Recorder {
        fn stop_left(&self, _server_pid: &Path) {}

        fn take(&self, job: &ShotsJob) -> ShotsRun {
            self.0.lock().unwrap().push(job.clone());
            ShotsRun {
                shots: plan(&job.routes, &job.out),
                ..ShotsRun::default()
            }
        }
    }

    fn session(requests: &[Value]) -> (tempfile::TempDir, Recorder, Vec<Value>) {
        let dir = tempfile::tempdir().unwrap();
        let job = ShotsJob {
            worktree: dir.path().join("wt"),
            build: dir.path().join("build"),
            out: dir.path().join("shots"),
            launch: Err("not started in this test".into()),
            routes: vec![Route::try_from("/".to_owned()).unwrap()],
            domains: vec![],
            env: BTreeMap::new(),
            server_pid: dir.path().join("shots/dev-server.pid"),
            shep_home: dir.path().join("shep"),
        };
        let file = dir.path().join("job.json");
        std::fs::write(&file, serde_json::to_string(&job).unwrap()).unwrap();
        let input: String = requests.iter().map(|r| format!("{r}\n")).collect();
        let recorder = Recorder::default();
        let mut output = Vec::new();
        serve(&file, &recorder, input.as_bytes(), &mut output).unwrap();
        let replies = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        (dir, recorder, replies)
    }

    #[test]
    fn it_introduces_itself_lists_its_tool_and_ignores_notifications() {
        let (_dir, _, replies) = session(&[
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        ]);
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0]["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(replies[0]["result"]["capabilities"], json!({ "tools": {} }));
        assert_eq!(replies[1]["id"], 2);
        assert_eq!(replies[1]["result"]["tools"][0]["name"], "shots");
    }

    #[test]
    fn a_call_captures_the_named_routes_and_keeps_them_for_kelpie() {
        let (dir, recorder, replies) = session(&[json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "shots", "arguments": { "routes": ["/events"] } },
        })]);
        let jobs = recorder.0.lock().unwrap().clone();
        let [job] = jobs.try_into().unwrap();
        assert_eq!(job.routes, [Route::try_from("/events".to_owned()).unwrap()]);
        assert_eq!(job.out, dir.path().join("shots/worker"));
        let text = replies[0]["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("/events at mobile, light: "), "{text}");
        assert_eq!(replies[0]["result"]["isError"], false);
        assert_eq!(
            named_routes(&dir.path().join("shots")),
            [Route::try_from("/events".to_owned()).unwrap()]
        );
    }

    #[test]
    fn a_route_that_is_not_a_path_is_refused_before_anything_runs() {
        let (_dir, recorder, replies) = session(&[json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": "shots", "arguments": { "routes": ["https://evil.example"] } },
        })]);
        assert!(recorder.0.lock().unwrap().is_empty());
        assert_eq!(replies[0]["result"]["isError"], true);
    }

    #[test]
    fn a_call_naming_no_route_runs_nothing() {
        let (_dir, recorder, replies) = session(&[json!({
            "jsonrpc": "2.0", "id": 6, "method": "tools/call",
            "params": { "name": "shots", "arguments": { "routes": [] } },
        })]);
        assert!(recorder.0.lock().unwrap().is_empty());
        assert_eq!(
            replies[0]["result"]["content"][0]["text"],
            "name at least one route"
        );
    }

    #[test]
    fn an_unknown_method_is_an_error_not_silence() {
        let (_dir, _, replies) =
            session(&[json!({"jsonrpc": "2.0", "id": 5, "method": "resources/list"})]);
        assert_eq!(replies[0]["error"]["code"], -32601);
    }
}
