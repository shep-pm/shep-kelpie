//! The project manager's words reach a comment inert, so they cannot mention
//! anyone or link away, and reach the worker as they were written

use std::time::Instant;

use super::rulings::{answered, asking, left_to_pm, settled};
use super::{name_pm, past_the_window, step_until};
use crate::ports::{AgentError, Role};
use crate::runner::{StepReport, step};
use crate::settings::Harness;
use crate::test::{CALL_PATIENCE, Rig, Scripted};

/// The project manager's reply answering ruling 71 with `choice`, giving a
/// why that mentions someone and links away
fn answers_with_a_mention(choice: char, design: bool) -> &'static str {
    let text = format!(
        "{{\"pick\": null, \"votes\": [{{\"ruling\": 71, \"choice\": \"{choice}\", \
         \"why\": \"ask @someone, see [it](https://evil.example/x) or www.evil.example\", \
         \"design\": {design}}}]}}"
    );
    Box::leak(text.into_boxed_str())
}

/// The why [`answers_with_a_mention`] gives, as a comment shows it
const INERT_WHY: &str = "ask @\u{200b}someone, see \\[it\\]\\(https:\u{200b}//evil.example/x\\) or www\u{200b}.evil.example";

#[test]
fn the_project_managers_why_cannot_mention_anyone_or_link_in_its_answer() {
    let (rig, runner) = left_to_pm("pm-inert");
    rig.claude.script_for(
        Role::Pm,
        [Scripted::Say(answers_with_a_mention('A', false))],
    );

    settled(&rig, &runner);

    let said = answered(&rig).expect("the answer is posted");
    assert!(said.contains(INERT_WHY), "{said}");
}

#[test]
fn the_project_managers_why_cannot_mention_anyone_or_link_on_the_maintainers_card() {
    let (rig, runner) = left_to_pm("pm-inert-card");
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(answers_with_a_mention('B', true))]);

    settled(&rig, &runner);

    let card = asking(&rig).expect("the ruling is posted for the maintainer");
    assert!(card.contains(INERT_WHY), "{card}");
}

#[test]
fn the_worker_gets_the_project_managers_why_as_it_was_written() {
    let (rig, runner) = left_to_pm("pm-raw-note");
    rig.claude.script_for(
        Role::Pm,
        [Scripted::Say(answers_with_a_mention('B', false))],
    );
    rig.claude.script([Scripted::Say("done")]);

    settled(&rig, &runner);
    let deadline = Instant::now() + CALL_PATIENCE;
    let raw = "ask @someone, see [it](https://evil.example/x) or www.evil.example";
    let sent = |rig: &Rig| (rig.claude.calls().iter()).any(|c| c.prompt.contains(raw));
    while !sent(&rig) {
        assert!(
            Instant::now() < deadline,
            "the worker was never sent the why"
        );
        step(&runner).unwrap();
        std::thread::yield_now();
    }
}

#[test]
fn a_retry_s_why_cannot_mention_anyone_or_link_in_the_settled_comment() {
    let rig = Rig::new("pm-inert-retry");
    rig.with_app();
    rig.maintainer("octocat");
    name_pm(&rig);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let failed = AgentError::Failed(Harness::ClaudeCode, "it crashed".into());
    rig.claude.script([
        Scripted::Fail(failed),
        Scripted::Say(
            "{\"pick\": null, \"unstick\": {\"item\": 7, \"action\": \"retry\", \
             \"why\": \"ask @someone, see [it](https://evil.example/x) or www.evil.example\"}}",
        ),
        Scripted::Say("done"),
    ]);
    step(&runner).unwrap();
    past_the_window(&rig);

    step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));

    let edits = rig.github.edits();
    let settled = (edits.iter().map(|(_, body)| body))
        .find(|body| body.starts_with("**Settled.**"))
        .unwrap_or_else(|| panic!("the ruling's comment was not settled: {edits:?}"));
    assert!(settled.contains(INERT_WHY), "{settled}");
}
