//! The rulings that were answered, kept for `status` and the retro
//!
//! One JSON object a line in `<project>/rulings.jsonl`, appended as an
//! answer is saved. The state file keeps only the rulings that wait, so
//! without this nothing could say how an earlier one went. A file that
//! cannot be read or written costs that history and nothing else.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ports::Timestamp;
use crate::runner::Answer;
use crate::settings::RulingName;
use crate::state::{Ruling, RulingKind, Stuck};
use crate::ticket::TicketId;

/// The most of a ruling's question kept, in characters
const QUESTION: usize = 1_200;

/// Who answered a ruling
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runner) enum Answerer {
    /// The maintainer, in its thread, at the terminal or on ntfy
    Maintainer,
    /// The project manager, for the maintainer
    Pm,
}

/// A ruling and how it was answered
// wire format: changing this is a breaking change to the rulings file. It
// names no `deny_unknown_fields`, so a line an older build wrote still reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::runner) struct Record {
    pub(in crate::runner) id: TicketId,
    pub(in crate::runner) at: Timestamp,
    // When it was raised, if the state file said
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::runner) raised: Option<Timestamp>,
    pub(in crate::runner) issue: Option<TicketId>,
    pull_request: Option<u64>,
    pub(in crate::runner) asked: String,
    pub(in crate::runner) answer: String,
    // Why a turn failed, for a failed turn
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::runner) stem: Option<String>,
    // Whether the project manager answered for the maintainer
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    by_pm: bool,
    // The kind of ruling, as `rulings.pm_decides` names it; none for `unlabelled`
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "known_kind"
    )]
    pub(in crate::runner) kind: Option<RulingName>,
    // The option the answer is, when it is one of the ruling's
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chose: Option<char>,
    // The commit the worktree stood at when the ruling was raised, for a
    // timed-out or failed turn
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::runner) head: Option<String>,
}

impl Record {
    /// `ruling`, answered with `answer` at `at` by `by`
    pub(in crate::runner) fn of(
        ruling: &Ruling,
        answer: &Answer,
        at: Timestamp,
        by: Answerer,
    ) -> Self {
        let chose = crate::runner::voice::chosen(&ruling.kind, answer);
        let answer = match answer {
            Answer::Yes => "yes".to_owned(),
            Answer::No(note) => format!("no: {note}"),
            Answer::Rework(note) => format!("rework: {note}"),
            Answer::Text(text) => text.clone(),
            Answer::Trust(head) => format!("yes {head}"),
            Answer::Do(action) => action.word().to_owned(),
        };
        Self {
            id: ruling.id,
            at,
            raised: ruling.raised,
            issue: ruling.issue,
            pull_request: ruling.pull_request,
            asked: asked_of(ruling),
            stem: stem_of(ruling),
            answer: clip(&answer, QUESTION),
            by_pm: by == Answerer::Pm,
            kind: Some(RulingName::of(&ruling.kind)),
            chose,
            head: ruling.stuck_head.clone(),
        }
    }

    /// Who answered it
    pub(in crate::runner) fn by(&self) -> Answerer {
        match self.by_pm {
            true => Answerer::Pm,
            false => Answerer::Maintainer,
        }
    }

    /// Adds the record to the file at `file`
    ///
    /// # Errors
    ///
    /// What went wrong, for the log.
    pub(in crate::runner) fn append(&self, file: &Path) -> Result<(), String> {
        let line = serde_json::to_string(self).map_err(|e| e.to_string())?;
        let opened = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)
            .and_then(|mut f| writeln!(f, "{line}"));
        opened.map_err(|e| format!("cannot add to {}: {}", file.display(), e.kind()))
    }
}

// The kind a line names. `unlabelled` reads as none; any other unknown kind fails.
fn known_kind<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<RulingName>, D::Error> {
    match Option::<String>::deserialize(deserializer)? {
        Some(name) if name == "unlabelled" => Ok(None),
        Some(name) => RulingName::named(&name)
            .map(Some)
            .map_err(serde::de::Error::custom),
        None => Ok(None),
    }
}

// Why a failed turn failed, as `rulings.jsonl` keeps it, so a later case can
// say what each of the item's turn rulings was for.
fn stem_of(ruling: &Ruling) -> Option<String> {
    let RulingKind::Stuck(Stuck::TurnFailed { why, .. }) = &ruling.kind else {
        return None;
    };
    let why = why.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(clip(&format!("turn-failed: {why}"), QUESTION))
}

