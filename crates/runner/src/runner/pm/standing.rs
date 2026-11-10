//! The standing lines that bind one work item, for its worker and its reviewers
//!
//! A line of `pm-standing.md` binds a work item when it names the issue, one
//! of its blockers, or a path the issue touches. A line that names none of
//! them stays the project manager's to apply.

use crate::board::check::{blockers, touched};
use crate::runner::Runner;
use crate::ticket::TicketId;

use super::notes::PM_STANDING;

impl Runner {
    /// The lines of `pm-standing.md` that name `ticket`, one of its blockers or a
    /// path its issue touches, one to a line, or nothing when none does
    ///
    /// The blockers and paths come from `body`, or from the body the board
    /// last read when it is `None`, and the files the work item changes are
    /// added to the paths.
    pub(in crate::runner) fn standing_note(&self, ticket: TicketId, body: Option<&str>) -> String {
        let Ok(text) = std::fs::read_to_string(self.paths.pm.join(PM_STANDING)) else {
            return String::new();
        };
        let body = body.or_else(|| self.brief_body(ticket)).unwrap_or_default();
        let mut tickets = vec![ticket];
        tickets.extend(blockers(body));
        let mut paths = touched(body).unwrap_or_default();
        let changed = self.item_files(ticket);
        paths.extend(changed.list().iter().map(String::as_str));
        applying(&text, &tickets, &paths).join("\n")
    }
}

/// What a worker is told of the standing `lines` that bind its work item, to
/// add after its first prompt, and nothing when there are none
pub(in crate::runner) fn worker_note(lines: &str) -> String {
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "\nThe project manager keeps these standing rulings, and each names this work item, \
         a work item it waits on, or a file it touches. Follow each line. Where one cannot \
         be followed, say so in the pull request and why.\n\n--- the standing rulings ---\n\
         {lines}\n--- end ---\n"
    )
}

// The lines of `text` that apply to a work item: each names one of `tickets` or a path it
// touches. A `## ` tell heading is the start of a block and no line.
fn applying<'a>(text: &'a str, tickets: &[TicketId], paths: &[&str]) -> Vec<&'a str> {
    let shown: Vec<String> = tickets.iter().map(TicketId::shown).collect();
    (text.lines())
        .filter(|line| !line.starts_with("## ") && !line.trim().is_empty())
        .filter(|line| {
            shown.iter().any(|ticket| names_ticket(line, ticket)) || names_path(line, paths)
        })
        .collect()
}

// Whether `line` names `ticket`, as `#12` or `k3`, whole: not as the start of a longer number,
// and not after a repo's name, which is another repo's issue.
fn names_ticket(line: &str, ticket: &str) -> bool {
    line.match_indices(ticket).any(|(at, _)| {
        let before = line[..at].chars().next_back();
        let after = line[at + ticket.len()..].chars().next();
        !before.is_some_and(|c| c.is_alphanumeric() || "/-_".contains(c))
            && !after.is_some_and(|c| c.is_ascii_digit())
    })
}

// Whether a word of `line` is one of `paths` as written, or a folder, written with its trailing
// slash, that holds one.
fn names_path(line: &str, paths: &[&str]) -> bool {
    line.split_whitespace().any(|word| {
        let word = word.trim_matches(|c: char| "`'\"()[]<>,;:*".contains(c));
        let word = word.strip_suffix('.').unwrap_or(word);
        !word.is_empty()
            && paths
                .iter()
                .any(|path| *path == word || (word.ends_with('/') && path.starts_with(word)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(number: u64) -> TicketId {
        number.to_string().parse().unwrap()
    }

    #[test]
    fn a_line_naming_the_issue_a_blocker_or_a_touched_path_applies() {
        let text = "Hold #12 until the release\n\
                    Wait for #9 before any change to retries\n\
                    Do not edit `src/route.rs` by hand\n\
                    Leave crates/runner/src/ alone (until #40)\n\
                    Hold k3 for the demo\n";
        let tickets = [id(12), id(9), "k3".parse().unwrap()];
        let paths = ["src/route.rs", "crates/runner/src/pm/mod.rs"];

        assert_eq!(
            applying(text, &tickets, &paths),
            text.lines().collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_line_naming_none_of_them_does_not_apply() {
        let text = "Hold #40 until the release\n\
                    Do not edit `src/other.rs` by hand\n\
                    Review every diff before a merge\n\
                    Leave crates/core/ alone\n";

        assert!(
            applying(
                text,
                &[id(12), id(9)],
                &["src/route.rs", "crates/runner/a.rs"]
            )
            .is_empty()
        );
    }

    #[test]
    fn an_issue_number_matches_whole_and_not_as_the_start_of_a_longer_one() {
        let text = "Hold #12 for now\nHold #123 for now\nHold #1 for now\nHold #1, then #12.\n";

        assert_eq!(
            applying(text, &[id(1)], &[]),
            ["Hold #1 for now", "Hold #1, then #12."]
        );
        assert_eq!(
            applying(text, &[id(12)], &[]),
            ["Hold #12 for now", "Hold #1, then #12."]
        );
    }

    #[test]
    fn a_tell_heading_is_never_a_line_of_its_own() {
        let text = "## 2026-09-20 #12 touches src/route.rs\nHold #12 for now\n";

        assert_eq!(
            applying(text, &[id(12)], &["src/route.rs"]),
            ["Hold #12 for now"]
        );
    }
}
