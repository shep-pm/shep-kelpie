//! What an ntfy post carries beyond its text, and reading the topic back
//!
//! A ruling's alert ends with the replies that answer it, each carrying the
//! ruling's one-time code, and a yes-or-no ruling's alert carries buttons
//! that post those replies to the topic. Everything kelpie posts is tagged
//! `kelpie`, so reading the topic back skips it.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::ports::{Reply, ReplyWith, Since, Takes};

/// The tag on everything kelpie posts, which a read skips
pub(super) const TAG: &str = "kelpie";

/// The note a Send back button's no carries, since a button cannot ask for one
pub(super) const SEND_BACK_NOTE: &str =
    "Sent back from ntfy with no note. Ask the maintainer what to change.";

/// The line that ends a ruling's alert: the replies that answer it
pub(super) fn reply_line(reply: &ReplyWith) -> String {
    let (id, code) = (reply.id, reply.code.expose());
    match reply.takes {
        Takes::Answer => format!("\n\nReply here with `{id} answer <text> {code}`."),
        Takes::YesOrNo { .. } => {
            format!(
                "\n\nTap a button, or reply here with `{id} yes {code}` or `{id} no <note> {code}`."
            )
        }
    }
}

/// The `Actions` header's JSON for a ruling's buttons, or `None` for a
/// ruling that takes a typed answer
///
/// Each button posts to the topic itself at the lowest priority, so the
/// reply does not buzz the phone again, and clears the alert. Leave posts
/// a line tagged as kelpie's, which a read skips.
pub(super) fn actions(url: &str, reply: &ReplyWith) -> Option<String> {
    let Takes::YesOrNo { yes } = reply.takes else {
        return None;
    };
    let (id, code) = (reply.id, reply.code.expose());
    let button = |label: &str, body: String, tags: Option<&str>| {
        let mut headers = json!({ "X-Priority": "1" });
        if let Some(tags) = tags {
            headers["X-Tags"] = Value::from(tags);
        }
        json!({
            "action": "http",
            "label": label,
            "url": url,
            "method": "POST",
            "headers": headers,
            "body": body,
            "clear": true,
        })
    };
    let buttons = json!([
        button(yes, format!("{id} yes {code}"), None),
        button(
            "Send back",
            format!("{id} no {SEND_BACK_NOTE} {code}"),
            None
        ),
        button("Leave", format!("Ruling {id} left for later."), Some(TAG)),
    ]);
    Some(buttons.to_string())
}

/// The URL that reads the topic's cached messages since `since` and closes
///
/// `url` is the topic's, whose query (an `auth` token, say) is kept.
pub(super) fn poll_url(url: &str, since: &Since) -> String {
    let url = url.split('#').next().unwrap_or_default();
    let (topic, query) = url.split_once('?').unwrap_or((url, ""));
    let since = match since {
        Since::Time(at) => at.0.to_string(),
        Since::After(id) => id.clone(),
    };
    let query = match query {
        "" => String::new(),
        query => format!("{query}&"),
    };
    format!(
        "{}/json?{query}poll=1&since={since}",
        topic.trim_end_matches('/')
    )
}

/// One line of ntfy's JSON stream
#[derive(Deserialize)]
struct Line {
    id: String,
    event: String,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    attachment: Option<Value>,
}

