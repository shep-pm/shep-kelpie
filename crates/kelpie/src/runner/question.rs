//! The worker's question block
//!
//! A worker that needs a decision only the maintainer can make ends its
//! final message with the question between [`OPEN`] and [`CLOSE`], as
//! kelpie's worker instructions describe. Only a block that ends the
//! message counts, so a worker that quotes the tags mid-message asks nothing.

/// Opens a question block
pub(crate) const OPEN: &str = "<kelpie-question>";
/// Closes a question block, as the last thing in the message
pub(crate) const CLOSE: &str = "</kelpie-question>";

/// The question `text` ends on, verbatim, if it ends on one
pub(super) fn asked(text: &str) -> Option<String> {
    let body = text.trim_end().strip_suffix(CLOSE)?;
    let start = body.rfind(OPEN)? + OPEN.len();
    let question = body[start..].trim();
    (!question.is_empty()).then(|| question.to_owned())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::ports::{Checks, Session};
    use crate::profile::INSTRUCTIONS;
    use crate::runner::{Runner, StepReport, step};
    use crate::test::{Rig, Scripted};

    const ASKS: &str = "I added the flag.\n\n<kelpie-question>\nShould it be `--dry-run` or \
                        `--check`?\n\nBoth appear in the docs.\n</kelpie-question>\n";
    const QUESTION: &str = "Should it be `--dry-run` or `--check`?\n\nBoth appear in the docs.";

    // A running project with issue 7 in flight, whose worker's first turn
    // ends on `ASKS`
    fn asking(project: &str) -> (Rig, Mutex<Runner>) {
        let rig = Rig::new(project);
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "add", Some("7"));
        rig.claude.script([Scripted::Say(ASKS)]);
        (rig, runner)
    }

    #[test]
    fn a_question_block_is_read_only_where_it_ends_the_message() {
        assert_eq!(asked(ASKS).as_deref(), Some(QUESTION));
        assert_eq!(
            asked("<kelpie-question>why?</kelpie-question>  \n\n").as_deref(),
            Some("why?")
        );
        let last = "<kelpie-question>old</kelpie-question> <kelpie-question>new</kelpie-question>";
        assert_eq!(asked(last).as_deref(), Some("new"));
        for none in [
            "done",
            "<kelpie-question>why?</kelpie-question>\nThen I pushed.",
            "<kelpie-question>\n  \n</kelpie-question>",
            "why?</kelpie-question>",
            "",
        ] {
            assert_eq!(asked(none), None, "{none:?}");
        }
    }

    // Recorded from Claude Code 2.1.283 on Sonnet 5 at medium effort, with
    // kelpie's worker instructions, on an issue that left the choice to the
    // maintainer.
    const RECORDED: &str = include_str!("../../fixtures/claude-p-question.json");

    #[test]
    fn a_real_workers_question_block_is_read() {
        let reply: serde_json::Value = serde_json::from_str(RECORDED).unwrap();
        let text = reply["result"].as_str().unwrap();
        assert_eq!(
            asked(text).as_deref(),
            Some(
                "Should I remove `/v1/export` now, or keep it one more release behind \
                 a deprecation warning before removing it?"
            )
        );
    }

    #[test]
    fn the_worker_instructions_describe_the_block() {
        assert!(INSTRUCTIONS.contains(OPEN), "{INSTRUCTIONS}");
        assert!(INSTRUCTIONS.contains(CLOSE), "{INSTRUCTIONS}");
    }

    #[test]
    fn a_question_parks_the_worker_and_its_answer_is_the_next_turn() {
        let (rig, runner) = asking("rotom");
        let Some(StepReport::Asked {
            id,
            question,
            pull_request,
            comment_failed,
            ..
        }) = step(&runner).unwrap()
        else {
            panic!("the question raised no ruling");
        };
        assert_eq!((id, pull_request, comment_failed), (1, None, None));
        assert_eq!(
            question,
            format!(
                "The worker on issue #7 asks:\n\n{QUESTION}\n\n\
                 `shep trigger rotom rule '1 answer <text>'` sends the worker your answer."
            )
        );
        let status = rig.ask(&runner, "status", None);
        assert_eq!(
            status["rulings"],
            json!([{
                "id": 1,
                "question": question,
                "pull_request": null,
                "kind": { "kind": "question", "asked": QUESTION },
                "alerted": false,
            }])
        );
        assert_eq!(
            status["work_item"]["phase"],
            json!({ "state": "ruling", "id": 1 })
        );
        assert_eq!(rig.forge.comments(), [], "no pull request to comment on");

        assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
        assert_eq!(rig.alerts.posts()[0].1.text, question);
        rig.clock.advance(3600);
        assert_eq!(step(&runner).unwrap(), None, "parked until answered");

        rig.ask(
            &runner,
            "rule",
            Some("1 answer  Use --dry-run, it matches shep. "),
        );
        rig.claude.script([Scripted::Say("renamed")]);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Ended { .. })
        ));
        let [first, answered] = rig.claude.calls().try_into().unwrap();
        assert_eq!(
            answered.session,
            Session::Resume(first.session.id().clone())
        );
        assert_eq!(
            answered.prompt,
            "The maintainer answered your question:\n\nUse --dry-run, it matches shep.\n"
        );
        assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
    }

    #[test]
    fn a_question_about_an_open_pull_request_is_posted_on_it_and_ci_waits() {
        let (rig, runner) = asking("shep");
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        let Some(StepReport::Asked {
            pull_request: Some(71),
            question,
            ..
        }) = step(&runner).unwrap()
        else {
            panic!("the question was not about pull request 71");
        };
        assert!(question.starts_with("The worker on pull request #71 asks:\n\n"));
        assert_eq!(rig.forge.comments(), [(71, question)]);
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["phase"],
            json!({ "state": "ruling", "id": 1 })
        );

        rig.ask(&runner, "rule", Some("1 answer --dry-run"));
        rig.claude
            .script([Scripted::Push("rename.txt", "renamed\n")]);
        step(&runner).unwrap();
        let head = rig.forge.head_of("kelpie/7").unwrap();
        rig.forge.set_checks(&head, Checks::Passed);
        assert!(matches!(
            rig.verdict(&runner),
            Some(StepReport::Ruling { id: 2, .. })
        ));
    }

    #[test]
    fn a_question_takes_an_answer_and_a_gate_ruling_does_not() {
        let (rig, runner) = asking("koji");
        step(&runner).unwrap();
        for params in ["1 yes", "1 no not now"] {
            assert_eq!(
                rig.ask(&runner, "rule", Some(params)),
                json!({ "error": "ruling 1 is the worker's question: answer it with `1 answer <text>`" })
            );
        }
        assert_eq!(
            rig.ask(&runner, "rule", Some("1 answer")),
            json!({ "error": "`rule` takes `<id> yes`, `<id> no <note>` or `<id> answer <text>`, not \"1 answer\"" })
        );
        assert_eq!(rig.ask(&runner, "status", None)["rulings"][0]["id"], 1);

        let (rig, runner, _) = Rig::parked("golbat");
        assert_eq!(
            rig.ask(&runner, "rule", Some("1 answer merge it")),
            json!({ "error": "ruling 1 takes `1 yes` or `1 no <note>`, not an answer" })
        );
        assert_eq!(rig.forge.merges(), []);
    }

    #[test]
    fn a_dropped_work_item_takes_its_question_with_it() {
        let (rig, runner) = asking("zeus");
        step(&runner).unwrap();
        let status = rig.ask(&runner, "drop", None);
        assert_eq!(
            (&status["work_item"], &status["rulings"]),
            (&json!(null), &json!([]))
        );
    }

    #[test]
    fn a_turn_that_quotes_the_tags_mid_message_asks_nothing() {
        let rig = Rig::new("chelone");
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "add", Some("7"));
        rig.claude.script([Scripted::Say(
            "I documented <kelpie-question>x</kelpie-question> in the README.",
        )]);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Ended { .. })
        ));
        assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
    }
}
