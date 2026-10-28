//! A worker's question through the project manager: it answers, or passes
//! the question on with a recommendation

use std::sync::Mutex;

use serde_json::json;

use super::{answered, pm_on, step_until, with_pm};
use crate::ports::AgentError;
use crate::runner::flight::advance;
use crate::runner::{Pass, Runner, StepReport, step};
use crate::settings::Harness;
use crate::test::ticket;
use crate::test::{CALL_PATIENCE, Hold, Rig, Scripted};
use crate::ticket::TicketId;

const CHOICES: &str = "Done so far.\n\n<kelpie-question>\nWhich flag?\n- `--dry-run`\n\
                       - `--check`\n</kelpie-question>\n";

pub(super) const ASKS: &str = "I added the flag.\n\n<kelpie-question>\nShould it be `--dry-run` or \
                    `--check`?\n\nBoth appear in the docs.\n</kelpie-question>\n";

/// A project with a project manager, and issue 7's worker asking `question`
/// on its first turn, the ruling raised
pub(super) fn asking(project: &str, question: &'static str) -> (Rig, Mutex<Runner>) {
    let (rig, runner) = with_pm(project);
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Say(question)]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Asked { id, .. })
     if id == ticket(7)));
    (rig, runner)
}

#[test]
fn a_question_wakes_it_and_its_text_is_the_workers_next_turn() {
    let (rig, runner) = asking("rotom", ASKS);
    rig.claude.script([
        Scripted::Say(
            "{\"answer\": {\"item\": 7, \"text\": \"Use --dry-run: the issue says it matches \
             shep.\", \"why\": \"#7 names the flag\"}}",
        ),
        Scripted::Say("renamed"),
    ]);
    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (woke_for, acted, dropped) = answered(Some(report));
    assert_eq!(
        woke_for,
        [
            "#7's worker asks, on ruling 7:\n\n> Should it be `--dry-run` or `--check`?\n>\n> \
             Both appear in the docs."
        ]
    );
    assert_eq!(acted, ["answered ruling 7 on #7"]);
    assert_eq!(dropped, Vec::<String>::new());

    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"], json!([]));
    assert!(
        rig.alerts.posts().is_empty(),
        "the maintainer was never asked"
    );

    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Ended { .. })
    ));
    let calls = rig.claude.calls();
    let resumed = &calls.last().unwrap().prompt;
    assert!(
        resumed.contains("PM answered: Use --dry-run: the issue says it matches shep."),
        "{resumed}"
    );
}

#[test]
fn a_question_posted_while_draining_is_edited_to_say_the_project_manager_answered_it() {
    let rig = Rig::new("raichu");
    rig.with_app();
    rig.maintainer("octocat");
    rig.forge.list_ready(7, false);
    // With no webhook the maintainer is told by the comment alone, so the
    // ruling is not alerted, and the project manager may still answer it.
    std::fs::write(rig.kelpie_settings_file(), "").unwrap();
    let (rig, runner) = pm_on(rig);
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Say(ASKS)]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Asked { id, .. }) if id == ticket(7)
    ));
    // A runner that drains for an upgrade does not hold the question back
    // for the project manager, so it is posted at once.
    rig.ask(&runner, "drain", None);
    step(&runner).unwrap();
    let posted = rig.github.comments();
    assert!(
        posted
            .iter()
            .any(|(_, c)| c.contains("Both appear in the docs."))
    );
    assert_eq!(rig.github.edits(), []);

    // The restarted runner's project manager answers it.
    rig.ask(&runner, "undrain", None);
    rig.claude.script([
        Scripted::Say(
            "{\"answer\": {\"item\": 7, \"text\": \"Use --dry-run: the issue says it matches \
             shep.\", \"why\": \"#7 names the flag\"}}",
        ),
        Scripted::Say("renamed"),
    ]);
    step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));

    let [(comment, body)] = rig.github.edits().try_into().unwrap();
    assert_eq!(comment, 100);
    let first = "**Settled.** The project manager answered: Use --dry-run: the issue says it \
                 matches shep (why: \\#7 names the flag). Nothing waits on the maintainer.";
    assert_eq!(body.lines().next(), Some(first));
    assert!(
        body.contains("### Should it be `--dry-run` or `--check`?"),
        "{body}"
    );
    assert!(body.contains("Both appear in the docs."), "{body}");
    for gone in ["@octocat", "- [ ]", "Reply", "| Option |"] {
        assert!(!body.contains(gone), "{gone} is still in:\n{body}");
    }
}

#[test]
fn the_worker_is_still_told_to_ask_for_what_only_the_maintainer_can_decide() {
    let told = crate::profile::INSTRUCTIONS;
    assert!(
        told.contains("a decision only the maintainer can make"),
        "{told}"
    );
}

