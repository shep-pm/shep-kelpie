//! Planning: whether a ready issue is one pull request or several
//!
//! Before the board's pick opens a work item, a planning call reads the
//! repo and answers in JSON: keep the issue whole, or split it into pieces,
//! each a complete vertical slice with the earlier pieces it waits on. A
//! split becomes sub-issues of the issue, worked in its place. This is a
//! level above the work split, which stays the worker's inside one work item.
//!
//! The call also picks a worker for the issue kept whole, or for each piece,
//! from the three in [`PICKS`]. The runner applies it as that `worker:`
//! label, made on the repo when missing, unless the issue already carries
//! one. A reply that names none, or any other, falls back to the project's
//! own worker.

use serde::{Deserialize, Serialize};

use crate::board::{WORKER_LABEL, WorkerLabel, parse_worker_value, worker_override};
use crate::ports::NewLabel;

/// A worker the planning call may pick, and when it fits
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pick {
    /// The `worker:` label it applies
    pub label: NewLabel,
    /// When it fits, as the planning prompt says
    pub when: &'static str,
}

impl Pick {
    /// The label's value, `<model>-<effort>`, as a reply names it
    #[must_use]
    pub fn value(&self) -> &'static str {
        let name = self.label.name;
        name.strip_prefix(WORKER_LABEL).unwrap_or(name)
    }
}

/// The only workers the planning call may pick, the default first
pub const PICKS: [Pick; 3] = [
    Pick {
        label: NewLabel {
            name: "worker:sonnet-high",
            color: "c5def5",
            description: "Worker on Sonnet at high effort, the planner's default pick",
        },
        when: "The default: any feature, new behaviour across modules, state, persistence, \
               timing, process lifecycle",
    },
    Pick {
        label: NewLabel {
            name: "worker:sonnet-medium",
            color: "d4c5f9",
            description: "Worker on Sonnet at medium effort, for small or mechanical work",
        },
        when: "Small or mechanical: docs, config, a rename, test-only, one module, no state \
               or concurrency",
    },
    Pick {
        label: NewLabel {
            name: "worker:opus-high",
            color: "5319e7",
            description: "Worker on Opus at high effort, where a bad first build is hard to undo",
        },
        when: "Only where a bad first build is hard to undo: migrations, credentials, \
               irreversible operations",
    },
];

