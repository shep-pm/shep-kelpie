//! The worker's question block
//!
//! A worker that needs a decision only the maintainer can make ends its
//! final message with the question between [`OPEN`] and [`CLOSE`], as
//! kelpie's worker instructions describe. Only a block that ends the
//! message counts, so a worker that quotes the tags mid-message asks nothing.
//! A block a worker wrapped in a code fence anyway still counts.

/// Opens a question block
pub(crate) const OPEN: &str = "<kelpie-question>";
/// Closes a question block, as the last thing in the message
pub(crate) const CLOSE: &str = "</kelpie-question>";

/// The question `text` ends on, verbatim, if it ends on one
pub(super) fn asked(text: &str) -> Option<String> {
    let text = text.trim_end();
    let body = text.strip_suffix(CLOSE).or_else(|| {
        let (rest, last) = text.rsplit_once('\n')?;
        let fence = last.trim();
        let fenced = fence.len() >= 3
            && (fence.bytes().all(|b| b == b'`') || fence.bytes().all(|b| b == b'~'));
        rest.trim_end().strip_suffix(CLOSE).filter(|_| fenced)
    })?;
    let start = body.rfind(OPEN)? + OPEN.len();
    let question = body[start..].trim();
    (!question.is_empty()).then(|| question.to_owned())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::ports::{Checks, Finding, Session, Severity};
    use crate::profile::INSTRUCTIONS;
    use crate::runner::{Runner, StepReport, step};
    use crate::test::{Rig, Scripted, ScriptedRound};

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
        for fenced in [
            "Asking.\n\n```\n<kelpie-question>\nwhy?\n</kelpie-question>\n```\n",
            "```text\n<kelpie-question>why?</kelpie-question>\n  ````  ",
            "~~~\n<kelpie-question>why?</kelpie-question>\n~~~",
        ] {
            assert_eq!(asked(fenced).as_deref(), Some("why?"), "{fenced:?}");
        }
        for none in [
            "<kelpie-question>why?</kelpie-question>\n```\nThen I pushed.",
            "<kelpie-question>why?</kelpie-question>\n``",
            "<kelpie-question>why?</kelpie-question>\n`~`",
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
                "kind": {
                    "kind": "question",
                    "asked": QUESTION,
                    "resume": { "state": "nothing" },
                },
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

    // A question on the very turn that opens the pull request interrupts
    // before the qwen-review loop ever started; once answered, the loop
    // still runs, rather than skipping straight to CI the way it used to.
    #[test]
    fn a_question_on_the_pr_opening_turn_still_runs_the_loop() {
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
        rig.claude.script([
            Scripted::Push("rename.txt", "renamed\n"),
            Scripted::Text("CLEAN"),
        ]);
        step(&runner).unwrap(); // the answered turn: pushes, enters round 1
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
            "review",
            "answering resumes the qwen-review loop, not CI directly"
        );
        step(&runner).unwrap(); // review round 1, qwen: clean by default
        step(&runner).unwrap(); // review round 2, claude: scripted clean above
        let head = rig.forge.head_of("kelpie/7").unwrap();
        rig.forge.set_checks(&head, Checks::Passed);
        assert!(matches!(
            rig.verdict(&runner),
            Some(StepReport::Ruling { id: 2, .. })
        ));
    }

    // A question asked mid-fix, during a review round's own turn,
    // interrupts that exact round; once answered, it resumes there rather
    // than restarting the loop.
    #[test]
    fn a_question_during_a_fix_turn_resumes_that_round() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "add", Some("7"));
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        rig.claude.script([Scripted::Push("work.txt", "work\n")]);
        step(&runner).unwrap(); // opens the pull request, enters round 1 (qwen)

        rig.reviewer.script([ScriptedRound::Findings(vec![Finding {
            severity: Severity::Medium,
            file: "src/lib.rs".into(),
            line: 3,
            what: "unused variable".into(),
            why: "dead code".into(),
        }])]);
        step(&runner).unwrap(); // round 1's qwen call
        rig.claude.script([Scripted::Text(
            r#"{"holds": true, "severity": "low", "reason": "a nit"}"#,
        )]);
        step(&runner).unwrap(); // the judge holds it, a nit
        step(&runner).unwrap(); // the round finalizes: sends the worker its fix

        rig.claude.script([Scripted::Say(ASKS)]);
        let Some(StepReport::Asked { id, .. }) = step(&runner).unwrap() else {
            panic!("the fix turn's question raised no ruling");
        };
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
            "ruling",
            "parked, but the round underneath is still round 1"
        );

        rig.ask(&runner, "rule", Some(&format!("{id} answer use --dry-run")));
        rig.claude.script([
            Scripted::Push("fixed.txt", "fixed\n"),
            Scripted::Text("CLEAN"),
        ]);
        step(&runner).unwrap(); // the answered fix turn: pushes, round 1 ends
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["phase"],
            json!({
                "state": "review",
                "round": 2,
                "consecutive_clean": 1,
                "guard_cleared": false,
                "stage": { "stage": "round" },
            }),
            "resumed round 1, not restarted at round 1 again"
        );
        step(&runner).unwrap(); // round 2, claude: scripted clean above
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
            "ci",
            "two clean rounds in a row end the loop"
        );
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