#[test]
fn each_answer_and_its_why_is_in_the_notes_the_board_and_status() {
    let (rig, runner) = asking("pikachu", ASKS);
    let long = "x".repeat(2_500);
    let answer = format!(
        "{{\"answer\": {{\"item\": 7, \"text\": \"{long}\", \"why\": \"the issue says so\"}}}}"
    );
    rig.claude
        .script([Scripted::Say(Box::leak(answer.into_boxed_str()))]);
    step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));

    let line = "ruling 7 on #7: the project manager answered it: the issue says so";
    let notes = std::fs::read_to_string(rig.paths().pm.join("pm-notes.md")).unwrap();
    let lines: Vec<&str> = notes.lines().collect();
    assert_eq!(lines.len(), 2, "{notes}");
    assert_eq!(lines[0], format!("- {line}"), "{notes}");
    assert!(
        lines[1].starts_with("- [2026-09-21 14:13] woke for: #7's worker asks, on ruling 7:")
            && lines[1].contains("; kelpie: answered ruling 7 on #7"),
        "kelpie's own line for the wake follows it: {notes}"
    );
    let pm = &rig.ask(&runner, "status", None)["pm"];
    assert_eq!(
        pm["settled"],
        json!([{
            "ruling": "7",
            "issue": "7",
            "how": "answered",
            "why": "the issue says so",
        }])
    );
    step(&runner).unwrap();
    let board = std::fs::read_to_string(&rig.paths().board).unwrap();
    assert!(board.contains(line), "{board}");

    let calls = rig.claude.calls();
    let turn = &calls.last().unwrap().prompt;
    let sent = turn.split("PM answered: ").nth(1).unwrap().trim_end();
    assert_eq!(sent.chars().count(), 2_000 + " …".chars().count());
}

// The ruling's question as the maintainer is alerted of it, once the
// project manager has passed it on
fn passed_on(rig: &Rig, runner: &Mutex<Runner>, answer: &'static str) -> String {
    rig.claude.script([Scripted::Say(answer)]);
    let report = step_until(runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (_, acted, dropped) = answered(Some(report));
    // A question asked before a pull request is named by its issue.
    let id: TicketId = rig.ask(runner, "status", None)["rulings"][0]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        acted,
        [format!("put ruling {id} on #7 to the maintainer")],
        "{dropped:?}"
    );
    assert_eq!(step(runner).unwrap(), Some(StepReport::Alerted { id }));
    let [(_, alert)] = rig.alerts.posts().try_into().unwrap();
    alert.text
}

#[test]
fn the_candidates_the_project_manager_drafts_reach_the_maintainer() {
    let (rig, runner) = asking("vulpix", ASKS);
    let text = passed_on(
        &rig,
        &runner,
        "{\"answer\": {\"item\": 7, \"escalate\": true, \"candidates\": [\"Use --check\", \"Ask the \
         maintainer\"], \"why\": \"a product call\"}}",
    );
    assert!(text.contains("PM says: a product call"), "{text}");
    assert!(text.contains("Use --check"), "{text}");
}

#[test]
fn passing_a_question_on_puts_its_recommended_choice_first_in_the_list() {
    let (rig, runner) = asking("bulbasaur", CHOICES);
    let text = passed_on(
        &rig,
        &runner,
        "{\"answer\": {\"item\": 7, \"escalate\": \"--check\", \"why\": \"shep uses it\"}}",
    );
    assert_eq!(
        text,
        "The worker on issue #7 asks:\n\nWhich flag?\nPM recommends: `--check`\n\
         PM says: shep uses it\n- `--check`\n- `--dry-run`\n\n\
         `shep kelpie rule 7 <text>` sends the worker your answer."
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["question"], text);
    assert_eq!(status["rulings"][0]["alerted"], true);
    assert_eq!(
        rig.claude.calls().len(),
        1,
        "the worker waits on the maintainer"
    );
}

const EFFECTS: &str = "<kelpie-question>\nProblem: The hook refuses two commits.\n\
                       Which flag?\n- `--dry-run` => The worker renames the flag\n\
                       - `--check` => The worker keeps it\n</kelpie-question>\n";