// What `ruling` asks, less the table of options, as `rulings.jsonl` keeps it.
fn asked_of(ruling: &Ruling) -> String {
    let (brief, _) = crate::runner::voice::brief(ruling);
    let asked = brief.split("\n\n| | Option").next().unwrap_or_default();
    clip(asked.trim_start_matches('#').trim(), QUESTION)
}

// `text` cut to `most` characters, with an ellipsis.
fn clip(text: &str, most: usize) -> String {
    match text.char_indices().nth(most) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}

/// Every ruling answered in the file at `file`, oldest first; a line that
/// does not read is skipped, and so is one the panel decided, since neither
/// the maintainer nor the project manager answered it
pub(in crate::runner) fn records(file: &Path) -> Vec<Record> {
    try_records(file).unwrap_or_default()
}

/// [`records`], or why the file could not be read
///
/// # Errors
///
/// When the file exists and cannot be read. A file not there yet holds no ruling.
pub(in crate::runner) fn try_records(file: &Path) -> std::io::Result<Vec<Record>> {
    let text = match std::fs::read_to_string(file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        read => read?,
    };
    Ok((text.lines())
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|line| line.get("by_panel") != Some(&serde_json::Value::Bool(true)))
        .filter_map(|line| serde_json::from_value(line).ok())
        .collect())
}

