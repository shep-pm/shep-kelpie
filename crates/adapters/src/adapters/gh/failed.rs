//! What a failed `gh` call said, and when it told kelpie to ask again
//!
//! Every call runs with `GH_DEBUG=api`, which makes gh print each HTTP
//! exchange on stderr, headers prefixed `< `. A failed call keeps its own
//! error lines, and a rate-limited one reads `Retry-After` and
//! `X-RateLimit-Reset` from those headers.

use crate::ports::{Cooldown, ForgeError, says_used_up};

// gh's debug lines for a request, a response and a note on either
const TRAFFIC: [&str; 3] = ["* ", "> ", "< "];

/// The error for a call that exited unsuccessfully with `stderr`
pub(super) fn failure(stderr: &str) -> ForgeError {
    let lines: Vec<&str> = stderr.lines().collect();
    let is_traffic = |line: &&str| {
        TRAFFIC.iter().any(|mark| line.starts_with(mark)) || matches!(*line, "<" | ">")
    };
    let traffic = lines.iter().any(is_traffic);
    let cooldown = match traffic {
        true => cooldown_of(&lines),
        false => Cooldown::default(),
    };
    let own: Vec<&str> = lines.iter().copied().filter(|l| !is_traffic(l)).collect();
    // What gh adds after the exchange is its own line; the body is not.
    let pick =
        |keep: fn(&str) -> bool| -> Vec<&str> { own.iter().copied().filter(|l| keep(l)).collect() };
    let (gh, status) = (pick(|l| l.starts_with("gh: ")), pick(is_status));
    let said = if !gh.is_empty() {
        gh
    } else if !status.is_empty() {
        status
    } else if traffic {
        pick(|l| !l.trim_start().starts_with(['{', '[', '}', ']', '"']))
    } else {
        return limited_or(stderr, stderr, cooldown);
    };
    limited_or(&said.join("\n"), &said.join("\n"), cooldown)
}

// gh's line for a call that got an HTTP error, such as `HTTP 504: 504 Gateway
// Timeout (https://api.github.com/graphql)`, after the page GitHub answered with.
fn is_status(line: &str) -> bool {
    line.strip_prefix("HTTP ")
        .and_then(|rest| rest.split_once(':'))
        .is_some_and(|(code, _)| code.len() == 3 && code.bytes().all(|b| b.is_ascii_digit()))
}

// A rate limit is `Limited` with what the answer said of when, anything else
// stays what gh printed.
fn limited_or(kept: &str, text: &str, cooldown: Cooldown) -> ForgeError {
    match says_used_up(kept) {
        true => ForgeError::Limited {
            said: format!("gh failed: {}", text.trim()),
            cooldown,
        },
        false => ForgeError::Failed(kept.to_owned()),
    }
}