/// The messages in a poll's output, oldest first
///
/// A message kelpie posted, or one sent as an attachment, has no text. An
/// id that is not plain letters and digits is skipped, since it goes back
/// into the next read's URL. `None` when any line is not one of ntfy's.
pub(super) fn parse(output: &str) -> Option<Vec<Reply>> {
    let mut replies = Vec::new();
    for line in output.lines().filter(|l| !l.trim().is_empty()) {
        let line: Line = serde_json::from_str(line).ok()?;
        let plain = !line.id.is_empty() && line.id.bytes().all(|b| b.is_ascii_alphanumeric());
        if line.event != "message" || !plain {
            continue;
        }
        let ours = line.tags.iter().any(|t| t == TAG);
        let text = line.message.filter(|_| !ours && line.attachment.is_none());
        replies.push(Reply { id: line.id, text });
    }
    Some(replies)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{OneTimeCode, Timestamp};

    const POLL: &str = include_str!("../../fixtures/ntfy-poll.jsonl");

    fn reply(takes: Takes) -> ReplyWith {
        ReplyWith {
            id: 3,
            code: OneTimeCode::draw().unwrap(),
            takes,
        }
    }

    #[test]
    fn a_recorded_poll_reads_as_replies_skipping_kelpies_own_and_files() {
        let replies = parse(POLL).unwrap();
        let texts: Vec<_> = replies.iter().map(|r| r.text.as_deref()).collect();
        assert_eq!(
            texts,
            [
                None,
                Some("3 yes 7hq2mx9d"),
                None,
                Some("4 answer call it `kelpie-probe`, not probe. 2m8kqz3c"),
                None,
                Some("hello from the phone"),
            ]
        );
        assert_eq!(replies[0].id, "W3EqiUm5rsNq");
        assert_eq!(replies[5].id, "dvMOjwjVgxy8");
    }

    #[test]
    fn a_poll_that_is_not_ntfys_is_refused_and_odd_ids_are_skipped() {
        assert_eq!(parse("<html>rate limited</html>"), None);
        assert_eq!(parse(""), Some(vec![]));
        let odd = r#"{"id":"a&since=all","event":"message","message":"1 yes x"}
{"id":"k1","event":"keepalive"}
{"id":"ok1","event":"message","message":"1 yes x"}"#;
        assert_eq!(
            parse(odd),
            Some(vec![Reply {
                id: "ok1".into(),
                text: Some("1 yes x".into())
            }])
        );
    }

    #[test]
    fn the_poll_url_keeps_the_topics_query() {
        let after = Since::After("W3EqiUm5rsNq".into());
        assert_eq!(
            poll_url("https://ntfy.sh/topic", &after),
            "https://ntfy.sh/topic/json?poll=1&since=W3EqiUm5rsNq"
        );
        assert_eq!(
            poll_url(
                "https://ntfy.example/topic/?auth=abc",
                &Since::Time(Timestamp(17))
            ),
            "https://ntfy.example/topic/json?auth=abc&poll=1&since=17"
        );
    }

    #[test]
    fn every_reply_and_button_carries_the_rulings_code() {
        let yes_or_no = reply(Takes::YesOrNo { yes: "Merge" });
        let code = yes_or_no.code.expose().to_owned();
        let line = reply_line(&yes_or_no);
        assert!(line.contains(&format!("`3 yes {code}`")), "{line}");
        assert!(line.contains(&format!("`3 no <note> {code}`")), "{line}");

        let url = "https://ntfy.sh/topic?auth=abc";
        let buttons: Value = serde_json::from_str(&actions(url, &yes_or_no).unwrap()).unwrap();
        let shown: Vec<_> = buttons
            .as_array()
            .unwrap()
            .iter()
            .map(|b| (b["label"].as_str().unwrap(), b["body"].as_str().unwrap()))
            .collect();
        let send_back = format!("3 no {SEND_BACK_NOTE} {code}");
        assert_eq!(
            shown,
            [
                ("Merge", format!("3 yes {code}").as_str()),
                ("Send back", send_back.as_str()),
                ("Leave", "Ruling 3 left for later."),
            ]
        );
        for button in buttons.as_array().unwrap() {
            assert_eq!(button["url"], url);
            assert_eq!(button["method"], "POST");
            assert_eq!(button["clear"], true);
            assert_eq!(button["headers"]["X-Priority"], "1");
        }
        assert_eq!(buttons[2]["headers"]["X-Tags"], TAG);
        assert!(buttons[0]["headers"].get("X-Tags").is_none());

        let question = reply(Takes::Answer);
        let code = question.code.expose().to_owned();
        assert_eq!(actions(url, &question), None);
        assert_eq!(
            reply_line(&question),
            format!("\n\nReply here with `3 answer <text> {code}`.")
        );
    }
}
