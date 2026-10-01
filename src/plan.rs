//! Planning: whether a ready issue is one pull request or several
//!
//! Before the board's pick opens a work item, a planning call reads the
//! repo and answers in JSON: keep the issue whole, or split it into pieces,
//! each a complete vertical slice with the earlier pieces it waits on. A
//! split becomes sub-issues of the issue, worked in its place. This is a
//! level above the work split, which stays the worker's inside one work item.

use serde::{Deserialize, Serialize};

/// What the planning call decided
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Planned {
    /// One pull request
    Whole {
        /// Why, in a sentence
        why: String,
    },
    /// Several pull requests, one per piece
    Split {
        /// Why, for the issue's comment
        why: String,
        /// The pieces, blockers first
        pieces: Vec<Piece>,
    },
}

/// One piece of a split, which becomes a sub-issue
///
/// Unknown keys are ignored, so a reply with one extra key is still a plan.
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Piece {
    /// The sub-issue's title
    pub title: String,
    /// The sub-issue's body
    pub body: String,
    /// The earlier pieces it waits on, numbered from 1
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<usize>,
}

/// Where planning an issue stands, kept in the project's state
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    /// The issue planned
    pub issue: u64,
    /// Where it stands
    pub stage: Stage,
}

/// Where planning one issue stands
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Stage {
    /// Worked whole: the board opens its work item next time it picks it
    Whole,
    /// The maintainer sent the plan back with a note, which the next
    /// planning call carries
    Again {
        /// The note, as written
        note: String,
    },
    /// Being split into sub-issues, one piece at a time
    Splitting {
        /// Why, for the issue's comment
        why: String,
        /// The pieces, blockers first
        pieces: Vec<Piece>,
        /// The sub-issues opened so far, one per piece in order
        #[serde(default)]
        opened: Vec<u64>,
        /// How many of those are linked as sub-issues, with their blockers
        #[serde(default)]
        linked: usize,
        /// Steps in a row the forge refused
        #[serde(default, skip_serializing_if = "is_zero")]
        failures: u32,
    },
    /// Its sub-issues are all closed, and closing it failed this many times
    /// in a row
    Closing {
        /// Tries in a row the forge refused
        failures: u32,
    },
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// Reads the planning call's reply
///
/// # Errors
///
/// Why the reply is not a plan: no JSON object, or a split whose pieces
/// are fewer than two, blank, or wait on a piece that is not earlier.
pub fn read(text: &str) -> Result<Planned, String> {
    #[derive(Deserialize)]
    struct Raw {
        split: bool,
        why: String,
        #[serde(default)]
        pieces: Vec<Piece>,
    }
    let start = text.find('{').ok_or("no JSON object")?;
    let end = text.rfind('}').filter(|&end| end > start);
    let raw: Raw = serde_json::from_str(&text[start..=end.ok_or("no JSON object")?])
        .map_err(|e| format!("not a plan: {e}"))?;
    let why = raw.why.trim().to_owned();
    if !raw.split {
        return Ok(Planned::Whole { why });
    }
    if raw.pieces.len() < 2 {
        return Err("a split with fewer than two pieces".into());
    }
    for (at, piece) in raw.pieces.iter().enumerate() {
        let number = at + 1;
        if piece.title.trim().is_empty() || piece.body.trim().is_empty() {
            return Err(format!("piece {number} is blank"));
        }
        if let Some(&by) = piece.blocked_by.iter().find(|&&by| by == 0 || by >= number) {
            return Err(format!(
                "piece {number} waits on piece {by}, which is not earlier"
            ));
        }
    }
    Ok(Planned::Split {
        why,
        pieces: raw.pieces,
    })
}

/// Kelpie's prompt for planning issue `number`, with the maintainer's note
/// on the last plan when it was sent back
pub fn prompt(number: u64, title: &str, body: &str, note: Option<&str>) -> String {
    let note = note.map_or_else(String::new, |note| {
        format!(
            "The maintainer sent back the last plan for this issue with this note:\n\n{note}\n\n"
        )
    });
    format!(
        "Plan issue #{number}: decide whether it is one pull request or several.\n\n\
         The maintainer's rule: split only where each piece works, tests and ships on \
         its own, as a pull request that merges to main by itself. Never cut a piece \
         short to keep it small. Most issues stay whole.\n\n\
         Read the repo with Read, Grep and Glob, where you have them, to judge the work. \
         Change nothing, and \
         publish nothing: kelpie opens the sub-issues from your reply.\n\n\
         Reply with one JSON object and nothing else, either\n\
         {{\"split\": false, \"why\": \"<one sentence>\"}}\n\
         or\n\
         {{\"split\": true, \"why\": \"<one or two sentences>\", \"pieces\": \
         [{{\"title\": \"<title>\", \"body\": \"<what to build, and its acceptance \
         criteria>\", \"blocked_by\": [<earlier piece numbers>]}}]}}\n\
         Number the pieces from 1 in the order listed, blockers first, at least two. \
         A piece waits only on earlier pieces. Leave parent and blocked-by sections \
         out of each body: kelpie links those on the forge.\n\n\
         {note}--- issue #{number}: {title} ---\n{}\n--- end ---",
        body.trim_end()
    )
}

/// The pieces as a numbered list, each with the pieces it waits on
pub fn list(pieces: &[Piece]) -> String {
    let lines = pieces.iter().enumerate().map(|(at, piece)| {
        let after = match piece.blocked_by.as_slice() {
            [] => String::new(),
            by => {
                let by: Vec<String> = by.iter().map(usize::to_string).collect();
                format!(" (after {})", by.join(", "))
            }
        };
        format!("{}. {}{after}", at + 1, piece.title.trim())
    });
    lines.collect::<Vec<_>>().join("\n")
}

/// The one comment a split leaves on its issue, naming each sub-issue
pub fn comment(why: &str, pieces: &[Piece], opened: &[u64]) -> String {
    let lines = pieces.iter().zip(opened).map(|(piece, number)| {
        let after: Vec<String> = piece
            .blocked_by
            .iter()
            .filter_map(|&by| by.checked_sub(1).and_then(|at| opened.get(at)))
            .map(|n| format!("#{n}"))
            .collect();
        let after = match after.as_slice() {
            [] => String::new(),
            by => format!(", after {}", by.join(", ")),
        };
        format!("- #{number}: {}{after}", piece.title.trim())
    });
    format!(
        "Kelpie planned this issue as {} pull requests. {}\n\n{}\n\n\
         Each is worked on its own, and this issue closes when the last one does.",
        pieces.len(),
        why.trim(),
        lines.collect::<Vec<_>>().join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piece(title: &str, blocked_by: &[usize]) -> Piece {
        Piece {
            title: title.into(),
            body: format!("Build {title}."),
            blocked_by: blocked_by.to_vec(),
        }
    }

    #[test]
    fn a_whole_plan_is_read_from_the_reply_even_inside_a_fence() {
        let text = "```json\n{\"split\": false, \"why\": \"One small change.\"}\n```";
        assert_eq!(
            read(text),
            Ok(Planned::Whole {
                why: "One small change.".into()
            })
        );
    }

    #[test]
    fn a_split_is_read_with_its_pieces_and_what_each_waits_on() {
        let text = r#"{"split": true, "why": "Two slices.", "pieces": [
            {"title": "Schema", "body": "Build Schema."},
            {"title": "Screen", "body": "Build Screen.", "blocked_by": [1]}]}"#;
        assert_eq!(
            read(text),
            Ok(Planned::Split {
                why: "Two slices.".into(),
                pieces: vec![piece("Schema", &[]), piece("Screen", &[1])],
            })
        );
    }

    #[test]
    fn a_split_that_is_not_a_plan_is_refused() {
        let one = r#"{"split": true, "why": "x", "pieces": [{"title": "a", "body": "b"}]}"#;
        assert_eq!(read(one), Err("a split with fewer than two pieces".into()));
        let ahead = r#"{"split": true, "why": "x", "pieces": [
            {"title": "a", "body": "b", "blocked_by": [2]}, {"title": "c", "body": "d"}]}"#;
        assert_eq!(
            read(ahead),
            Err("piece 1 waits on piece 2, which is not earlier".into())
        );
        let blank = r#"{"split": true, "why": "x", "pieces": [
            {"title": "a", "body": "b"}, {"title": " ", "body": "d"}]}"#;
        assert_eq!(read(blank), Err("piece 2 is blank".into()));
        let zero = r#"{"split": true, "why": "x", "pieces": [
            {"title": "a", "body": "b"}, {"title": "c", "body": "d", "blocked_by": [0]}]}"#;
        assert_eq!(
            read(zero),
            Err("piece 2 waits on piece 0, which is not earlier".into())
        );
        assert_eq!(read("I would split it."), Err("no JSON object".into()));
    }

    #[test]
    fn the_prompt_carries_the_issue_and_a_note_sent_back() {
        let first = prompt(7, "Add a thing", "Body.\n", None);
        assert!(first.starts_with("Plan issue #7: "));
        assert!(first.ends_with("--- issue #7: Add a thing ---\nBody.\n--- end ---"));
        assert!(!first.contains("sent back"));
        let again = prompt(7, "Add a thing", "Body.", Some("Keep the API whole."));
        assert!(again.contains("with this note:\n\nKeep the API whole.\n\n--- issue #7"));
    }

    #[test]
    fn the_prompt_never_mentions_cost_or_budget() {
        let text = prompt(7, "t", "b", Some("n")).to_lowercase();
        for word in ["cost", "budget", "token", "spend", "usd", "$"] {
            assert!(!text.contains(word), "{word}");
        }
    }

    #[test]
    fn the_comment_names_each_sub_issue_and_what_it_waits_on() {
        let pieces = [piece("Schema", &[]), piece("Screen", &[1])];
        assert_eq!(
            comment("Two slices.", &pieces, &[901, 902]),
            "Kelpie planned this issue as 2 pull requests. Two slices.\n\n\
             - #901: Schema\n- #902: Screen, after #901\n\n\
             Each is worked on its own, and this issue closes when the last one does."
        );
        assert_eq!(list(&pieces), "1. Schema\n2. Screen (after 1)");
    }

    #[test]
    fn a_plan_in_the_state_file_is_pinned() {
        let plan = Plan {
            issue: 12,
            stage: Stage::Splitting {
                why: "w".into(),
                pieces: vec![piece("a", &[]), piece("b", &[1])],
                opened: vec![901],
                linked: 0,
                failures: 2,
            },
        };
        let text = serde_json::to_string(&plan).unwrap();
        assert_eq!(
            text,
            r#"{"issue":12,"stage":{"kind":"splitting","why":"w","pieces":[{"title":"a","body":"Build a."},{"title":"b","body":"Build b.","blocked_by":[1]}],"opened":[901],"linked":0,"failures":2}}"#
        );
        assert_eq!(serde_json::from_str::<Plan>(&text).unwrap(), plan);
        let again = r#"{"issue":3,"stage":{"kind":"again","note":"n"}}"#;
        assert!(serde_json::from_str::<Plan>(again).is_ok());
        let closing = r#"{"issue":4,"stage":{"kind":"closing","failures":1}}"#;
        assert!(serde_json::from_str::<Plan>(closing).is_ok());
    }

    #[test]
    fn a_piece_with_a_key_kelpie_does_not_know_is_still_read() {
        let text = r#"{"split": true, "why": "w", "pieces": [
            {"title": "a", "body": "b", "size": "small"}, {"title": "c", "body": "d"}]}"#;
        assert!(matches!(read(text), Ok(Planned::Split { .. })));
    }
}
