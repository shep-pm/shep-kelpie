//! One frame each way: one JSON object on one line

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::request::Request;

/// The protocol these types speak, which starts at 1 (ADR 0017)
pub const PROTOCOL: u32 = 1;

/// The longest a frame may be, in bytes, which is the lease door's line
///
/// A longer one closes the connection, so a peer cannot make a runner buffer
/// without end.
pub const LONGEST_FRAME: usize = 64 * 1024;

/// The first frame each way, which says which protocol the peer speaks
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// The protocol the peer speaks
    pub protocol: u32,
}

/// A request the runner does not have, which it answers with and keeps the
/// connection open for
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnknownRequest {
    /// The request it was asked for
    pub request: String,
    /// The runner's version, which the client names as the one to upgrade
    pub version: String,
}

impl fmt::Display for UnknownRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "this runner ({}) has no `{}` request: upgrade kelpie",
            self.version, self.request
        )
    }
}

impl core::error::Error for UnknownRequest {}

/// Why a frame was refused
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// The line is longer than [`LONGEST_FRAME`], so the connection closes
    TooLong {
        /// How long the line was, in bytes
        length: usize,
    },
    /// The line names a request this crate has no variant for
    Unknown {
        /// The request it names
        request: String,
    },
    /// The line is not one JSON object, or names a request this crate has with
    /// params that do not read as it, with what reading it said
    Unreadable(String),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong { length } => write!(
                f,
                "a frame is at most {LONGEST_FRAME} bytes, and this one is {length}"
            ),
            Self::Unknown { request } => write!(f, "unknown request `{request}`"),
            Self::Unreadable(why) => write!(f, "the frame does not read: {why}"),
        }
    }
}

impl core::error::Error for FrameError {}

/// `value` as one frame: one JSON object and the end of its line
///
/// # Errors
///
/// [`serde_json::Error`] when `value` cannot be written as JSON.
pub fn encode<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    Ok(line)
}

/// The request one frame carries
///
/// # Errors
///
/// [`FrameError::TooLong`] when the line is over [`LONGEST_FRAME`],
/// [`FrameError::Unknown`] when it names a request this crate has no variant
/// for, and [`FrameError::Unreadable`] when it is not one JSON object, or names
/// a request this crate has with params that do not read as it.
///
/// [`FrameError::Unknown`]: FrameError::Unknown
/// [`FrameError::Unreadable`]: FrameError::Unreadable
pub fn decode(line: &str) -> Result<Request, FrameError> {
    if line.len() > LONGEST_FRAME {
        return Err(FrameError::TooLong { length: line.len() });
    }
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|e| FrameError::Unreadable(e.to_string()))?;
    let unreadable = match Request::deserialize(&value) {
        Ok(request) => return Ok(request),
        Err(e) => e,
    };
    match value.get("action").and_then(Value::as_str) {
        Some(action) if !Request::reads(action) => Err(FrameError::Unknown {
            request: action.to_owned(),
        }),
        _ => Err(FrameError::Unreadable(unreadable.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::request::RuleAnswer;
    use shep_kelpie_core::ticket::TicketId;

    // The ticket of GitHub issue `number`
    fn ticket(number: u64) -> TicketId {
        TicketId::from_issue(number).expect("a test's issue is not zero")
    }

    #[test]
    fn a_frame_over_the_bound_is_refused() {
        let request = Request::Tell("x".repeat(LONGEST_FRAME));
        let line = encode(&request).unwrap();
        assert!(line.len() > LONGEST_FRAME, "{line}");
        assert_eq!(
            decode(&line),
            Err(FrameError::TooLong { length: line.len() })
        );
    }

    #[test]
    fn a_frame_round_trips_and_ends_its_line() {
        for request in [
            Request::Status,
            Request::Rule(
                ticket(7),
                RuleAnswer::Rework {
                    note: "one more".into(),
                },
            ),
            Request::Board(Some((ticket(7), 2))),
        ] {
            let line = encode(&request).unwrap();
            assert!(line.ends_with('\n'), "{line}");
            assert_eq!(decode(&line).unwrap(), request, "{line}");
        }
    }

    #[test]
    fn a_request_this_crate_has_no_variant_for_is_named_not_dropped() {
        let line = r#"{"action":"subscribe","since":12}"#;
        assert_eq!(
            decode(line),
            Err(FrameError::Unknown {
                request: "subscribe".to_owned()
            })
        );
        assert_eq!(
            UnknownRequest {
                request: "subscribe".to_owned(),
                version: "1.2.3".to_owned(),
            }
            .to_string(),
            "this runner (1.2.3) has no `subscribe` request: upgrade kelpie"
        );
    }

    #[test]
    fn a_request_it_has_with_params_that_do_not_read_is_not_called_unknown() {
        let asked: [(&str, &str); 4] = [
            (r#"{"action":"add"}"#, "missing field `params`"),
            (
                r#"{"action":"add","params":"x"}"#,
                "invalid value: string \"x\", expected",
            ),
            (r#"{"action":"attach","params":[7]}"#, "invalid length 1"),
            (
                r#"{"action":"rule","params":[7,{"answer":"nope"}]}"#,
                "unknown variant `nope`",
            ),
        ];
        for (line, want) in asked {
            let error = decode(line).expect_err("{line} reads as a request");
            assert!(
                matches!(&error, FrameError::Unreadable(seen) if seen.contains(want)),
                "{line}: {error}"
            );
        }
    }

    #[test]
    fn a_line_that_is_not_one_object_is_refused() {
        assert!(matches!(decode("{"), Err(FrameError::Unreadable(_))));
        assert!(matches!(
            decode(r#"{"since":12}"#),
            Err(FrameError::Unreadable(_))
        ));
    }

    #[test]
    fn the_hello_frame_carries_the_protocol() {
        let line = encode(&Hello { protocol: PROTOCOL }).unwrap();
        assert_eq!(
            serde_json::from_str::<Hello>(&line).unwrap(),
            Hello { protocol: 1 }
        );
        assert_eq!(json!(1), json!(PROTOCOL));
    }
}
