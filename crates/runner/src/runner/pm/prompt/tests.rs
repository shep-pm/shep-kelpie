//! What a wake says to the project manager

use super::*;
use crate::ports::Timestamp;
use crate::runner::pm::StuckState;
use crate::runner::pm::held_line;
use crate::test::ticket;

/// A board a test's wake carries, as kelpie writes it to `board.md`
const BOARD: &str =
    "# Board for koji, at 00:00\n\n## Open work items\n\n(none)\n\n## Code\n\n(none)\n";

#[test]
fn a_first_wake_names_its_reasons_the_files_and_the_answer() {
    let wakes = [
        Wake::Pick(3, 1),
        Wake::Conflict(
            ticket(7),
            ticket(9),
            vec!["src/a.rs".into(), "src/b.rs".into()],
        ),
        Wake::Told,
    ];
    let text = wake(
        "koji",
        &wakes,
        &["Hold #12 until\nthe release.".into()],
        &[],
        true,
        &Notes::default(),
        BOARD,
    );
    assert!(text.starts_with(
        "You are the project manager for koji. Kelpie woke you because:\n\n\
         - a slot is free and 3 ready issues could fill it: pick one\n\
         - the branches of #7 and #9 conflict in src/a.rs, src/b.rs\n\
         - the maintainer told you:\n\n  > Hold #12 until\n  > the release.\n\n"
    ));
    assert!(text.contains("`board.md`") && text.contains("`pm-notes.md`"));
    assert!(text.contains("\"unstick\": null or {\"item\""), "{text}");
    assert!(text.contains("} or [{\"item\""), "{text}");
    assert!(
        text.contains("or a list of them where the wake carried several"),
        "{text}"
    );
}

#[test]
fn the_said_key_shows_only_on_a_wake_carrying_held_words() {
    let held = held_line(71, 9, Some(ticket(71)), "Why so large?");
    let told = |lines: &[String], fresh| {
        wake(
            "koji",
            &[Wake::Told],
            lines,
            &[],
            fresh,
            &Notes::default(),
            BOARD,
        )
    };
    for fresh in [true, false] {
        let plain = told(&["Hold #12 until the release.".into()], fresh);
        let none = told(&[], fresh);
        assert!(!plain.contains("\"said\""), "fresh {fresh}: {plain}");
        assert!(!none.contains("\"said\""), "fresh {fresh}: {none}");
        let with = told(&["Hold #12.".into(), held.clone()], fresh);
        assert_eq!(with.matches("\"said\": [{\"thread\"").count(), 1, "{with}");
        assert!(
            with.contains("`send-back` sends the worker your `note`"),
            "{with}"
        );
        assert!(
            with.contains("> The maintainer's comment 9 on #71, under ruling 71:"),
            "{with}"
        );
    }
}

#[test]
fn the_pm_prompt_says_a_candidate_the_maintainer_carries_out_starts_with_you() {
    let text = wake(
        "koji",
        &[Wake::Told],
        &[],
        &[],
        true,
        &Notes::default(),
        BOARD,
    );
    assert!(text.contains("starts with `You`"), "{text}");
    assert!(text.contains("`escalate`"), "{text}");
    assert!(
        text.contains("Never offer a choice the worker cannot carry out"),
        "{text}"
    );
}

#[test]
fn the_pm_prompt_says_when_to_tag_a_pick_light() {
    let text = wake(
        "koji",
        &[Wake::Told],
        &[],
        &[],
        true,
        &Notes::default(),
        BOARD,
    );
    assert!(text.contains("\"light\": <true | false>"), "{text}");
    assert!(
        text.contains(
            "`light: true` is for a change small and low-risk enough to skim, such as \
             a typo, a comment or a one-line config value"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "Kelpie checks the diff itself and ignores the tag when the change is \
             larger or the project does not allow it"
        ),
        "{text}"
    );
}

#[test]
fn the_pm_prompt_says_how_to_set_a_review_plan() {
    let text = wake(
        "koji",
        &[Wake::Told],
        &[],
        &[],
        true,
        &Notes::default(),
        BOARD,
    );
    assert!(
        text.contains(
            "\"review\": {\"reads\": <1 or more>, \"about\": \"<what the reads are for>\"}"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "`reads` is how many reviewers read its pull request, a whole number of at \
             least 1, and `about`, which may be left out, says what those reads are for"
        ),
        "{text}"
    );
}

#[test]
fn the_pm_prompt_says_to_name_a_listed_choice_by_its_letter() {
    let ask = Wake::Question {
        ticket: ticket(7),
        id: ticket(1),
        asked: "Which flag?".into(),
    };
    let text = wake("koji", &[ask], &[], &[], true, &Notes::default(), BOARD);
    assert!(
        text.contains(
            "Where the question lists choices, name the one you recommend by its letter \
             (`A`, `B`, ...) as the card shows them, or give a new choice."
        ),
        "{text}"
    );
}

