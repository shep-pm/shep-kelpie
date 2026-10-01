//! MCP servers kelpie runs outside an agent's sandbox, reached over a Unix socket
//!
//! The preview's browser cannot start inside the sandbox on macOS, so its
//! Playwright server and kelpie's shots tool run beside it. The agent starts
//! `kelpie mcp-connect <socket>` in their place, which carries its stdio to
//! the socket. Kelpie reads every message on its way to the server, because
//! anything inside the sandbox can reach the socket, not only the agent.
//! Only the MCP methods a tool call needs pass, and each tool call is judged
//! as `kelpie browse-guard` judges it.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use serde_json::{Value, json};

use crate::confine::Verdict;

/// What passes to a bridged server
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    /// The preview's domains, which a URL argument may name beside the dev server
    pub domains: Vec<String>,
    /// Tools no call may reach, as the server names them
    pub denied_tools: Vec<String>,
}

/// What becomes of one message on its way to a server
#[derive(Debug, Clone, PartialEq)]
pub enum Passage {
    /// It goes on, as it came
    Pass,
    /// It stops here, and this reply goes back, if it wants one
    Stop(Option<Value>),
}

impl Filter {
    /// Judges `line`, one JSON-RPC message from inside the sandbox
    pub fn judge(&self, line: &str) -> Passage {
        let Ok(Value::Object(message)) = serde_json::from_str::<Value>(line) else {
            return Passage::Stop(Some(error(
                &Value::Null,
                "kelpie passes one JSON object a line",
            )));
        };
        let id = message.get("id").cloned();
        let Some(method) = message.get("method") else {
            // A reply to a request the server made, such as for its roots
            return Passage::Pass;
        };
        let reply = |answer: Value| Passage::Stop(id.as_ref().map(|_| answer));
        let id = id.clone().unwrap_or(Value::Null);
        match method.as_str() {
            Some("initialize" | "ping" | "tools/list") => Passage::Pass,
            Some(note) if note.starts_with("notifications/") => Passage::Pass,
            Some("tools/call") => match self.tool_call(&message["params"]) {
                Verdict::Allow => Passage::Pass,
                Verdict::Refuse(why) => reply(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "content": [{ "type": "text", "text": why }], "isError": true },
                })),
            },
            _ => reply(error(&id, "kelpie does not pass this method to the server")),
        }
    }

    fn tool_call(&self, params: &Value) -> Verdict {
        let Some(name) = params["name"].as_str() else {
            return Verdict::Refuse("kelpie cannot read this tool call's name".into());
        };
        if self.denied_tools.iter().any(|t| t == name) {
            return Verdict::Refuse(format!(
                "{name} reaches past the preview, so kelpie refuses it"
            ));
        }
        let arguments = match &params["arguments"] {
            Value::Null => json!({}),
            other => other.clone(),
        };
        let call = json!({ "tool_input": arguments }).to_string();
        crate::browse::judge(call.as_bytes(), &self.domains)
    }
}

fn error(id: &Value, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": message } })
}

/// `kelpie mcp-connect <socket>`: carries stdin to `socket`, and what comes back to stdout
///
/// # Errors
///
/// The reason, when the socket cannot be reached.
pub fn connect(socket: &Path) -> Result<(), String> {
    let stream = UnixStream::connect(socket)
        .map_err(|e| format!("cannot reach {}: {e}", socket.display()))?;
    let mut to_server = stream
        .try_clone()
        .map_err(|e| format!("cannot use {}: {e}", socket.display()))?;
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut std::io::stdin().lock(), &mut to_server);
        let _ = to_server.shutdown(std::net::Shutdown::Write);
    });
    let mut from_server = BufReader::new(stream);
    let mut stdout = std::io::stdout().lock();
    let mut line = String::new();
    while from_server.read_line(&mut line).is_ok_and(|n| n > 0) {
        if stdout
            .write_all(line.as_bytes())
            .and_then(|()| stdout.flush())
            .is_err()
        {
            break;
        }
        line.clear();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter() -> Filter {
        Filter {
            domains: vec!["api.example.com".into()],
            denied_tools: vec!["browser_run_code_unsafe".into()],
        }
    }

    fn call(name: &str, arguments: Value) -> String {
        json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        })
        .to_string()
    }

    fn refusal(passage: Passage) -> String {
        match passage {
            Passage::Stop(Some(reply)) => {
                assert_eq!(reply["id"], 4);
                assert_eq!(reply["result"]["isError"], true, "{reply}");
                reply["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            }
            other => panic!("went through: {other:?}"),
        }
    }

    #[test]
    fn the_handshake_listing_and_a_preview_call_pass() {
        let f = filter();
        for line in [
            r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#,
            r#"{"jsonrpc":"2.0","id":9,"result":{"roots":[]}}"#,
        ] {
            assert_eq!(f.judge(line), Passage::Pass, "{line}");
        }
        let navigate = call(
            "browser_navigate",
            json!({ "url": "http://localhost:5273/" }),
        );
        assert_eq!(f.judge(&navigate), Passage::Pass);
        assert_eq!(
            f.judge(&call("shots", json!({ "routes": ["/"] }))),
            Passage::Pass
        );
        assert_eq!(
            f.judge(&call("browser_snapshot", Value::Null)),
            Passage::Pass
        );
    }

    // What a worker's own code could send down the socket, past Claude Code's hooks
    #[test]
    fn a_call_browse_guard_refuses_is_refused_on_the_wire() {
        let f = filter();
        for url in [
            "view-source:file:///Users/me/.kelpie/settings.toml",
            "file:///etc/hosts",
            "https://example.com/",
        ] {
            let why = refusal(f.judge(&call("browser_navigate", json!({ "url": url }))));
            assert!(why.contains("not the dev server"), "{why}");
        }
        let save = call(
            "browser_take_screenshot",
            json!({ "filename": "/Users/me/.zshrc" }),
        );
        assert!(refusal(f.judge(&save)).contains("filename"));
        let unsafe_code = call("browser_run_code_unsafe", json!({ "code": "1" }));
        assert!(refusal(f.judge(&unsafe_code)).contains("refuses"));
    }

    #[test]
    fn any_other_method_or_shape_is_refused() {
        let f = filter();
        let read = r#"{"jsonrpc":"2.0","id":3,"method":"resources/read","params":{"uri":"file:///etc/hosts"}}"#;
        let Passage::Stop(Some(reply)) = f.judge(read) else {
            panic!("resources/read went through");
        };
        assert_eq!(reply["id"], 3);
        assert_eq!(reply["error"]["code"], -32601);
        for line in [
            "not json",
            "[]",
            r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#,
        ] {
            assert!(matches!(f.judge(line), Passage::Stop(Some(_))), "{line}");
        }
        let quiet = r#"{"jsonrpc":"2.0","method":"sampling/createMessage"}"#;
        assert_eq!(f.judge(quiet), Passage::Stop(None));
    }
}
