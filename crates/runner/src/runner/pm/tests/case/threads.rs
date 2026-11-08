//! Who wrote each open thread and comment on the pull request, and which of
//! their words reach the case and which reach nothing at all

use super::rig::{BODY, case, everything_it_holds, red, red_with_coderabbit, wake};
use crate::ports::Standing;

#[test]
fn a_listed_bots_thread_and_a_writers_thread_are_in_the_case() {
    let (rig, runner) = red_with_coderabbit("case-bots");
    let says = &rig.forge.coderabbit;
    says.person_opens(
        71,
        "src/a.rs",
        "coderabbitai",
        Standing::Bot,
        "Overflows here.",
    );
    says.person_opens(71, "src/b.rs", "octocat", Standing::Writer, "Rename this.");

    wake(&rig, &runner);

    let case = case(&rig, 7).unwrap();
    assert!(case.contains("#### `src/a.rs:3`"), "{case}");
    assert!(
        case.contains("coderabbitai, a listed bot, wrote:\n\n> Overflows here."),
        "{case}"
    );
    assert!(case.contains("#### `src/b.rs:3`"), "{case}");
    assert!(
        case.contains("octocat, a writer, wrote:\n\n> Rename this."),
        "{case}"
    );
    assert!(!case.contains("left out"), "{case}");
}

#[test]
fn a_resolved_or_outdated_thread_is_not_in_the_case() {
    let (rig, runner, _) = red("case-closed", "A title", BODY);
    let says = &rig.forge.coderabbit;
    let done = says.person_opens(71, "a.rs", "octocat", Standing::Writer, "RESOLVED-WORDS");
    let old = says.person_opens(71, "b.rs", "octocat", Standing::Writer, "OUTDATED-WORDS");
    says.person_opens(71, "c.rs", "octocat", Standing::Writer, "OPEN-WORDS");
    says.resolve(&done).unwrap();
    says.person_outdates(&old);

    wake(&rig, &runner);

    let case = case(&rig, 7).unwrap();
    assert!(case.contains("> OPEN-WORDS"), "{case}");
    assert!(!case.contains("RESOLVED-WORDS"), "{case}");
    assert!(!case.contains("OUTDATED-WORDS"), "{case}");
    assert!(
        !case.contains("left out"),
        "closed threads are no stranger's: {case}"
    );
}

#[test]
fn a_strangers_thread_is_counted_and_its_words_are_nowhere() {
    let (rig, runner, _) = red("case-stranger", "A title", BODY);
    let says = &rig.forge.coderabbit;
    says.person_opens(71, "a.rs", "passer-by", Standing::Other, "STRANGER-WORDS");
    says.person_opens(71, "b.rs", "octocat", Standing::Writer, "WRITER-WORDS");

    let seen = wake(&rig, &runner);

    let case = case(&rig, 7).unwrap();
    assert!(case.contains("1 open thread was left out"), "{case}");
    assert!(case.contains("> WRITER-WORDS"), "{case}");
    assert!(!everything_it_holds(&rig).contains("STRANGER-WORDS"));
    assert!(!seen.call.prompt.contains("STRANGER-WORDS"));
}

#[test]
fn an_unlisted_bots_thread_is_counted_and_its_words_are_nowhere() {
    let (rig, runner) = red_with_coderabbit("case-unlisted");
    let says = &rig.forge.coderabbit;
    says.person_opens(71, "a.rs", "other-bot", Standing::Bot, "UNLISTED-WORDS");
    says.person_opens(71, "b.rs", "coderabbitai", Standing::Bot, "LISTED-WORDS");

    wake(&rig, &runner);

    let case = case(&rig, 7).unwrap();
    assert!(case.contains("1 open thread was left out"), "{case}");
    assert!(case.contains("> LISTED-WORDS"), "{case}");
    assert!(!everything_it_holds(&rig).contains("UNLISTED-WORDS"));
}

#[test]
fn a_strangers_reply_in_a_writers_thread_is_left_out() {
    let (rig, runner, _) = red("case-reply", "A title", BODY);
    let says = &rig.forge.coderabbit;
    let thread = says.person_opens(71, "a.rs", "octocat", Standing::Writer, "WRITER-ASKS");
    says.person_replies(&thread, "passer-by", Standing::Other, "STRANGER-REPLIES");

    wake(&rig, &runner);

    let case = case(&rig, 7).unwrap();
    assert!(case.contains("> WRITER-ASKS"), "{case}");
    assert!(!everything_it_holds(&rig).contains("STRANGER-REPLIES"));
    assert!(
        !case.contains("left out"),
        "the thread itself stays: {case}"
    );
}

#[test]
fn a_strangers_comment_on_the_pull_request_is_left_out() {
    let (rig, runner, _) = red("case-comment", "A title", BODY);
    let says = &rig.forge.coderabbit;
    says.person_says(71, "passer-by", Standing::Other, "STRANGER-COMMENT");
    says.person_says(71, "octocat", Standing::Writer, "WRITER-COMMENT");

    wake(&rig, &runner);

    let case = case(&rig, 7).unwrap();
    assert!(
        case.contains("octocat, a writer, wrote:\n\n> WRITER-COMMENT"),
        "{case}"
    );
    assert!(!everything_it_holds(&rig).contains("STRANGER-COMMENT"));
}