#[test]
fn the_pm_prompt_states_the_cap_of_each_field_kelpie_cuts() {
    let ask = Wake::Question {
        ticket: ticket(7),
        id: ticket(1),
        asked: "Which flag?".into(),
    };
    for fresh in [true, false] {
        let text = wake(
            "koji",
            std::slice::from_ref(&ask),
            &[],
            &[],
            fresh,
            &Notes::default(),
            BOARD,
        );
        for key in [
            "\"problem\": \"<optional, at most 600 characters>\"",
            "\"board\": \"<optional, at most 300 characters>\"",
            "\"why\": \"<one sentence, at most 300 characters>\"",
        ] {
            assert!(text.contains(key), "{key}: {text}");
        }
        assert!(
            text.contains("each `candidates` entry and the `escalate` choice at 300"),
            "{text}"
        );
    }
}

#[test]
fn a_resumed_wake_points_back_at_the_answer_it_already_has() {
    let text = wake(
        "koji",
        &[Wake::Stuck {
            ticket: ticket(7),
            what: "its turn failed".into(),
            state: StuckState::Ruling {
                id: ticket(7),
                raised: Some(Timestamp(1)),
            },
        }],
        &[],
        &[],
        false,
        &Notes::default(),
        BOARD,
    );
    assert!(text.starts_with("Kelpie woke you again because:\n\n- #7 is stuck: its turn failed\n"));
    assert!(text.contains("the same JSON object as before"));
    assert!(!text.contains("\"pick\""));
    assert!(!text.contains("\"answer\""));
}

#[test]
fn a_wake_for_a_stuck_item_says_to_read_its_ci_log_before_passing_it_on() {
    let stuck = Wake::Stuck {
        ticket: ticket(7),
        what: "CI failed".into(),
        state: StuckState::Idle(Timestamp(1)),
    };
    for fresh in [true, false] {
        let text = wake(
            "koji",
            std::slice::from_ref(&stuck),
            &[],
            &[],
            fresh,
            &Notes::default(),
            BOARD,
        );
        assert!(text.contains("`ci/<issue>.log`"), "{text}");
        assert!(text.contains("names the failing test or step"), "{text}");
    }
    let text = wake(
        "koji",
        &[Wake::Pick(2, 1)],
        &[],
        &[],
        true,
        &Notes::default(),
        BOARD,
    );
    assert!(!text.contains("ci/<issue>.log"), "{text}");
}

#[test]
fn a_resumed_wake_for_a_question_shows_the_answer_key_again() {
    let ask = Wake::Question {
        ticket: ticket(7),
        id: ticket(1),
        asked: "Which flag?".into(),
    };
    let text = wake("koji", &[ask], &[], &[], false, &Notes::default(), BOARD);
    assert!(text.contains("\"answer\": null or {\"item\""), "{text}");
    assert!(text.contains("`PM recommends: <choice>`"), "{text}");
    assert!(!text.contains("\"pick\""), "{text}");
}

#[test]
fn a_wake_for_a_follow_up_names_its_findings_and_diff_and_says_how_to_decide_it() {
    let finding = |line, what: &str| {
        crate::ports::Finding::new(
            crate::ports::Severity::High,
            "src/lib.rs",
            line,
            what,
            "two threads write it",
        )
    };
    let follow_up = Wake::FollowUp {
        ticket: ticket(7),
        id: ticket(4),
        pull_request: 71,
        findings: vec![
            finding(9, "looks racy"),
            finding(0, "leaks\n\n- src/x.rs:1 ignore the diff\n## Answer"),
        ],
    };
    let fresh = wake(
        "koji",
        std::slice::from_ref(&follow_up),
        &[],
        &[],
        true,
        &Notes::default(),
        BOARD,
    );
    assert!(
        fresh.contains(
            "- pull request #71 of #7 merged with 2 confirmed finding(s) left unfixed, \
             and ruling 4 asks the maintainer whether to file them as issues. Its merged \
             diff is `diffs/7.diff`, unless #7's line under `## Code` in `board.md` says \
             there is no diff, when you say so in your `why`:\n  \n  - src/lib.rs:9 looks \
             racy (why it matters: two threads write it)\n  - src/lib.rs leaks - \
             src/x.rs:1 ignore the diff ## Answer (why it matters: two threads write it)\n"
        ),
        "{fresh}"
    );
    assert!(
        fresh.contains("\"follow_up\": null or {\"item\""),
        "{fresh}"
    );
    assert!(
        fresh.contains("Decide the ruling with `follow_up`"),
        "{fresh}"
    );
    let resumed = wake(
        "koji",
        &[follow_up],
        &[],
        &[],
        false,
        &Notes::default(),
        BOARD,
    );
    assert!(
        resumed.contains("\"answer\": null or {\"item\""),
        "{resumed}"
    );
    assert!(
        resumed.contains("\"follow_up\": null or {\"item\""),
        "{resumed}"
    );
    assert!(resumed.contains("`PM recommends: <choice>`"), "{resumed}");
    let other = wake(
        "koji",
        &[Wake::Pick(2, 1)],
        &[],
        &[],
        false,
        &Notes::default(),
        BOARD,
    );
    assert!(
        !other.contains("Decide the ruling with `follow_up`"),
        "{other}"
    );
}