/// How many of `records` the project manager answered, by kind, for `status`
pub(in crate::runner) fn pm_answers(records: &[Record]) -> BTreeMap<String, u32> {
    let mut counts = BTreeMap::new();
    for kind in (records.iter()).filter(|r| r.by_pm).filter_map(|r| r.kind) {
        *counts.entry(kind.as_str().to_owned()).or_default() += 1;
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::ticket;

    // A ruling id from before a ruling was named by its ticket, where the
    // counter named no ticket at all (0), is an id no record can carry: the
    // line is skipped as any line that does not read is.
    #[test]
    fn a_line_whose_id_names_no_ticket_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("rulings.jsonl");
        std::fs::write(
            &file,
            "{\"id\":0,\"at\":100,\"issue\":7,\"pull_request\":null,\"asked\":\"q\",\"answer\":\"yes\"}\n",
        )
        .unwrap();
        assert_eq!(records(&file), Vec::new());
    }

    // An answered ruling of a kind that has since gone stays in the history.
    #[test]
    fn an_answered_ruling_of_a_removed_kind_is_still_read() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("rulings.jsonl");
        let text = serde_json::json!({
            "id": 3, "at": 100, "issue": 7, "pull_request": null,
            "asked": "q", "answer": "yes", "kind": "local-model-spilled",
        });
        std::fs::write(&file, format!("{text}\n")).unwrap();
        let records = records(&file);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, ticket(3));
    }

    // An `unlabelled` line reads with no kind and keeps every other field.
    #[test]
    fn an_answered_unlabelled_ruling_is_read_with_no_kind() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("rulings.jsonl");
        let text = serde_json::json!({
            "id": 4, "at": 100, "issue": 7, "pull_request": null,
            "asked": "q", "answer": "yes", "by_pm": true, "kind": "unlabelled",
        });
        std::fs::write(&file, format!("{text}\n")).unwrap();
        let records = records(&file);
        assert_eq!(records.len(), 1);
        assert_eq!((records[0].id, records[0].kind), (ticket(4), None));
        assert_eq!(records[0].by(), Answerer::Pm);
    }

    // A kind no build ever had fails the line, which is skipped.
    #[test]
    fn a_line_of_a_kind_no_build_had_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("rulings.jsonl");
        let text = serde_json::json!({
            "id": 5, "at": 100, "issue": 7, "pull_request": null,
            "asked": "q", "answer": "yes", "kind": "no-such-kind",
        });
        std::fs::write(&file, format!("{text}\n")).unwrap();
        assert_eq!(records(&file), Vec::new());
    }

    // A line the panel decided, from a build that had one, still reads its
    // other fields, but it is no answer the maintainer or the project manager
    // gave, so it is skipped.
    #[test]
    fn a_line_from_a_build_with_the_panel_reads_and_one_the_panel_decided_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("rulings.jsonl");
        let line = |id: u64, by_panel: bool| {
            serde_json::json!({
                "id": id, "at": 100, "issue": 7, "pull_request": 71,
                "asked": "Merge?", "answer": "yes", "kind": "merge", "chose": "A",
                "by_panel": by_panel, "skipped": "draining",
                "verdict": {"work": "feat", "tier": "full", "risky": false, "shadow": true},
                "ballots": [{"voter": "advisor", "vote": null}, {"voter": "worker", "vote":
                    {"choice": "A", "why": "w", "design": false}}],
                "overtaken": ["worker"],
            })
        };
        std::fs::write(&file, format!("{}\n{}\n", line(3, false), line(4, true))).unwrap();

        let records = records(&file);

        let ids: Vec<TicketId> = records.iter().map(|r| r.id).collect();
        assert_eq!(ids, [ticket(3)]);
        assert_eq!(records[0].by(), Answerer::Maintainer);
    }

    #[test]
    fn status_counts_the_rulings_the_project_manager_answered_by_kind() {
        let rebase = Ruling::new(
            ticket(3),
            "rebase",
            RulingKind::from(Stuck::Rebase {
                why: "conflicts".into(),
                review: None,
            }),
        );
        let question = asking(4, None, "Which flag?");
        let yes = Answer::Yes;
        let at = Timestamp(1);
        let records = [
            Record::of(&rebase, &yes, at, Answerer::Pm),
            Record::of(&rebase, &yes, at, Answerer::Pm),
            Record::of(&question, &yes, at, Answerer::Maintainer),
        ];

        let counts = pm_answers(&records);

        assert_eq!(counts, BTreeMap::from([("rebase".to_owned(), 2)]));
    }

    fn asking(id: u64, pull_request: Option<u64>, asked: &str) -> Ruling {
        let kind = crate::state::RulingKind::Question {
            asked: asked.into(),
            resume: crate::state::Resume::Nothing,
        };
        Ruling {
            issue: Some(ticket(7)),
            pull_request,
            ..Ruling::new(ticket(id), asked, kind)
        }
    }

    fn failing(id: u64, why: &str) -> Ruling {
        let kind = RulingKind::from(Stuck::TurnFailed {
            why: why.into(),
            phase: crate::work_item::Phase::Implement,
            retry: crate::work_item::Turn::Due,
            restore: Vec::new(),
            setup: false,
        });
        Ruling {
            issue: Some(ticket(7)),
            pull_request: Some(71),
            ..Ruling::new(ticket(id), why, kind)
        }
    }

    // A timed-out or failed turn's record keeps the head its ruling was raised
    // at, and one raised with no head keeps none.
    #[test]
    fn a_record_keeps_its_rulings_head() {
        let timed_out = Ruling {
            issue: Some(ticket(7)),
            pull_request: Some(71),
            stuck_head: Some("c0ffee1".into()),
            ..Ruling::new(
                ticket(71),
                "q",
                RulingKind::from(Stuck::TurnTimeout { phase: None }),
            )
        };
        let mut failed = failing(72, "harness exited with 1");
        failed.stuck_head = Some("9beef12".into());
        let headless = failing(73, "no worktree");

        let kept = Record::of(&timed_out, &Answer::Yes, Timestamp(1), Answerer::Pm);
        let other = Record::of(&failed, &Answer::Yes, Timestamp(1), Answerer::Pm);
        let none = Record::of(&headless, &Answer::Yes, Timestamp(1), Answerer::Pm);

        assert_eq!(
            (kept.head.as_deref(), other.head.as_deref()),
            (Some("c0ffee1"), Some("9beef12"))
        );
        assert_eq!(none.head, None);
    }

    // A line a build that kept no head wrote still reads, with none.
    #[test]
    fn a_line_an_older_build_wrote_reads_with_no_head() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("rulings.jsonl");
        let line = serde_json::json!({
            "id": 71, "at": 100, "raised": 90, "issue": 7, "pull_request": 71,
            "asked": "q", "answer": "yes", "kind": "turn-timeout",
        });
        assert_eq!(line.get("head"), None);
        std::fs::write(&file, format!("{line}\n")).unwrap();

        let records = records(&file);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].head, None);
        assert_eq!(records[0].kind, Some(RulingName::TurnTimeout));
    }

    // A line an older build wrote with a `stem` still reads: the history keeps
    // why each failed turn failed, whoever answered it.
    #[test]
    fn a_line_with_a_stem_still_reads() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("rulings.jsonl");
        let line = serde_json::json!({
            "id": 71, "at": 100, "issue": 7, "pull_request": 71, "asked": "q",
            "answer": "yes", "kind": "turn-failed", "by_pm": true,
            "stem": "turn-failed: harness exited with 1",
        });
        std::fs::write(&file, format!("{line}\n")).unwrap();

        let records = records(&file);

        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].stem.as_deref(),
            Some("turn-failed: harness exited with 1")
        );
        assert_eq!(records[0].by(), Answerer::Pm);
    }
}
