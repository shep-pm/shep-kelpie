//! What an ntfy post carries beyond its text, and reading the topic back
//!
//! A ruling's alert ends with the replies that answer it, each ending with
//! the maintainer's authenticator code. Everything kelpie posts is tagged
//! `kelpie`, so reading the topic back skips it.

use serde::Deserialize;
use serde_json::Value;

use crate::ports::{Reply, ReplyWith, Since, Takes, Timestamp};

/// The tag on everything kelpie posts, which a read skips
pub(super) const TAG: &str = "kelpie";

/// The line that ends a ruling's alert: the replies that answer it
pub(super) fn reply_line(reply: &ReplyWith) -> String {
    let ruling = reply.id;
    let replies = match reply.takes {
        Takes::Answer => format!("`{ruling} <text> <code>`"),
        Takes::Merge => format!(
            "`{ruling} yes <code>`, `{ruling} no <note> <code>` or `{ruling} rework <note> <code>`"
        ),
        Takes::YesOrNo => format!("`{ruling} yes <code>` or `{ruling} no <note> <code>`"),
    };
    format!("\n\nReply here with {replies}, where <code> is kelpie's authenticator code.")
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
    time: u64,
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
        let whole: Value = serde_json::from_str(line).ok()?;
        let line: Line = serde_json::from_value(whole.clone()).ok()?;
        let plain = !line.id.is_empty() && line.id.bytes().all(|b| b.is_ascii_alphanumeric());
        if line.event != "message" || !plain {
            continue;
        }
        let mut said = Vec::new();
        strings(&whole, &mut said);
        let ours = line.tags.iter().any(|t| t == TAG);
        let cut = line.attachment.is_some();
        let text = line.message.filter(|_| !ours && !cut);
        replies.push(Reply {
            id: line.id,
            time: Timestamp(line.time),
            text,
            said,
            cut,
        });
    }
    Some(replies)
}

// Every string a post carries (its message, title, tags, attachment,
// actions and anything ntfy adds later), but not the fields ntfy itself
// sets, whose digits no sender chose.
fn strings(value: &Value, into: &mut Vec<String>) {
    match value {
        Value::String(text) => into.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| strings(item, into)),
        Value::Object(fields) => {
            let set_by_ntfy = ["id", "time", "expires", "event", "topic"];
            for (name, field) in fields {
                if !set_by_ntfy.contains(&name.as_str()) {
                    strings(field, into);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLL: &str = include_str!("../../fixtures/ntfy-poll.jsonl");

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
        assert_eq!(replies[0].time, Timestamp(1_790_683_787));
        // Kelpie's own alert and the attachment are not answers, but every
        // text they carry is still read for codes to spend.
        assert!(replies[0].said.iter().any(|s| s == "kelpie: koji ruling 3"));
        assert!(
            replies[0].said.iter().any(|s| s == "3 yes 7hq2mx9d"),
            "a button's body"
        );
        assert!(replies[4].said.iter().any(|s| s == "attachment.txt"));
        let cut: Vec<_> = replies.iter().map(|r| r.cut).collect();
        assert_eq!(cut, [false, false, false, false, true, false]);
        assert!(
            !replies[0].said.iter().any(|s| s == "kelpie-probe-topic"),
            "not the topic"
        );
        assert_eq!(replies[5].id, "dvMOjwjVgxy8");
    }

    #[test]
    fn a_poll_that_is_not_ntfys_is_refused_and_odd_ids_are_skipped() {
        assert_eq!(parse("<html>rate limited</html>"), None);
        assert_eq!(parse(""), Some(vec![]));
        let odd = r#"{"id":"a&since=all","time":1,"event":"message","message":"1 yes x"}
{"id":"k1","time":1,"event":"keepalive"}
{"id":"ok1","time":2,"event":"message","message":"1 yes x"}"#;
        assert_eq!(
            parse(odd),
            Some(vec![Reply {
                id: "ok1".into(),
                time: Timestamp(2),
                text: Some("1 yes x".into()),
                said: vec!["1 yes x".into()],
                cut: false,
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
    fn the_reply_line_names_what_the_ruling_takes() {
        let yes_or_no = ReplyWith {
            id: 3,
            takes: Takes::YesOrNo,
        };
        assert_eq!(
            reply_line(&yes_or_no),
            "\n\nReply here with `3 yes <code>` or `3 no <note> <code>`, \
             where <code> is kelpie's authenticator code."
        );
        let merge = ReplyWith {
            takes: Takes::Merge,
            ..yes_or_no.clone()
        };
        assert_eq!(
            reply_line(&merge),
            "\n\nReply here with `3 yes <code>`, `3 no <note> <code>` or \
             `3 rework <note> <code>`, where <code> is kelpie's authenticator code."
        );
        let question = ReplyWith {
            takes: Takes::Answer,
            ..yes_or_no
        };
        assert!(reply_line(&question).contains("`3 <text> <code>`"));
    }
}
