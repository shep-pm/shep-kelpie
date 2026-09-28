//! Rulings: raising one, and the maintainer's answer
//!
//! A ruling is saved before its comment is posted, so a comment that fails
//! loses nothing: the ruling stays in status and in the log. Its question
//! carries the exact triggers that answer it. What a yes does depends on
//! the ruling; a no's note, or an answer to the worker's question, is
//! always the worker's next turn.

use std::fmt;

use super::Runner;
use super::gate::short;
use super::report::{Begin, StepReport};
use crate::ports::Timestamp;
use crate::state::{ProjectState, Resume, Ruling, RulingKind, StateError};
use crate::work_item::{Known, Phase, Review, Turn, WorkItem};

/// The prompt for a turn resumed after the maintainer accepts a timed-out
/// turn's ruling with a yes
const TIMEOUT_CONTINUE: &str = "Kelpie stopped your last turn: it ran past its ceiling. \
                                Carry on with the work item from where you left off.";

/// The maintainer's answer to a ruling
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Go ahead: what that means depends on the ruling
    Yes,
    /// Do not, and send the worker this note
    No(String),
    /// The answer to the worker's question
    Text(String),
}

/// Why an answer was refused
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleError {
    /// No pending ruling has this id
    NoSuchRuling(u64),
    /// The ruling is the worker's question, and was given a yes or a no
    WantsAnswer(u64),
    /// The ruling is not a question, and was given an answer
    NotAQuestion(u64),
    /// The answer could not be saved
    State(StateError),
}

