//! A ruling's answer in plain words, read by the ruling's kind
//!
//! `shep kelpie rule` and replies on ntfy read an answer this way: `yes`, or
//! `no <note>`, for a ruling that asks yes or no, those or `rework <note>`
//! for a merge ruling, and any text at all for the worker's question or a
//! contributor's request for a decision, a lone `yes` included. What they read goes to the runner as `rule`'s own `<id>
//! yes`, `<id> no <note>`, `<id> rework <note>` or `<id> answer <text>`.

use super::{Action, Answer};
use crate::state::{Held, RulingKind};
use crate::ticket::TicketId;

/// The note a worker gets from a no or a rework that was given by a reaction
/// or a tick, which carries none
pub const NO_NOTE: &str = "The maintainer gave no note: read the pull request, its reviews \
                           and CI again, and fix what looks wrong.";

/// Which answer a ruling takes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wants {
    /// A worker's question, or a contributor's request for a decision,
    /// answered with `<id> answer <text>`
    Answer,
    /// The merge ruling: `<id> yes`, `<id> no <note>` or `<id> rework <note>`
    Merge,
    /// Every other ruling: `<id> yes`, or `<id> no <note>`
    YesOrNo,
}

impl Wants {
    /// What a ruling of `kind` takes
    pub fn of(kind: &RulingKind) -> Self {
        match kind {
            RulingKind::Question { .. }
            | RulingKind::Contribution(
                Held::Question { .. } | Held::Waiting | Held::NoEdits | Held::CannotAdopt { .. },
            ) => Self::Answer,
            // Kelpie's own merge ruling and a contribution's take the same
            // three answers.
            RulingKind::Merge { .. } | RulingKind::Contribution(Held::Merge { .. }) => Self::Merge,
            RulingKind::Stuck(..)
            | RulingKind::AgentFiles { .. }
            | RulingKind::ForeignChange { .. }
            | RulingKind::Contribution(..)
            | RulingKind::FollowUp { .. } => Self::YesOrNo,
        }
    }
}

/// `words` as the answer to a ruling that wants `wants`
///
/// A question's text may still start with the older `answer`, which is
/// dropped. `yes`, `no` and `rework` are read in any case.
///
/// # Errors
///
/// A message saying what the ruling takes, when `words` is not that.
pub fn read_answer(wants: Wants, words: &str) -> Result<Answer, String> {
    let words = words.trim();
    match wants {
        Wants::Answer => {
            let text = match words.split_once(char::is_whitespace) {
                Some(("answer", rest)) => rest.trim_start(),
                _ => words,
            };
            if text.is_empty() {
                return Err("it is the worker's question, so it takes your answer".into());
            }
            Ok(Answer::Text(text.to_owned()))
        }
        Wants::Merge | Wants::YesOrNo => {
            let (first, rest) = words.split_once(char::is_whitespace).unwrap_or((words, ""));
            let rest = rest.trim();
            let merge = wants == Wants::Merge;
            match (first.to_ascii_lowercase().as_str(), rest) {
                ("yes", "") => Ok(Answer::Yes),
                // Only a contribution's ruling takes the head, and `rule` says so
                // for any other.
                ("yes", head) if !merge && super::trigger::is_head(head) => {
                    Ok(Answer::Trust(head.to_owned()))
                }
                ("no", "") => Err("a no takes a note for the worker: `no <note>`".into()),
                ("no", note) => Ok(Answer::No(note.to_owned())),
                ("rework", "") if merge => {
                    Err("a rework takes a note for the worker: `rework <note>`".into())
                }
                ("rework", note) if merge => Ok(Answer::Rework(note.to_owned())),
                ("rerun", "") => Ok(Answer::Do(Action::Rerun)),
                ("restore", "") => Ok(Answer::Do(Action::Restore)),
                _ if merge => Err("it takes `yes`, `no <note>` or `rework <note>`".into()),
                _ => Err("it takes `yes`, or `no <note>`".into()),
            }
        }
    }
}

