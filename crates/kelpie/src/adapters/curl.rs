//! The webhook port over `curl`
//!
//! The URL and the body reach curl as a config on its stdin, never as
//! arguments, so no process listing shows the URL. Curl's own messages are
//! dropped: an error names only curl's exit code or the HTTP status.

use std::io::Write;
use std::process::{Command, Stdio};

use crate::ports::{Alert, AlertError, Alerts};
use crate::webhook::{Webhook, WebhookKind};

// Discord refuses a message over 2,000 characters, and ntfy turns a body
// over 4,096 bytes into an attachment. Counting bytes keeps under both.
const DISCORD_MAX: usize = 2000;
const NTFY_MAX: usize = 4096;

/// How much of a cut text's end is kept, where a ruling's triggers are
const KEEP_TAIL: usize = 400;

/// What stands in for the middle of a cut text
const CUT: &str = "\n[…cut; the whole question is in status]\n";

// `fit` needs room for a head beside the tail and the cut.
const _: () = assert!(DISCORD_MAX > KEEP_TAIL + CUT.len() && NTFY_MAX > DISCORD_MAX);

/// Seconds curl gets to connect, and to finish the whole post
const CONNECT_TIMEOUT: u32 = 5;
const MAX_TIME: u32 = 15;

/// Posts alerts with the system's `curl`
#[derive(Debug, Clone, Copy, Default)]
pub struct Curl;