#[test]
fn passing_a_question_on_rewrites_its_problem_and_adds_the_boards_note_over_the_workers_effects() {
    let (rig, runner) = asking("squirtle", EFFECTS);
    let text = passed_on(
        &rig,
        &runner,
        "{\"answer\": {\"item\": 7, \"escalate\": \"--check\", \"why\": \"shep uses it\", \
         \"problem\": \"The push hook refuses two commits.\", \
         \"board\": \"#9 stays parked and 3 ready issues wait behind it.\"}}",
    );
    assert_eq!(
        text,
        "The worker on issue #7 asks:\n\nProblem: The push hook refuses two commits.\n\
         Which flag?\nPM recommends: `--check`\n\
         PM board: #9 stays parked and 3 ready issues wait behind it.\n\
         PM says: shep uses it\n\
         - `--check` => The worker keeps it\n- `--dry-run` => The worker renames the flag\n\n\
         `shep kelpie rule 7 <text>` sends the worker your answer."
    );
}

#[test]
fn a_problem_of_500_characters_reaches_the_ruling_whole() {
    let (rig, runner) = asking("togepi", EFFECTS);
    let problem = ["word"; 100].join(" ");
    assert_eq!(problem.chars().count(), 499);
    let answer = format!(
        "{{\"answer\": {{\"item\": 7, \"escalate\": \"--check\", \"why\": \"shep uses it\", \
         \"problem\": \"{problem}\"}}}}"
    );
    let text = passed_on(&rig, &runner, Box::leak(answer.into_boxed_str()));
    assert!(text.contains(&format!("Problem: {problem}\n")), "{text}");
}

#[test]
fn a_why_past_300_characters_is_cut_at_a_word_with_an_ellipsis() {
    let (rig, runner) = asking("eevee", ASKS);
    let why = ["reasons"; 60].join(" ");
    let answer = format!(
        "{{\"answer\": {{\"item\": 7, \"escalate\": true, \"candidates\": [\"Use --check\", \
         \"Ask the maintainer\"], \"why\": \"{why}\"}}}}"
    );
    let text = passed_on(&rig, &runner, Box::leak(answer.into_boxed_str()));
    let said = text
        .lines()
        .find_map(|l| l.strip_prefix("PM says: "))
        .unwrap();
    let kept = ["reasons"; 37].join(" ");
    assert_eq!(said, format!("{kept} …"));
}

#[test]
fn a_recommendation_outside_the_list_is_added_as_its_first_line() {
    let (rig, runner) = asking("ivysaur", CHOICES);
    let text = passed_on(
        &rig,
        &runner,
        "{\"answer\": {\"item\": 7, \"escalate\": \"--plan\", \"why\": \"\"}}",
    );
    assert_eq!(
        text,
        "The worker on issue #7 asks:\n\nWhich flag?\nPM recommends: --plan\n\
         - --plan\n- `--dry-run`\n- `--check`\n\n\
         `shep kelpie rule 7 <text>` sends the worker your answer."
    );
}

#[test]
fn a_recommendation_naming_a_choice_by_its_letter_moves_it_up_with_no_new_row() {
    let (rig, runner) = asking("oddish", CHOICES);
    let text = passed_on(
        &rig,
        &runner,
        "{\"answer\": {\"item\": 7, \"escalate\": \"B\", \"why\": \"shep uses it\"}}",
    );
    assert_eq!(
        text,
        "The worker on issue #7 asks:\n\nWhich flag?\nPM recommends: `--check`\n\
         PM says: shep uses it\n- `--check`\n- `--dry-run`\n\n\
         `shep kelpie rule 7 <text>` sends the worker your answer."
    );
}

#[test]
fn a_recommendation_naming_a_letter_past_the_list_is_dropped_and_the_question_stands() {
    let (rig, runner) = asking("gloom", CHOICES);
    rig.claude.script([Scripted::Say(
        "{\"answer\": {\"item\": 7, \"escalate\": \"C\", \"why\": \"guess\"}}",
    )]);
    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (_, acted, dropped) = answered(Some(report));
    assert_eq!(acted, Vec::<String>::new());
    assert_eq!(
        dropped,
        ["answer #7: choice C is past the 2 options the question lists"]
    );
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(7) })
    );
    let [(_, alert)] = rig.alerts.posts().try_into().unwrap();
    assert_eq!(
        alert.text,
        "The worker on issue #7 asks:\n\nWhich flag?\n- `--dry-run`\n- `--check`\n\n\
         `shep kelpie rule 7 <text>` sends the worker your answer."
    );
}

const LANGUAGES: &str = "<kelpie-question>\nWhich language?\n- Go\n- C\n- Rust\n\
                         </kelpie-question>\n";

const TWO_LANGUAGES: &str = "<kelpie-question>\nWhich language?\n- Rust\n- C\n\
                             </kelpie-question>\n";