impl fmt::Display for RuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchRuling(id) => write!(f, "no ruling {id} is pending"),
            Self::WantsAnswer(id) => write!(
                f,
                "ruling {id} is the worker's question: answer it with `{id} answer <text>`"
            ),
            Self::NotAQuestion(id) => write!(
                f,
                "ruling {id} takes `{id} yes` or `{id} no <note>`, not an answer"
            ),
            Self::State(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for RuleError {}

/// What an answer does to the work item parked on its ruling
enum Move {
    /// It goes on to this phase
    Phase(Phase),
    /// The worker takes a turn with this prompt, under this phase; once
    /// that turn ends with no further question, `force` (when given)
    /// replaces the ordinary rule of a known pull request going to CI
    Turn {
        prompt: String,
        phase: Phase,
        force: Option<Phase>,
    },
    /// A yes on a foreign change: kelpie adopts it and watches CI again
    Accept(Known),
}

impl Runner {
    /// Answers ruling `id`. A no's note, or an answer, is the worker's next turn.
    ///
    /// # Errors
    ///
    /// [`RuleError`] when no such ruling is pending, the answer does not fit
    /// it, or the answer cannot be saved. Nothing changes then.
    pub fn rule(&mut self, id: u64, answer: Answer) -> Result<(), RuleError> {
        let at = self.state.rulings.iter().position(|r| r.id == id);
        let at = at.ok_or(RuleError::NoSuchRuling(id))?;
        let now = self.ports.clock.now();
        let mut next = self.state.clone();
        let ruling = next.rulings.remove(at);
        let lifts_cap = matches!(
            (&answer, &ruling.kind),
            (Answer::Yes, RulingKind::CodeRabbitCap { .. })
        );
        let moved = decide(id, answer, ruling, now)?;
        // Only the ruling the work item is parked on moves it. Any other,
        // which nothing leaves behind today, is answered by clearing it.
        let parked_on = |item: &WorkItem| item.phase == Phase::Ruling { id };
        if let Some(item) = next.work_item.as_mut().filter(|item| parked_on(item)) {
            item.coderabbit.cap_cleared |= lifts_cap;
            match moved {
                Move::Phase(phase) => item.phase = phase,
                Move::Turn {
                    prompt,
                    phase,
                    force,
                } => {
                    item.turn = Turn::Next { prompt };
                    item.phase = phase;
                    item.resume = force;
                }
                Move::Accept(known) => {
                    item.known = known;
                    item.phase = Phase::Ci {
                        head: None,
                        since: now,
                    };
                }
            }
        }
        self.save(next).map_err(RuleError::State)
    }

    // Saves the ruling and parks the worker on it, then posts it on pull
    // request `number`.
    pub(super) fn raise(&mut self, number: u64, kind: RulingKind) -> Result<Begin, StateError> {
        let mut next = self.state.clone();
        let (issue, id, question) = park(self.project.as_str(), &mut next, Some(number), kind);
        self.save(next)?;
        let comment_failed = self.post_ruling(Some(number), &question);
        Ok(Begin::Report(StepReport::Ruling {
            issue,
            pull_request: number,
            id,
            question,
            comment_failed,
        }))
    }

    // Posts a saved ruling's question on its pull request, if it has one,
    // and returns why the comment failed, if it did.
    pub(super) fn post_ruling(&self, number: Option<u64>, question: &str) -> Option<String> {
        let number = number?;
        let posted = self
            .ports
            .forge
            .comment(&self.settings.forge, number, question);
        posted.err().map(|e| e.to_string())
    }

    pub(super) fn update(&mut self, change: impl FnOnce(&mut WorkItem)) -> Result<(), StateError> {
        let mut next = self.state.clone();
        change(
            next.work_item
                .as_mut()
                .expect("a change to the work item in flight"),
        );
        self.save(next)
    }
}

/// Adds a ruling to `next` and parks its work item on it
///
/// Returns the work item's issue, and the ruling's id and question.
pub(super) fn park(
    project: &str,
    next: &mut ProjectState,
    pull_request: Option<u64>,
    kind: RulingKind,
) -> (u64, u64, String) {
    let id = next.last_ruling + 1;
    let item = next
        .work_item
        .as_mut()
        .expect("a ruling is about a work item");
    item.phase = Phase::Ruling { id };
    let issue = item.issue;
    let text = question(project, id, issue, pull_request, &kind);
    next.last_ruling = id;
    next.rulings.push(Ruling {
        id,
        question: text.clone(),
        pull_request,
        kind,
        alerted: false,
    });
    (issue, id, text)
}

// A yes, a no or an answer that does not fit the ruling is refused.
fn decide(id: u64, answer: Answer, ruling: Ruling, now: Timestamp) -> Result<Move, RuleError> {
    let phase = match (answer, ruling.kind) {
        (Answer::Text(text), RulingKind::Question { resume, .. }) => {
            // A question resumes exactly where it interrupted the qwen-review
            // loop; one asked before the loop ever started, with a pull
            // request already open, starts it once answered instead of the
            // ordinary rule of going straight to CI.
            let (phase, force) = match resume {
                Resume::Nothing => (Phase::Implement, None),
                Resume::ReviewFirst => (Phase::Implement, Some(Phase::Review(Review::first()))),
                Resume::Review(review) => (Phase::Review(review), None),
            };
            return Ok(Move::Turn {
                prompt: answer_prompt(&text),
                phase,
                force,
            });
        }
        (_, RulingKind::Question { .. }) => return Err(RuleError::WantsAnswer(id)),
        (Answer::Text(_), _) => return Err(RuleError::NotAQuestion(id)),
        (Answer::Yes, RulingKind::TurnTimeout) => {
            return Ok(Move::Turn {
                prompt: TIMEOUT_CONTINUE.to_owned(),
                phase: Phase::Implement,
                force: None,
            });
        }
        (Answer::No(_), RulingKind::TurnTimeout) => Phase::Done { merged: false },
        (Answer::Yes, RulingKind::ForeignChange { known, .. }) => return Ok(Move::Accept(known)),
        // A no's fix is new code, unreviewed: it goes through the
        // qwen-review loop again before CI, whatever ruling this answers.
        (Answer::No(note), _) => {
            return Ok(Move::Turn {
                prompt: note_prompt(ruling.pull_request, &note),
                phase: Phase::Implement,
                force: Some(Phase::Review(Review::first())),
            });
        }
        (Answer::Yes, RulingKind::Merge { head }) => Phase::Merge {
            head,
            readied: None,
        },
        (Answer::Yes, RulingKind::Rebase { .. } | RulingKind::StillRed { .. }) => Phase::Ci {
            head: None,
            since: now,
        },
        (Answer::Yes, RulingKind::Closed) => Phase::Done { merged: false },
        (Answer::Yes, RulingKind::ReviewGuard { review }) => Phase::Review(Review {
            guard_cleared: true,
            ..review
        }),
        // The fix ends under Implement, which takes it to CI and the next round.
        (Answer::Yes, RulingKind::CodeRabbitCap { prompt, .. }) => {
            return Ok(Move::Turn {
                prompt,
                phase: Phase::Implement,
                force: None,
            });
        }
        (Answer::Yes, RulingKind::CodeRabbitSilent { .. }) => Phase::Ci {
            head: None,
            since: now,
        },
    };
    Ok(Move::Phase(phase))
}

fn question(project: &str, id: u64, issue: u64, number: Option<u64>, kind: &RulingKind) -> String {
    let trigger = |answer: &str| format!("`shep trigger {project} rule '{id} {answer}'`");
    let (yes, no) = (trigger("yes"), trigger("no <note>"));
    let about = number.map_or_else(
        || format!("issue #{issue}"),
        |n| format!("pull request #{n}"),
    );
    let ask = match kind {
        RulingKind::Merge { head } => {
            format!(
                "Merge {about} at {} into main? {yes} merges it",
                short(head)
            )
        }
        RulingKind::Rebase { reason } => format!(
            "Kelpie cannot rebase {about} onto main: {reason}. \
             Once the branch is fixed, {yes} has kelpie look again"
        ),
        RulingKind::StillRed { head, checks } => format!(
            "CI failed again on {about} at {}, and the worker pushed \
             no fix: {}. {yes} has kelpie look again",
            short(head),
            checks.join(", ")
        ),
        RulingKind::Closed => format!(
            "{} was closed without merging. {yes} drops the work \
             item and keeps its branch on the forge",
            capitalized(&about)
        ),
        RulingKind::ReviewGuard { review } => format!(
            "The qwen-review loop on {about} has run {} rounds without \
             settling. {yes} lets it keep going",
            review.round.saturating_sub(1)
        ),
        RulingKind::CodeRabbitCap { rounds, held, .. } => format!(
            "CodeRabbit has run {rounds} rounds on {about}, its cap, and the judge \
             still holds {held} of its findings. {yes} sends the worker those \
             findings and lets the rounds go past the cap"
        ),
        RulingKind::CodeRabbitSilent { head } => format!(
            "CodeRabbit never reviewed {about} at {} after kelpie summoned it. \
             {yes} has kelpie look at CI and summon it again",
            short(head)
        ),
        RulingKind::Question { asked, .. } => {
            return format!(
                "The worker on {about} asks:\n\n{asked}\n\n{} sends the worker your answer.",
                trigger("answer <text>")
            );
        }
        RulingKind::TurnTimeout => {
            return format!(
                "The worker on {about} has been running past its turn's ceiling, \
                 and kelpie stopped it. {yes} resumes its session for another turn, \
                 and {no} stops the work item, keeping its branch and pull request \
                 on the forge."
            );
        }
        RulingKind::ForeignChange { description, .. } => {
            return format!(
                "{} changed outside kelpie: {description}. {yes} accepts it and kelpie \
                 carries on, and {no} sends the worker your note.",
                capitalized(&about)
            );
        }
    };
    format!("{ask}, and {no} sends the worker your note.")
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

fn note_prompt(number: Option<u64>, note: &str) -> String {
    let about = number.map_or_else(
        || "your work item".to_owned(),
        |n| format!("pull request #{n}"),
    );
    format!("The maintainer answered no on {about}, with this note:\n\n{note}\n")
}

fn answer_prompt(text: &str) -> String {
    format!("The maintainer answered your question:\n\n{text}\n")
}

#[cfg(test)]
mod tests {

    use serde_json::json;

    use super::*;
    use crate::ports::{Checks, Session};
    use crate::runner::gate::CHECKS_SETTLE;
    use crate::runner::step;
    use crate::test::{Rig, Scripted};

    #[test]
    fn nothing_merges_without_a_yes() {
        let (rig, runner, _) = Rig::parked("shep");
        assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
        for _ in 0..5 {
            rig.clock.advance(3600);
            assert_eq!(step(&runner).unwrap(), None);
        }
        let refused = [
            ("2 yes", "no ruling 2 is pending"),
            (
                "1 no",
                "`rule` takes `<id> yes`, `<id> no <note>` or `<id> answer <text>`, not \"1 no\"",
            ),
            (
                "1 yes please",
                "`rule` takes `<id> yes`, `<id> no <note>` or `<id> answer <text>`, not \"1 yes please\"",
            ),
            (
                "one yes",
                "`rule` takes `<id> yes`, `<id> no <note>` or `<id> answer <text>`, not \"one yes\"",
            ),
            (
                "1 maybe",
                "`rule` takes `<id> yes`, `<id> no <note>` or `<id> answer <text>`, not \"1 maybe\"",
            ),
        ];
        for (params, error) in refused {
            assert_eq!(
                rig.ask(&runner, "rule", Some(params)),
                json!({ "error": error }),
                "{params}"
            );
            assert_eq!(step(&runner).unwrap(), None, "{params}");
        }
        assert_eq!(rig.forge.merges(), []);
        assert!(rig.forge.readied().is_empty());
        assert!(rig.worktree_7().exists());
    }

    // A no's fix is new code the loop has not seen: it goes back through
    // the qwen-review loop, not straight to CI, before the next ruling.
    #[test]
    fn a_no_sends_the_note_to_the_worker_and_it_goes_through_review_before_the_next_ruling() {
        let (rig, runner, _) = Rig::parked("rotom");
        rig.ask(&runner, "rule", Some("1 no  rename the flag to --dry-run "));
        rig.claude.script([
            Scripted::Push("rename.txt", "renamed\n"),
            Scripted::Text("CLEAN"),
        ]);
        step(&runner).unwrap(); // the noted turn: pushes, enters round 1
        let [first, noted] = rig.claude.calls().try_into().unwrap();
        assert_eq!(noted.session, Session::Resume(first.session.id().clone()));
        assert_eq!(
            noted.prompt,
            "The maintainer answered no on pull request #71, with this note:\n\n\
             rename the flag to --dry-run\n"
        );
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
            "review",
            "a no's fix resumes the loop, not CI directly"
        );

        let pushed = rig.forge.head_of("kelpie/7").unwrap();
        step(&runner).unwrap(); // review round 1, qwen: clean by default
        step(&runner).unwrap(); // review round 2, claude: scripted clean above
        assert_eq!(
            step(&runner).unwrap(),
            None,
            "CI on the new head is pending"
        );
        rig.forge.set_checks(&pushed, Checks::Passed);
        assert!(matches!(
            rig.verdict(&runner),
            Some(StepReport::Ruling { id: 2, .. })
        ));
        let rulings = &rig.ask(&runner, "status", None)["rulings"];
        assert_eq!(
            rulings[0]["kind"],
            json!({ "kind": "merge", "head": pushed })
        );
        assert_eq!(rulings.as_array().unwrap().len(), 1);
        assert_eq!(rig.forge.merges(), []);
    }

    #[test]
    fn ruling_ids_are_never_given_twice() {
        let (rig, runner, _) = Rig::parked("rotom");
        rig.ask(&runner, "rule", Some("1 yes"));
        step(&runner).unwrap();
        rig.clock.advance(CHECKS_SETTLE);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Finished { merged: true, .. })
        ));
        drop(runner);
        let runner = rig.open().unwrap();
        rig.ask(&runner, "add", Some("7"));
        rig.forge.open_pull_request(72, "kelpie/7", &[7]);
        rig.claude.script([
            Scripted::Push("again.txt", "again\n"),
            Scripted::Text("CLEAN"),
        ]);
        step(&runner).unwrap(); // the worker's first turn: opens the pull request
        step(&runner).unwrap(); // review round 1, qwen: clean by default
        step(&runner).unwrap(); // review round 2, claude: scripted clean above
        let head = rig.forge.head_of("kelpie/7").unwrap();
        rig.forge.set_checks(&head, Checks::Passed);
        assert!(matches!(
            rig.verdict(&runner),
            Some(StepReport::Ruling { id: 2, .. })
        ));
        assert_eq!(
            rig.ask(&runner, "rule", Some("1 yes")),
            json!({ "error": "no ruling 1 is pending" })
        );
    }
}