impl Alerts for Curl {
    fn post(&self, webhook: &Webhook, alert: &Alert) -> Result<(), AlertError> {
        let mut child = Command::new("curl")
            .args(["--config", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| AlertError::Spawn(e.to_string()))?;
        let config = config(webhook, alert);
        let mut stdin = child.stdin.take().expect("stdin was piped");
        let written = stdin.write_all(config.as_bytes());
        drop(stdin);
        let output = child
            .wait_with_output()
            .map_err(|e| AlertError::Spawn(e.to_string()))?;
        if written.is_err() || !output.status.success() {
            return Err(AlertError::Unreachable(output.status.code().unwrap_or(-1)));
        }
        let status = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u16>();
        match status {
            Ok(200..=299) => Ok(()),
            Ok(status) => Err(AlertError::Refused(status)),
            Err(_) => Err(AlertError::Unreachable(0)),
        }
    }
}

// A curl config: one `option = "value"` per line. No redirects are followed,
// so a post never goes anywhere but the URL.
fn config(webhook: &Webhook, alert: &Alert) -> String {
    let (header, body) = match webhook.kind {
        WebhookKind::Discord => {
            let content = fit(&format!("{}\n{}", alert.title, alert.text), DISCORD_MAX);
            let body = serde_json::json!({
                "username": "kelpie",
                "content": content,
                "allowed_mentions": { "parse": [] },
            });
            (
                "Content-Type: application/json".to_owned(),
                body.to_string(),
            )
        }
        WebhookKind::Ntfy => (
            format!("Title: {}", alert.title),
            fit(&alert.text, NTFY_MAX),
        ),
    };
    let lines = [
        ("url", webhook.url.expose().to_owned()),
        ("proto", "=https,http".to_owned()),
        ("silent", String::new()),
        ("connect-timeout", CONNECT_TIMEOUT.to_string()),
        ("max-time", MAX_TIME.to_string()),
        ("output", "/dev/null".to_owned()),
        ("write-out", "%{http_code}".to_owned()),
        // curl reads a literal `\n` in a value as a newline, so no worker text goes in a header.
        ("header", header),
        ("data-raw", body),
    ];
    lines
        .into_iter()
        .map(|(option, value)| match option {
            "silent" => "silent\n".to_owned(),
            _ => format!("{option} = \"{}\"\n", quote(&value)),
        })
        .collect()
}

// The escapes a quoted curl config value reads.
fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

// Cuts the middle of `text` to fit in `max` bytes, keeping its start and
// its end, where a ruling's triggers are.
fn fit(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    // The head rounds down and the tail's start rounds up, so both only shrink.
    let mut head = max - KEEP_TAIL - CUT.len();
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len() - KEEP_TAIL;
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    format!("{}{CUT}{}", &text[..head], &text[tail..])
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    use super::*;
    use crate::webhook::WebhookUrl;

    fn webhook(kind: WebhookKind, url: &str) -> Webhook {
        Webhook {
            kind,
            url: WebhookUrl::try_from(url.to_owned()).unwrap(),
        }
    }

    fn alert(text: &str) -> Alert {
        Alert {
            title: "kelpie: webapp ruling 3".into(),
            text: text.into(),
        }
    }

    /// One request as a local stand-in received it
    #[derive(Debug)]
    struct Received {
        request_line: String,
        headers: Vec<String>,
        body: String,
    }

    // A stand-in webhook on this machine that answers one post with `status`.
    fn stand_in(status: u16) -> (String, thread::JoinHandle<Received>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://127.0.0.1:{}/hook/s3cr3t",
            listener.local_addr().unwrap().port()
        );
        let served = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            // Curl gives up after MAX_TIME, so a read never waits longer.
            stream
                .set_read_timeout(Some(Duration::from_secs(MAX_TIME.into())))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut headers = Vec::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
                headers.push(line.trim().to_owned());
            }
            let length: usize = headers
                .iter()
                .find_map(|h| {
                    h.to_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse().unwrap())
                })
                .unwrap_or(0);
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let mut stream = stream;
            write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            Received {
                request_line: request_line.trim().to_owned(),
                headers,
                body: String::from_utf8(body).unwrap(),
            }
        });
        (url, served)
    }

    #[test]
    fn discord_gets_the_title_and_question_as_json_that_pings_nobody() {
        let (url, served) = stand_in(204);
        let text = "Merge pull request #71? `shep trigger x rule '3 yes'` \"quoted\"\\ @everyone";
        Curl.post(&webhook(WebhookKind::Discord, &url), &alert(text))
            .unwrap();
        let got = served.join().unwrap();
        assert_eq!(got.request_line, "POST /hook/s3cr3t HTTP/1.1");
        assert!(
            got.headers
                .contains(&"Content-Type: application/json".to_owned()),
            "{got:?}"
        );
        let body: serde_json::Value = serde_json::from_str(&got.body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "username": "kelpie",
                "content": format!("kelpie: webapp ruling 3\n{text}"),
                "allowed_mentions": { "parse": [] },
            })
        );
    }

    #[test]
    fn ntfy_gets_the_title_as_a_header_and_the_question_as_the_body() {
        let (url, served) = stand_in(200);
        let text =
            "The worker on #7 asks:\n\n\tWhich name?\r\n\nanswer with `rule '3 answer <text>'`";
        Curl.post(&webhook(WebhookKind::Ntfy, &url), &alert(text))
            .unwrap();
        let got = served.join().unwrap();
        assert!(
            got.request_line.starts_with("POST /hook/s3cr3t "),
            "{got:?}"
        );
        assert!(
            got.headers
                .contains(&"Title: kelpie: webapp ruling 3".to_owned()),
            "{got:?}"
        );
        assert_eq!(got.body, text);
    }

    #[test]
    fn a_refused_post_names_the_status_and_not_the_url() {
        let (url, served) = stand_in(500);
        let err = Curl
            .post(&webhook(WebhookKind::Ntfy, &url), &alert("q"))
            .unwrap_err();
        served.join().unwrap();
        assert_eq!(err, AlertError::Refused(500));
        assert_eq!(err.to_string(), "the webhook answered HTTP 500");
    }

    #[test]
    fn an_unreachable_webhook_names_curls_exit_and_not_the_url() {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let url = format!("http://127.0.0.1:{port}/hook/s3cr3t");
        let err = Curl
            .post(&webhook(WebhookKind::Discord, &url), &alert("q"))
            .unwrap_err();
        assert_eq!(
            err,
            AlertError::Unreachable(7),
            "curl's couldn't-connect code"
        );
        assert!(!err.to_string().contains("s3cr3t"), "{err}");
    }

    #[test]
    fn a_long_question_is_cut_in_the_middle_keeping_its_triggers() {
        let text = format!(
            "{}\n{}`shep trigger x rule '3 yes'`",
            "a".repeat(3000),
            "b".repeat(300)
        );
        let fitted = fit(&text, DISCORD_MAX);
        assert!(fitted.len() <= DISCORD_MAX, "{}", fitted.len());
        assert!(fitted.starts_with("aaa"));
        assert!(fitted.ends_with("`shep trigger x rule '3 yes'`"));
        assert!(fitted.contains("[…cut; the whole question is in status]"));
        assert_eq!(fit("short", DISCORD_MAX), "short");

        for pad in 0..4 {
            let wide = format!("{}{}", "a".repeat(pad), "€".repeat(1500));
            for max in [DISCORD_MAX, NTFY_MAX] {
                let fitted = fit(&wide, max);
                assert!(fitted.len() <= max, "{pad} {max}: {}", fitted.len());
            }
        }
    }
}