#[test]
fn a_recommendation_naming_an_option_that_is_a_letter_moves_that_option_up() {
    for (project, asked, expected) in [
        ("sandshrew", LANGUAGES, "- C\n- Go\n- Rust"),
        ("sandslash", TWO_LANGUAGES, "- C\n- Rust"),
    ] {
        let (rig, runner) = asking(project, asked);
        let text = passed_on(
            &rig,
            &runner,
            "{\"answer\": {\"item\": 7, \"escalate\": \"C\", \"why\": \"\"}}",
        );
        assert!(text.contains("PM recommends: C\n"), "{text}");
        assert!(text.contains(expected), "{text}");
    }
}

#[test]
fn a_recommendation_in_other_words_is_still_added_as_a_new_choice() {
    let (rig, runner) = asking("vileplume", CHOICES);
    let text = passed_on(
        &rig,
        &runner,
        "{\"answer\": {\"item\": 7, \"escalate\": \"Neither flag, run `--plan`\", \"why\": \"\"}}",
    );
    assert_eq!(
        text,
        "The worker on issue #7 asks:\n\nWhich flag?\nPM recommends: Neither flag, run `--plan`\n\
         - Neither flag, run `--plan`\n- `--dry-run`\n- `--check`\n\n\
         `shep kelpie rule 7 <text>` sends the worker your answer."
    );
}

#[test]
fn a_question_with_no_list_gets_the_recommendation_line_alone() {
    let (rig, runner) = asking("venusaur", ASKS);
    let text = passed_on(
        &rig,
        &runner,
        "{\"answer\": {\"item\": 7, \"escalate\": \"--check\", \"why\": \"\"}}",
    );
    assert!(
        text.contains("Both appear in the docs.\nPM recommends: --check\n\n"),
        "{text}"
    );
}

#[test]
fn an_answer_for_an_item_not_parked_on_a_question_is_dropped_and_the_ruling_stands() {
    let (rig, runner) = asking("charmander", ASKS);
    rig.claude.script([Scripted::Say(
        "{\"answer\": {\"item\": 9, \"text\": \"Use --check.\", \"why\": \"guess\"}}",
    )]);
    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (_, acted, dropped) = answered(Some(report));
    assert_eq!(acted, Vec::<String>::new());
    assert_eq!(
        dropped,
        ["answer #9: not a work item parked on a question or follow-up ruling"]
    );
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(7) })
    );
    let [(_, alert)] = rig.alerts.posts().try_into().unwrap();
    assert_eq!(
        alert.text,
        "The worker on issue #7 asks:\n\nShould it be `--dry-run` or `--check`?\n\n\
         Both appear in the docs.\n\n\
         `shep kelpie rule 7 <text>` sends the worker your answer."
    );
}

#[test]
fn a_wake_that_fails_sends_the_question_to_the_maintainer_as_it_is() {
    let (rig, runner) = asking("squirtle", ASKS);
    let down = AgentError::Failed(Harness::ClaudeCode, "overloaded".into());
    rig.claude.script([Scripted::Fail(down)]);
    step_until(&runner, |r| matches!(r, StepReport::PmFailed { .. }));
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(7) })
    );
    assert_eq!(rig.alerts.posts().len(), 1);
}

#[test]
fn an_answer_after_the_maintainer_was_alerted_is_dropped() {
    let (rig, runner) = asking("raichu", ASKS);
    let down = AgentError::Failed(Harness::ClaudeCode, "overloaded".into());
    rig.claude.script([Scripted::Fail(down)]);
    step_until(&runner, |r| matches!(r, StepReport::PmFailed { .. }));
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(7) })
    );

    rig.clock.advance(11 * 60);
    rig.ask(&runner, "tell", Some("Use --check"));
    rig.claude.script([Scripted::Say(
        "{\"answer\": {\"item\": 7, \"text\": \"Use --check.\", \"why\": \"told\"}}",
    )]);
    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (_, acted, dropped) = answered(Some(report));
    assert_eq!(acted, Vec::<String>::new());
    assert_eq!(
        dropped,
        ["answer #7: ruling 7 already went to the maintainer"]
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["id"], "7", "the ruling still waits");
    assert_eq!(rig.claude.calls().len(), 1, "the worker is not resumed");
}

#[test]
fn a_recommendation_naming_this_machine_is_withheld_from_the_post() {
    let (rig, runner) = asking("meowth", CHOICES);
    let home = rig.paths().pm.display().to_string();
    let answer = format!("{{\"answer\": {{\"item\": 7, \"escalate\": \"{home}\"}}}}");
    let text = passed_on(&rig, &runner, Box::leak(answer.into_boxed_str()));
    assert!(!text.contains(&home), "{text}");
    assert!(
        text.contains("PM recommends: (withheld: it names"),
        "{text}"
    );
}

