//! A ruling's answer in plain words, read by the ruling's kind
//!
//! `shep kelpie rule` and replies on ntfy read an answer this way: `yes`, or
//! `no <note>`, for a ruling that asks yes or no, and any text at all for the
//! worker's question, a lone `yes` included. What they read goes to the
//! runner as `rule`'s own `<id> yes`, `<id> no <note>` or `<id> answer
//! <text>`.

use super::Answer;
use crate::state::RulingKind;

/// Which answer a ruling takes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wants {
    /// A worker's question, answered with `<id> answer <text>`
    Answer,
    /// Every other ruling: `<id> yes`, or `<id> no <note>`
    YesOrNo,
}

impl Wants {
    /// What a ruling of `kind` takes
    pub fn of(kind: &RulingKind) -> Self {
        match kind {
            RulingKind::Question { .. } => Self::Answer,
            RulingKind::Merge { .. }
            | RulingKind::Rebase { .. }
            | RulingKind::StillRed { .. }
            | RulingKind::MergeRefused { .. }
            | RulingKind::Closed
            | RulingKind::LocalModelSpilled { .. }
            | RulingKind::FixNotPushed { .. }
            | RulingKind::CodeRabbitCap { .. }
            | RulingKind::CodeRabbitSilent { .. }
            | RulingKind::TurnTimeout { .. }
            | RulingKind::TurnFailed { .. }
            | RulingKind::ClaudeFiles { .. }
            | RulingKind::ForeignChange { .. }
            | RulingKind::FollowUp { .. } => Self::YesOrNo,
        }
    }
}

/// `words` as the answer to a ruling that wants `wants`
///
/// A question's text may still start with the older `answer`, which is
/// dropped. `yes` and `no` are read in any case.
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
        Wants::YesOrNo => {
            let (first, rest) = words.split_once(char::is_whitespace).unwrap_or((words, ""));
            let rest = rest.trim();
            match (first.to_ascii_lowercase().as_str(), rest) {
                ("yes", "") => Ok(Answer::Yes),
                ("no", "") => Err("a no takes a note for the worker: `no <note>`".into()),
                ("no", note) => Ok(Answer::No(note.to_owned())),
                _ => Err("it takes `yes`, or `no <note>`".into()),
            }
        }
    }
}

impl Answer {
    /// `rule`'s params for this answer to ruling `id`
    pub fn params(&self, id: u64) -> String {
        match self {
            Self::Yes => format!("{id} yes"),
            Self::No(note) => format!("{id} no {note}"),
            Self::Text(text) => format!("{id} answer {text}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::trigger::read_rule;

    #[test]
    fn a_question_takes_an_answer_and_every_other_ruling_a_yes_or_no() {
        let question = RulingKind::Question {
            asked: String::new(),
            resume: crate::state::Resume::Nothing,
        };
        assert_eq!(Wants::of(&question), Wants::Answer);
        let merge = RulingKind::Merge {
            head: "abc".into(),
            unreviewed: None,
        };
        assert_eq!(Wants::of(&merge), Wants::YesOrNo);
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
            Answer::Text("yes".into()),
            Answer::Text("line one\nline two".into()),
        ] {
            let params = answer.params(14);
            assert_eq!(read_rule(&params), Some((14, answer)), "{params:?}");
        }
    }
}