// The two headers of the last response in the debug lines
fn cooldown_of(lines: &[&str]) -> Cooldown {
    let (mut retry_after, mut reset) = ("", "");
    for line in lines {
        let Some(header) = line.strip_prefix("< ") else {
            continue;
        };
        if header.starts_with("HTTP/") {
            (retry_after, reset) = ("", "");
        }
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        match name.to_ascii_lowercase().as_str() {
            "retry-after" => retry_after = value,
            "x-ratelimit-reset" => reset = value,
            _ => {}
        }
    }
    Cooldown::read(retry_after, reset)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The shape gh's `GH_DEBUG=api` prints for a call a secondary limit
    // refused: the exchange, the body, then gh's own error line.
    const SECONDARY: &str = "\
* Request at 2026-10-10 05:31:54.1 +0000 UTC
* Request to https://api.github.com/graphql
> POST /graphql HTTP/1.1
> Host: api.github.com
>
< HTTP/2.0 403 Forbidden
< Content-Type: application/json; charset=utf-8
< Retry-After: 75
< X-Ratelimit-Reset: 1790000600
<
{\"message\":\"You have exceeded a secondary rate limit.\"}
gh: You have exceeded a secondary rate limit. Please wait a few minutes before you try again. (HTTP 403)
";

    #[test]
    fn a_secondary_limit_keeps_its_error_line_and_its_retry_after() {
        assert_eq!(
            failure(SECONDARY),
            ForgeError::Limited {
                said: "gh failed: gh: You have exceeded a secondary rate limit. Please wait a \
                       few minutes before you try again. (HTTP 403)"
                    .into(),
                cooldown: Cooldown {
                    retry_after: Some(75),
                    reset: Some(crate::ports::Timestamp(1_790_000_600)),
                },
            }
        );
    }

    #[test]
    fn a_failure_with_no_debug_lines_is_kept_as_gh_said_it() {
        let plain = "gh: Not Found (HTTP 404)\n";
        assert_eq!(failure(plain), ForgeError::Failed(plain.trim_end().into()));
        let limited = failure("gh: API rate limit exceeded for user ID 1. (HTTP 403)\n");
        assert!(
            matches!(limited, ForgeError::Limited { cooldown, .. } if cooldown == Cooldown::default())
        );
    }

    #[test]
    fn any_other_failure_drops_the_debug_lines_and_keeps_the_error() {
        let stderr = "* Request to https://api.github.com/x\n< HTTP/2.0 404 Not Found\n<\n\
                      {\"message\":\"Not Found\"}\ngh: Not Found (HTTP 404)\n";
        assert_eq!(
            failure(stderr),
            ForgeError::Failed("gh: Not Found (HTTP 404)".into())
        );
    }

    #[test]
    fn a_failure_with_no_gh_line_keeps_its_error_and_none_of_the_bodies_around_it() {
        let stderr = "* Request to https://api.github.com/graphql\n> POST /graphql HTTP/1.1\n>\n\
                      {\n  \"query\": \"mutation { addComment }\",\n  \
                      \"body\": \"kelpie holds on a rate limit\"\n}\n\
                      < HTTP/2.0 502 Bad Gateway\n<\n\
                      {\"message\":\"rate limit text in a body\"}\n\
                      GraphQL: Could not resolve to a node with the global id of 'X'. (node)\n";
        assert_eq!(
            failure(stderr),
            ForgeError::Failed(
                "GraphQL: Could not resolve to a node with the global id of 'X'. (node)".into()
            )
        );
    }

    // A 504 on a GraphQL read, as kept from a runner's log: the query and the
    // page GitHub answered with, its images cut short, then gh's status line.
    const PAGE: &str = include_str!("../../../../../fixtures/gh-graphql-504-page.txt");

    #[test]
    fn a_failure_that_printed_a_page_keeps_only_gh_s_status_line() {
        assert_eq!(
            failure(PAGE),
            ForgeError::Failed(
                "HTTP 504: 504 Gateway Timeout (https://api.github.com/graphql)".into()
            )
        );
        let exchanged = format!("* Request to https://api.github.com/graphql\n<\n{PAGE}");
        assert_eq!(failure(&exchanged), failure(PAGE));
    }

    #[test]
    fn a_failure_the_forge_answered_5xx_may_be_asked_again() {
        assert!(failure(PAGE).passes(), "{:?}", failure(PAGE));
        assert!(
            failure("gh: Bad Gateway (HTTP 502)\n").passes(),
            "gh's own line names the status it answered with"
        );
        assert!(
            !failure("gh: Not Found (HTTP 404)\n").passes(),
            "a refused ask is not the forge failing"
        );
        assert!(
            !failure("GraphQL: Could not resolve to a node with the global id of 'X'. (node)\n")
                .passes(),
            "a failure that names no status is not the forge failing"
        );
    }

    #[test]
    fn a_used_up_limit_wins_over_the_status_of_the_answer() {
        let stderr = "gh: API rate limit exceeded for user ID 1. (HTTP 502)\n";
        assert!(matches!(failure(stderr), ForgeError::Limited { .. }));
    }

    #[test]
    fn the_last_response_s_headers_are_the_ones_read() {
        let stderr = "< HTTP/2.0 502 Bad Gateway\n< Retry-After: 5\n<\n\
                      < HTTP/2.0 403 Forbidden\n< Retry-After: 120\n<\n\
                      gh: API rate limit exceeded (HTTP 403)\n";
        let ForgeError::Limited { cooldown, .. } = failure(stderr) else {
            panic!("a rate limit is limited");
        };
        assert_eq!(cooldown.retry_after, Some(120));
    }
}