// Issue 7's worker asking `ASKS` on its pull request 71
fn asking_on_a_pull_request(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner) = with_pm(project);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Say(ASKS)]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Asked {
            pull_request: Some(71),
            ..
        })
    ));
    (rig, runner)
}

#[test]
fn a_question_it_answers_is_never_commented_on_its_pull_request() {
    let (rig, runner) = asking_on_a_pull_request("lapras");
    assert_eq!(rig.forge.comments(), [], "held for the project manager");
    rig.claude.script([Scripted::Say(
        "{\"answer\": {\"item\": 7, \"text\": \"Use --check.\", \"why\": \"the docs\"}}",
    )]);
    step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    rig.claude.script([Scripted::Say("renamed")]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Ended { .. })
    ));
    assert_eq!(rig.forge.comments(), []);
}

#[test]
fn a_question_it_passes_on_is_commented_with_its_recommendation() {
    let (rig, runner) = asking_on_a_pull_request("snorlax");
    assert_eq!(rig.forge.comments(), [], "held for the project manager");
    passed_on(
        &rig,
        &runner,
        "{\"answer\": {\"item\": 7, \"escalate\": \"--check\", \"why\": \"shep uses it\"}}",
    );
    let [(number, body)] = rig.forge.comments().try_into().unwrap();
    assert_eq!(number, 71);
    assert!(body.contains("PM recommends: --check"), "{body}");
}

#[test]
fn a_question_whose_wake_fails_is_commented_as_it_stands() {
    let (rig, runner) = asking_on_a_pull_request("eevee");
    let down = AgentError::Failed(Harness::ClaudeCode, "overloaded".into());
    rig.claude.script([Scripted::Fail(down)]);
    step_until(&runner, |r| matches!(r, StepReport::PmFailed { .. }));
    step(&runner).unwrap();
    let [(number, body)] = rig.forge.comments().try_into().unwrap();
    assert_eq!(number, 71);
    assert!(body.contains("Should it be `--dry-run`"), "{body}");
    assert!(!body.contains("PM recommends"), "{body}");
}

#[test]
fn a_trigger_is_answered_while_a_released_questions_comment_is_posted() {
    let (rig, runner) = asking_on_a_pull_request("togepi");
    let down = AgentError::Failed(Harness::ClaudeCode, "overloaded".into());
    rig.claude.script([Scripted::Fail(down)]);
    step_until(&runner, |r| matches!(r, StepReport::PmFailed { .. }));
    let comment = rig.forge.hold_next_comment();

    let (reply, pass) = rig.asked_while_held(&runner, comment, "add", Some("9"));

    assert!(reply.is_some(), "the trigger waited for the comment");
    pass.unwrap();
    assert_eq!(rig.forge.comments().len(), 1, "the comment went up");
}

// The comment is placed once it is up, so a ruling settled in between has no
// comment to edit until then.
#[test]
fn a_released_question_settled_while_its_comment_is_posted_is_edited_to_say_so() {
    let rig = Rig::new("slowpoke");
    rig.with_app();
    rig.maintainer("octocat");
    rig.forge.list_ready(7, false);
    std::fs::write(rig.kelpie_settings_file(), "").unwrap();
    let (rig, runner) = pm_on(rig);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Say(ASKS)]);
    step_until(&runner, |r| matches!(r, StepReport::Asked { .. }));
    let down = AgentError::Failed(Harness::ClaudeCode, "overloaded".into());
    rig.claude.script([Scripted::Fail(down)]);
    step_until(&runner, |r| matches!(r, StepReport::PmFailed { .. }));
    let comment = rig.github.hold_next_comment();

    let ask = || rig.ask(&runner, "rule", Some("71 answer Use --check."));
    let (reply, pass) = rig.during_read(comment, || advance(&runner), ask);

    let reply = reply.expect("the trigger waited for the comment");
    assert!(reply.get("error").is_none(), "{reply}");
    pass.unwrap();
    let edits = rig.github.edits();
    assert_eq!(edits.len(), 1, "the comment was never edited: {edits:?}");
    assert!(edits[0].1.starts_with("**Settled.**"), "{edits:?}");
}

#[test]
fn a_question_waits_at_most_one_wake_for_its_answer() {
    let (rig, runner) = asking("wartortle", ASKS);
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(hold.entered(CALL_PATIENCE), "the wake never began");
    assert!(rig.alerts.posts().is_empty(), "the maintainer waits for it");

    rig.clock.advance(10 * 60);
    step_until(
        &runner,
        |r| matches!(r, StepReport::Alerted { id } if *id == ticket(7)),
    );
    hold.release();
}