impl Answer {
    /// `rule`'s params for this answer to ruling `id`
    pub fn params(&self, id: TicketId) -> String {
        match self {
            Self::Yes => format!("{id} yes"),
            Self::No(note) => format!("{id} no {note}"),
            Self::Rework(note) => format!("{id} rework {note}"),
            Self::Text(text) => format!("{id} answer {text}"),
            Self::Trust(head) => format!("{id} yes {head}"),
            Self::Do(action) => format!("{id} {}", action.word()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::RuleAnswer;
    use crate::runner::trigger::read_rule;
    use crate::state::Stuck;
    use crate::test::ticket;

    #[test]
    fn a_question_takes_an_answer_a_merge_ruling_a_rework_too_and_any_other_a_yes_or_no() {
        let question = RulingKind::Question {
            asked: String::new(),
            resume: crate::state::Resume::Nothing,
        };
        assert_eq!(Wants::of(&question), Wants::Answer);
        let merge = RulingKind::Merge {
            head: "abc".into(),
            unreviewed: None,
            open_threads: None,
            held_by: Vec::new(),
            unread_head: false,
            note_fix: false,
            late_fix: false,
            nits: None,
            disputes: None,
            rivals: Vec::new(),
        };
        assert_eq!(Wants::of(&merge), Wants::Merge);
        assert_eq!(Wants::of(&Stuck::Closed.into()), Wants::YesOrNo);
        let flagged = RulingKind::Contribution(crate::state::Held::Security {
            head: "abc".into(),
            lines: Vec::new(),
        });
        assert_eq!(Wants::of(&flagged), Wants::YesOrNo);
        let waiting = RulingKind::Contribution(crate::state::Held::Waiting);
        assert_eq!(Wants::of(&waiting), Wants::Answer);
    }

    #[test]
    fn a_merge_ruling_takes_a_rework_with_a_note_and_no_other_ruling_does() {
        let merge = |words| read_answer(Wants::Merge, words);
        assert_eq!(merge("yes"), Ok(Answer::Yes));
        assert_eq!(
            merge("no fix the nit"),
            Ok(Answer::No("fix the nit".into()))
        );
        assert_eq!(
            merge(" Rework  split the parser out "),
            Ok(Answer::Rework("split the parser out".into()))
        );
        assert!(merge("rework").unwrap_err().contains("`rework <note>`"));
        assert_eq!(
            merge("maybe"),
            Err("it takes `yes`, `no <note>` or `rework <note>`".into())
        );
        assert_eq!(
            read_answer(Wants::YesOrNo, "rework split it"),
            Err("it takes `yes`, or `no <note>`".into())
        );
    }

    #[test]
    fn a_yes_naming_a_head_is_a_trust_and_no_merge_ruling_takes_one() {
        assert_eq!(
            read_answer(Wants::YesOrNo, "yes 6b0606e"),
            Ok(Answer::Trust("6b0606e".into()))
        );
        assert!(read_answer(Wants::YesOrNo, "yes 6b0606e now").is_err());
        assert!(read_answer(Wants::YesOrNo, "yes please").is_err());
        assert!(read_answer(Wants::Merge, "yes 6b0606e").is_err());
        assert_eq!(
            Answer::Trust("6b0606e".into()).params(ticket(30)),
            "30 yes 6b0606e"
        );
    }

    #[test]
    fn a_yes_or_no_ruling_takes_yes_or_a_no_with_a_note() {
        let read = |words| read_answer(Wants::YesOrNo, words);
        assert_eq!(read("yes"), Ok(Answer::Yes));
        assert_eq!(read(" Yes "), Ok(Answer::Yes));
        assert_eq!(
            read("no rename the flag"),
            Ok(Answer::No("rename the flag".into()))
        );
        assert_eq!(
            read("No, it's wrong"),
            Err("it takes `yes`, or `no <note>`".into())
        );
        assert!(read("no").unwrap_err().contains("`no <note>`"));
        for refused in ["", "yes please", "looks good", "answer yes"] {
            assert!(read(refused).is_err(), "{refused:?}");
        }
    }

    #[test]
    fn a_yes_to_a_ruling_that_asks_for_text_is_text() {
        let read = |words| read_answer(Wants::Answer, words);
        assert_eq!(read("yes"), Ok(Answer::Text("yes".into())));
        assert_eq!(read("no idea"), Ok(Answer::Text("no idea".into())));
        assert_eq!(
            read("use the blue one"),
            Ok(Answer::Text("use the blue one".into()))
        );
        assert_eq!(read("answer use it"), Ok(Answer::Text("use it".into())));
        assert_eq!(read("answer"), Ok(Answer::Text("answer".into())));
        assert!(read("  ").is_err());
    }

    #[test]
    fn every_answer_goes_to_the_runner_in_rule_s_own_grammar() {
        for answer in [
            Answer::Yes,
            Answer::No("rename it".into()),
            Answer::Rework("split it".into()),
            Answer::Text("yes".into()),
            Answer::Text("line one\nline two".into()),
            Answer::Do(Action::Rerun),
            Answer::Do(Action::Restore),
        ] {
            let params = answer.params(ticket(14));
            assert_eq!(
                read_rule(&params),
                Some((ticket(14), RuleAnswer::from(answer.clone()))),
                "{params:?}"
            );
        }
    }
}