/// What the planning call decided
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Planned {
    /// One pull request
    Whole {
        /// Why, in a sentence
        why: String,
        /// The worker it named, `<model>-<effort>` as written, if any
        worker: Option<String>,
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
    /// The worker it named, `<model>-<effort>` as written, if any
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
}

/// Whether `labels` already carries a `worker:` label, which wins over a
/// plan's pick: even one kelpie cannot read (unknown, or more than one) is
/// still an existing choice, never the pick's to overwrite.
pub fn already_has_worker(labels: &[String]) -> bool {
    !matches!(worker_override(labels), Ok(None))
}

/// The label this piece or kept-whole issue's pick applies, when it named
/// one of [`PICKS`]
///
/// `None` when it named nothing, or any other worker, even one a `worker:`
/// label the maintainer adds could run, and the issue or sub-issue then
/// falls back to the project's own worker.
pub fn resolved_label(named: Option<&str>) -> Option<NewLabel> {
    let named = named?;
    PICKS.iter().find(|p| p.value() == named).map(|p| p.label)
}

/// Why a worker the plan named, or a label it resolved to, is not the one
/// that ended up running the issue or sub-issue, in one phrase
///
/// A name [`resolved_label`] reads as `Some` gets a phrase that does not
/// call it unsupported, for a caller whose write left no error to name.
pub fn fallback_reason(named: Option<&str>) -> String {
    let Some(named) = named else {
        return "the plan named no worker".to_owned();
    };
    if resolved_label(Some(named)).is_some() {
        return format!(
            "`{named}` is one of the planner's picks, but its label could not be confirmed"
        );
    }
    if matches!(parse_worker_value(named), Ok(WorkerLabel::Local)) {
        return "the plan named `local`, which only the maintainer picks by hand".to_owned();
    }
    let picks: Vec<&str> = PICKS.iter().map(Pick::value).collect();
    format!(
        "`{named}` is not one of the planner's picks ({})",
        picks.join(", ")
    )
}

/// What a split's comment says of one sub-issue's worker
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PieceWorker {
    /// It carries a `worker:` label, from its pick or its parent's labels
    Labelled,
    /// It carries none, so the project's own worker runs it, for this reason
    Defaulted(String),
    /// Reading its labels back failed, with the forge's error
    Unread(String),
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
        #[serde(default)]
        worker: Option<String>,
    }
    let start = text.find('{').ok_or("no JSON object")?;
    let end = text.rfind('}').filter(|&end| end > start);
    let mut raw: Raw = serde_json::from_str(&text[start..=end.ok_or("no JSON object")?])
        .map_err(|e| format!("not a plan: {e}"))?;
    let why = raw.why.trim().to_owned();
    if !raw.split {
        return Ok(Planned::Whole {
            why,
            worker: named(raw.worker),
        });
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
    for piece in &mut raw.pieces {
        piece.worker = named(piece.worker.take());
    }
    Ok(Planned::Split {
        why,
        pieces: raw.pieces,
    })
}

// A named value, trimmed and with a blank one taken as absent.
fn named(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// Kelpie's prompt for planning issue `number`, with the maintainer's note
/// on the last plan when it was sent back
///
/// `default` names the worker that runs an issue whose reply picks none,
/// such as "the project's default worker, `<model>` at high effort".
pub fn prompt(number: u64, title: &str, body: &str, note: Option<&str>, default: &str) -> String {
    let note = note.map_or_else(String::new, |note| {
        format!(
            "The maintainer sent back the last plan for this issue with this note:\n\n{note}\n\n"
        )
    });
    format!(
        "Plan issue #{number}: decide whether it is one pull request or several, and pick \
         the worker for it, or for each piece.\n\n\
         The maintainer's rule: split only where each piece works, tests and ships on \
         its own, as a pull request that merges to main by itself. Never cut a piece \
         short to keep it small. Most issues stay whole.\n\n\
         {}\n\n\
         Read the repo with Read, Grep and Glob, where you have them, to judge the work. \
         Change nothing, and \
         publish nothing: kelpie opens the sub-issues from your reply.\n\n\
         Reply with one JSON object and nothing else, either\n\
         {{\"split\": false, \"why\": \"<one sentence>\", \"worker\": \"<worker>\"}}\n\
         or\n\
         {{\"split\": true, \"why\": \"<one or two sentences>\", \"pieces\": \
         [{{\"title\": \"<title>\", \"body\": \"<what to build, and its acceptance \
         criteria>\", \"blocked_by\": [<earlier piece numbers>], \
         \"worker\": \"<worker>\"}}]}}\n\
         Number the pieces from 1 in the order listed, blockers first, at least two. \
         A piece waits only on earlier pieces. Leave parent and blocked-by sections \
         out of each body: kelpie links those on the forge. `worker` is optional on \
         either reply.\n\n\
         {note}--- issue #{number}: {title} ---\n{}\n--- end ---",
        picks(default),
        body.trim_end()
    )
}

// The prompt's worker section: the only picks, when each fits, and what
// runs when the reply names none.
fn picks(default: &str) -> String {
    let rows = PICKS
        .iter()
        .map(|p| format!("| `{}` | {} |", p.value(), p.when));
    format!(
        "Pick the worker from these three, and no other:\n\n\
         | worker | when |\n| --- | --- |\n{}\n\n\
         Leave `worker` out to run {default}.",
        rows.collect::<Vec<_>>().join("\n")
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
///
/// `workers` says, for each piece in order, what its sub-issue's worker
/// came to once the split finished. A labelled one leaves nothing to say.
pub fn comment(why: &str, pieces: &[Piece], opened: &[u64], workers: &[PieceWorker]) -> String {
    let lines = pieces
        .iter()
        .zip(opened)
        .zip(workers)
        .map(|((piece, number), worker)| {
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
            let defaulted = match worker {
                PieceWorker::Labelled => String::new(),
                PieceWorker::Defaulted(reason) => {
                    format!(", worker defaulted to the project's: {reason}")
                }
                PieceWorker::Unread(error) => {
                    format!(", worker not confirmed: could not read #{number}'s labels: {error}")
                }
            };
            format!("- #{number}: {}{after}{defaulted}", piece.title.trim())
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
            worker: None,
        }
    }

    #[test]
    fn a_whole_plan_is_read_from_the_reply_even_inside_a_fence() {
        let text = "```json\n{\"split\": false, \"why\": \"One small change.\"}\n```";
        assert_eq!(
            read(text),
            Ok(Planned::Whole {
                why: "One small change.".into(),
                worker: None,
            })
        );
    }

    #[test]
    fn a_whole_plan_s_worker_is_read_and_trimmed() {
        let text = r#"{"split": false, "why": "x", "worker": " sonnet-high "}"#;
        assert_eq!(
            read(text),
            Ok(Planned::Whole {
                why: "x".into(),
                worker: Some("sonnet-high".into()),
            })
        );
        let blank = r#"{"split": false, "why": "x", "worker": "  "}"#;
        assert_eq!(
            read(blank),
            Ok(Planned::Whole {
                why: "x".into(),
                worker: None,
            })
        );
    }

    #[test]
    fn a_piece_s_worker_is_read() {
        let text = r#"{"split": true, "why": "x", "pieces": [
            {"title": "a", "body": "b", "worker": "opus-max"},
            {"title": "c", "body": "d"}]}"#;
        let Ok(Planned::Split { pieces, .. }) = read(text) else {
            panic!("expected a split")
        };
        assert_eq!(pieces[0].worker, Some("opus-max".into()));
        assert_eq!(pieces[1].worker, None);
    }

    #[test]
    fn an_unreadable_worker_label_still_wins_over_a_plan_s_pick() {
        assert!(!already_has_worker(&["ready-for-agent".to_owned()]));
        assert!(already_has_worker(&["worker:sonnet-medium".to_owned()]));
        // Neither a label `worker_override` cannot parse, nor more than
        // one, is "no label": both still win over the plan's pick.
        assert!(already_has_worker(&["worker:nope".to_owned()]));
        assert!(already_has_worker(&[
            "worker:sonnet-medium".to_owned(),
            "worker:opus-low".to_owned()
        ]));
    }

    #[test]
    fn a_worker_the_reply_names_is_resolved_only_when_it_is_one_of_the_picks() {
        assert_eq!(resolved_label(None), None);
        assert_eq!(resolved_label(Some("nope")), None);
        assert_eq!(resolved_label(Some("local")), None);
        // Workers a label the maintainer adds could run, but never a pick.
        for other in ["opus-max", "sonnet-xhigh", "sonnet-low", "haiku-high"] {
            assert_eq!(resolved_label(Some(other)), None, "{other}");
        }
        let names: Vec<_> = ["sonnet-high", "sonnet-medium", "opus-high"]
            .map(|v| resolved_label(Some(v)).map(|l| l.name))
            .into();
        assert_eq!(
            names,
            [
                Some("worker:sonnet-high"),
                Some("worker:sonnet-medium"),
                Some("worker:opus-high")
            ]
        );
    }

    #[test]
    fn every_pick_is_a_worker_label_the_board_reads_as_a_model() {
        for pick in PICKS {
            let labels = [pick.label.name.to_owned()];
            assert!(
                matches!(worker_override(&labels), Ok(Some(WorkerLabel::Model(_)))),
                "{}",
                pick.label.name
            );
            assert_eq!(pick.label.color.len(), 6, "{}", pick.label.name);
            assert!(pick.label.description.len() <= 100, "{}", pick.label.name);
        }
    }

    #[test]
    fn why_a_worker_falls_back_names_what_the_reply_gave() {
        assert_eq!(fallback_reason(None), "the plan named no worker");
        assert!(fallback_reason(Some("nope-medium")).contains("`nope-medium`"));
        assert_eq!(
            fallback_reason(Some("opus-max")),
            "`opus-max` is not one of the planner's picks (sonnet-high, sonnet-medium, opus-high)"
        );
    }

    #[test]
    fn why_a_worker_falls_back_never_calls_a_resolved_one_unsupported() {
        // A name that does resolve still reaches `fallback_reason` when
        // its label could not be confirmed on the sub-issue (the write is
        // best effort): it must not then claim that name is unsupported.
        let text = fallback_reason(Some("sonnet-medium"));
        assert!(!text.contains("is not one of"), "{text}");
        assert!(text.contains("`sonnet-medium`"), "{text}");
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

    const DEFAULT: &str = "the project's default worker, `claude-sonnet-5-5` at high effort";

    #[test]
    fn the_prompt_carries_the_issue_and_a_note_sent_back() {
        let first = prompt(7, "Add a thing", "Body.\n", None, DEFAULT);
        assert!(first.starts_with("Plan issue #7: "));
        assert!(first.ends_with("--- issue #7: Add a thing ---\nBody.\n--- end ---"));
        assert!(!first.contains("sent back"));
        let again = prompt(
            7,
            "Add a thing",
            "Body.",
            Some("Keep the API whole."),
            DEFAULT,
        );
        assert!(again.contains("with this note:\n\nKeep the API whole.\n\n--- issue #7"));
    }

    #[test]
    fn the_prompt_names_only_the_three_picks_when_each_fits_and_the_default() {
        let text = prompt(7, "t", "b", None, DEFAULT);
        let section = "Pick the worker from these three, and no other:\n\n\
            | worker | when |\n\
            | --- | --- |\n\
            | `sonnet-high` | The default: any feature, new behaviour across modules, state, \
            persistence, timing, process lifecycle |\n\
            | `sonnet-medium` | Small or mechanical: docs, config, a rename, test-only, one \
            module, no state or concurrency |\n\
            | `opus-high` | Only where a bad first build is hard to undo: migrations, \
            credentials, irreversible operations |\n\n\
            Leave `worker` out to run the project's default worker, `claude-sonnet-5-5` at \
            high effort.\n\n";
        assert!(text.contains(section), "{text}");
        for other in ["xhigh", "haiku", "fable", "-max", "-low", ", low"] {
            assert!(!text.contains(other), "{other}");
        }
    }

    #[test]
    fn the_prompt_never_mentions_cost_or_budget() {
        let text = prompt(7, "t", "b", Some("n"), DEFAULT).to_lowercase();
        for word in ["cost", "budget", "token", "spend", "usd", "$"] {
            assert!(!text.contains(word), "{word}");
        }
    }

    #[test]
    fn the_comment_names_each_sub_issue_and_what_it_waits_on() {
        let pieces = [piece("Schema", &[]), piece("Screen", &[1])];
        assert_eq!(
            comment(
                "Two slices.",
                &pieces,
                &[901, 902],
                &[PieceWorker::Labelled, PieceWorker::Labelled]
            ),
            "Kelpie planned this issue as 2 pull requests. Two slices.\n\n\
             - #901: Schema\n- #902: Screen, after #901\n\n\
             Each is worked on its own, and this issue closes when the last one does."
        );
        assert_eq!(list(&pieces), "1. Schema\n2. Screen (after 1)");
    }

    #[test]
    fn the_comment_says_when_a_piece_s_worker_defaulted() {
        let pieces = [piece("Schema", &[]), piece("Screen", &[])];
        let defaulted = PieceWorker::Defaulted("`nope` is not one of the planner's picks".into());
        let text = comment(
            "Two slices.",
            &pieces,
            &[901, 902],
            &[PieceWorker::Labelled, defaulted],
        );
        assert!(!text.contains("- #901: Schema,"), "{text}");
        assert!(
            text.contains("- #902: Screen, worker defaulted to the project's: `nope`"),
            "{text}"
        );
    }

    #[test]
    fn a_sub_issue_the_forge_could_not_read_back_is_not_called_defaulted() {
        // A forge error at comment time is not the same as confirming no
        // `worker:` label landed: the comment must not claim one.
        let pieces = [piece("Schema", &[])];
        let unread = PieceWorker::Unread("gh failed: no issue #901".into());
        let text = comment("One slice.", &pieces, &[901], &[unread]);
        assert!(!text.contains("defaulted"), "{text}");
        assert!(
            text.contains(
                "- #901: Schema, worker not confirmed: could not read #901's labels: gh failed: \
                 no issue #901"
            ),
            "{text}"
        );
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