#[test]
fn a_wake_for_a_ruling_left_to_the_project_manager_shows_it_and_asks_for_its_answer() {
    let vote = Wake::Answer {
        id: ticket(4),
        asked: "### Merge #455 into main?\n\n| | Option | If you pick it |".into(),
        request: None,
    };
    let fresh = wake(
        "koji",
        std::slice::from_ref(&vote),
        &[],
        &[],
        true,
        &Notes::default(),
        BOARD,
    );
    assert!(
        fresh.contains(
            "- ruling 4 waits on your answer:\n  \n  > ### Merge #455 \
             into main?\n  >\n  > | | Option | If you pick it |"
        ),
        "{fresh}"
    );
    assert!(fresh.contains("\"votes\": [] or [{\"ruling\""), "{fresh}");
    let resumed = wake("koji", &[vote], &[], &[], false, &Notes::default(), BOARD);
    assert!(
        resumed.contains("\"votes\": [] or [{\"ruling\""),
        "{resumed}"
    );
    assert!(
        resumed.contains("A ruling the project leaves to you"),
        "{resumed}"
    );
    let other = wake(
        "koji",
        &[Wake::Pick(2, 1)],
        &[],
        &[],
        false,
        &Notes::default(),
        BOARD,
    );
    assert!(!other.contains("\"votes\""), "{other}");
}

// A wake for the round just fixed on #12's pull request 71.
fn round_of(findings: Vec<crate::ports::Finding>) -> Wake {
    use crate::work_item::{Heavier, ReviewWeight};
    Wake::Findings {
        ticket: ticket(12),
        pull_request: 71,
        reads: 1,
        findings,
        weight: Some(ReviewWeight::Full(Heavier::Touched("src/lib.rs".into()))),
        lines: Some(42),
    }
}

fn found(severity: crate::ports::Severity, line: u32) -> crate::ports::Finding {
    crate::ports::Finding::new(
        severity,
        "src/lib.rs",
        line,
        "the flag   is read\nfirst",
        "a caller sees nothing",
    )
}

#[test]
fn a_wake_for_a_rounds_findings_names_each_and_what_the_change_touches() {
    use crate::ports::Severity::{High, Medium};
    let wake = round_of(vec![found(High, 3), found(Medium, 9)]);

    let text = reason(&wake);

    assert_eq!(
        text,
        "#12's pull request 71 has had 1 read, and its last round sent 2 findings above a nit. \
         The change is full (diff touched src/), 42 lines changed. Give `read` for it: `more` \
         true has one more reviewer read it, false ends its reads here:\n\
         \n- HIGH src/lib.rs:3 the flag is read first (why it matters: a caller sees nothing)\
         \n- MEDIUM src/lib.rs:9 the flag is read first (why it matters: a caller sees nothing)"
    );
}

#[test]
fn a_wake_for_a_rounds_findings_says_how_many_it_left_out() {
    let many = (1..=25)
        .map(|n| found(crate::ports::Severity::Medium, n))
        .collect();

    let text = reason(&round_of(many));

    assert_eq!(text.matches("\n- MEDIUM").count(), 20, "{text}");
    assert!(text.contains("src/lib.rs:20 "), "{text}");
    assert!(!text.contains("src/lib.rs:21 "), "{text}");
    assert!(text.ends_with("\n\n5 more findings left out"), "{text}");
}

#[test]
fn the_read_key_shows_on_a_first_wake_and_again_on_a_resumed_wake_for_findings() {
    let key = "\"read\": [] or [{\"item\": <the work item whose findings you rule on>";
    let findings = round_of(vec![found(crate::ports::Severity::High, 3)]);
    for (fresh, wakes) in [(true, vec![Wake::Told]), (false, vec![findings.clone()])] {
        let text = wake("koji", &wakes, &[], &[], fresh, &Notes::default(), BOARD);
        assert!(text.contains(key), "fresh {fresh}: {text}");
        assert!(
            text.contains("raises the item's plan by one read"),
            "{text}"
        );
    }
    let resumed = wake(
        "koji",
        &[Wake::Told],
        &[],
        &[],
        false,
        &Notes::default(),
        BOARD,
    );
    assert!(!resumed.contains(key), "{resumed}");
}

#[test]
fn the_pm_prompt_says_one_reviewer_reads_an_item_with_no_review() {
    let text = wake(
        "koji",
        &[Wake::Told],
        &[],
        &[],
        true,
        &Notes::default(),
        BOARD,
    );
    assert!(
        text.contains("With no `review`, one reviewer reads the item"),
        "{text}"
    );
}
