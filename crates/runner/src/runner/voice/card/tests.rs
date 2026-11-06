use super::*;
use crate::ports::{Finding, Severity};
use crate::runner::ruling::question;
use crate::state::{AgentFit, AgentMeasure, OpenThread, Resume, Ruling, RulingKind, Stuck};
use crate::test::ticket;
use crate::work_item::{Known, Phase};

// A ruling on pull request 71, whose question is the one kelpie words for it
fn ruling(id: TicketId, kind: RulingKind) -> Ruling {
    Ruling {
        issue: Some(ticket(7)),
        pull_request: Some(71),
        ..Ruling::new(id, question(ticket(7), Some(71), &kind), kind)
    }
}

// ... with the project manager's words added to its question, as it adds them
fn said_by_the_pm(mut ruling: Ruling, words: &str) -> Ruling {
    ruling.question = format!(
        "{}\n\nThe project manager asks you to decide. PM says: {words}",
        ruling.question
    );
    ruling
}

fn merge() -> RulingKind {
    RulingKind::Merge {
        head: "6b0606e9b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6".into(),
        unreviewed: None,
        open_threads: None,
        held_by: Vec::new(),
        unread_head: false,
        note_fix: false,
        late_fix: false,
        nits: None,
        open_found: Vec::new(),
        disputes: None,
        rivals: Vec::new(),
    }
}

// cubic's thread on `file:line`, at the level it wrote
fn cubic(level: &str, severity: Severity, file: &str, line: u32, what: &str) -> OpenThread {
    OpenThread {
        bot: "cubic".into(),
        level: Some(level.into()),
        severity,
        file: file.into(),
        line,
        what: what.into(),
    }
}

// A merge ruling whose head has `found` open, and `nits` named apart
fn merge_with(found: Vec<OpenThread>, nits: Option<&str>) -> RulingKind {
    let above = found.iter().any(OpenThread::above_a_nit);
    let RulingKind::Merge { head, .. } = merge() else {
        unreachable!("merge() is a merge ruling");
    };
    RulingKind::Merge {
        head,
        unreviewed: None,
        open_threads: above.then(|| "1 from cubic".into()),
        held_by: Vec::new(),
        unread_head: false,
        note_fix: false,
        late_fix: false,
        nits: nits.map(Into::into),
        open_found: found,
        disputes: None,
        rivals: Vec::new(),
    }
}

fn asking(asked: &str) -> RulingKind {
    RulingKind::Question {
        asked: asked.into(),
        resume: Resume::Nothing,
    }
}

// The expected comment: a file under `fixtures/rulings`, less its last newline
macro_rules! expected {
    ($name:literal) => {
        include_str!(concat!("../../../../../../fixtures/rulings/", $name)).trim_end()
    };
}

fn rendered(ruling: &Ruling) -> String {
    ruling_comment(Some("octocat"), ruling)
}

// A `still_red` ruling, with the rerun option when it was raised with one
fn still_red(rerun: bool) -> RulingKind {
    RulingKind::Stuck(Stuck::StillRed {
        head: "6b0606e9b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6".into(),
        checks: vec!["test".into()],
        fix_turns: None,
        review: None,
        rerun,
    })
}

// A `foreign-change` ruling: the plainest card that takes a yes and a no
fn changed_outside() -> RulingKind {
    RulingKind::ForeignChange {
        description: "labels added: `ready-for-human`".into(),
        known: Known::default(),
    }
}

#[test]
fn a_yes_or_no_ruling_names_each_option_once_by_its_answer() {
    let comment = rendered(&ruling(ticket(1), changed_outside()));

    assert_eq!(
        comment,
        "### Accept a change made outside kelpie?\n\n\
         This pull request was changed: labels added: `ready-for-human`.\n\n\
         | Answer | What it does |\n\
         |---|---|\n\
         | 👍 or `71 yes` | Accepts the change and kelpie carries on |\n\
         | 👎 or `71 no <note>` | Sends the worker back with a note, and the review reads its change again |\n\n\
         @octocat"
    );
}

#[test]
fn a_yes_or_no_ruling_with_one_choice_offers_only_the_thumbs_up() {
    let kind = RulingKind::Stuck(Stuck::AdoptFailed {
        why: "its branch is held by a worktree".into(),
    });
    let comment = rendered(&ruling(ticket(1), kind));

    assert!(
        comment.contains("| 👍 or `71 yes` | Kelpie tries again, as it does every poll |"),
        "{comment}"
    );
    assert!(!comment.contains("👎"), "{comment}");
    assert!(!comment.contains("no <note>"), "{comment}");
}

#[test]
fn an_option_that_names_an_action_shows_its_reply_alone() {
    let comment = rendered(&ruling(ticket(1), still_red(true)));

    assert!(
        comment.contains(
            "| `71 rerun` | Reruns the failed jobs of 6b0606e once more, and kelpie looks at \
             CI again |"
        ),
        "{comment}"
    );
}

#[test]
fn an_unmentioned_yes_or_no_ruling_shows_the_terminal_replies() {
    let comment = ruling_comment(None, &ruling(ticket(1), still_red(true)));

    assert!(
        comment.contains("| `shep kelpie rule 71 yes` | Kelpie looks at CI again |"),
        "{comment}"
    );
    assert!(
        comment.contains("| `shep kelpie rule 71 no <note>` | Sends the worker back with a note"),
        "{comment}"
    );
    assert!(
        comment.contains("| `shep kelpie rule 71 rerun` |"),
        "{comment}"
    );
    assert!(!comment.contains("👍"), "{comment}");
    assert!(comment.ends_with("Answer it at the terminal."), "{comment}");
}

#[test]
fn the_project_manager_still_reads_lettered_options() {
    let (shown, count) = brief(&ruling(ticket(1), changed_outside()));

    assert_eq!(count, 2);
    assert!(shown.contains("| | Option | If you pick it |"), "{shown}");
    assert!(
        shown.contains("| **A** | Yes | Accepts the change and kelpie carries on |"),
        "{shown}"
    );
    assert!(shown.contains("| **B** | No |"), "{shown}");
    assert!(!shown.contains("👍"), "{shown}");
}

#[test]
fn a_project_managers_pick_on_a_yes_or_no_card_reads_by_name() {
    let mut ruling = ruling(ticket(1), changed_outside());
    ruling.question = format!(
        "{}\n\nPM recommends: No\nPM says: the label is yours.",
        ruling.question
    );
    let comment = rendered(&ruling);

    assert!(
        comment.contains("**Project manager: No**. the label is yours."),
        "{comment}"
    );
}

// A merge comment carrying boxes to tick
const POSTED_WITH_BOXES: &str = "### Merge this pull request at 6b0606e into main?\n\n\
     | | Option | If you pick it |\n\
     |---|---|---|\n\
     | **A** | Merge | Merges it into main |\n\
     | **B** | Send back | Sends the worker back for a fix that goes to CI and back to you |\n\
     | **C** | Rework | Sends the worker back for a change the whole review reads again |\n\n\
     @octocat\n\
     - 👍 or 👎 answers, or tick one:\n\
     - [ ] A\n\
     - [ ] B\n\
     - [ ] C\n\
     - Or reply `71 no <note>` or `71 rework <note>`";

#[test]
fn a_merge_ruling_names_each_option_once_by_its_answer() {
    let comment = rendered(&ruling(ticket(1), merge()));

    assert_eq!(comment, expected!("merge.md"));
    assert!(!comment.contains("- [ ]"), "{comment}");
    assert!(!comment.contains("tick one"), "{comment}");
}

#[test]
fn a_merge_comment_posted_with_boxes_can_still_be_ticked() {
    let kind = merge();
    let ticked = |key: char| {
        ticked_answer(
            &kind,
            &POSTED_WITH_BOXES.replace(&format!("- [ ] {key}"), &format!("- [x] {key}")),
        )
    };

    assert_eq!(ticked('A'), Some(Answer::Yes));
    assert_eq!(ticked('B'), Some(Answer::No(NO_NOTE.into())));
    assert_eq!(ticked('C'), Some(Answer::Rework(NO_NOTE.into())));
    assert_eq!(
        ticked_answer(&kind, &POSTED_WITH_BOXES.replace("- [ ] ", "- [x] ")),
        None,
        "two ticked"
    );
}

#[test]
fn a_merge_ruling_that_needs_a_pass_offers_no_rework() {
    let kind = RulingKind::Merge {
        head: "6b0606e9b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6".into(),
        unreviewed: Some("the reviewer was down".into()),
        open_threads: Some("coderabbit".into()),
        held_by: Vec::new(),
        unread_head: false,
        note_fix: false,
        late_fix: false,
        nits: None,
        open_found: Vec::new(),
        disputes: None,
        rivals: Vec::new(),
    };

    let comment = rendered(&ruling(ticket(1), kind));

    assert_eq!(comment, expected!("merge-open-threads.md"));
    assert!(!comment.contains("rework"), "{comment}");
}

#[test]
fn a_ruling_that_mentions_no_one_says_to_answer_it_at_the_terminal() {
    assert_eq!(
        ruling_comment(None, &ruling(ticket(1), merge())),
        expected!("merge-unmentioned.md")
    );
}

#[test]
fn an_unmentioned_ruling_names_only_the_terminal_answers_its_options_allow() {
    let kind = RulingKind::Merge {
        head: "6b0606e9b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6".into(),
        unreviewed: Some("the reviewer was down".into()),
        open_threads: None,
        held_by: Vec::new(),
        unread_head: false,
        note_fix: false,
        late_fix: false,
        nits: None,
        open_found: Vec::new(),
        disputes: None,
        rivals: Vec::new(),
    };
    let no_rework = ruling_comment(None, &ruling(ticket(1), kind));
    let stuck = RulingKind::Stuck(Stuck::AdoptFailed {
        why: "its branch is held by a worktree".into(),
    });
    let only_yes = ruling_comment(None, &ruling(ticket(1), stuck));

    assert!(
        no_rework.contains("| `shep kelpie rule 71 no <note>` |"),
        "{no_rework}"
    );
    assert!(!no_rework.contains("rework"), "{no_rework}");
    assert!(
        no_rework.ends_with("Answer it at the terminal."),
        "{no_rework}"
    );
    assert!(
        only_yes
            .contains("| `shep kelpie rule 71 yes` | Kelpie tries again, as it does every poll |"),
        "{only_yes}"
    );
    assert!(
        only_yes.ends_with("Answer it at the terminal."),
        "{only_yes}"
    );
}

#[test]
fn a_question_becomes_the_options_it_lists_plus_an_other_row() {
    let asked = "Should it be `--dry-run` or `--check`?\n\nBoth appear in the docs.\n\
                 PM recommends: `--dry-run`\nPM says: shep uses `--dry-run` everywhere.\n\
                 - `--dry-run`\n- `--check`";

    assert_eq!(
        rendered(&ruling(ticket(1), asking(asked))),
        expected!("question.md")
    );
}

#[test]
fn a_ticked_line_in_a_workers_question_is_no_box_of_the_comment() {
    let asked = "Where we are:\n- [x] tests pass\n\nWhich flag?\n- `--dry-run`\n- `--check`";
    let comment = rendered(&ruling(ticket(1), asking(asked)));
    let ticked = comment.replace("- [ ] A", "- [x] A");
    let two = comment
        .replace("- [ ] A", "- [x] A")
        .replace("- [ ] B", "- [x] B");

    assert_eq!(
        ticked_answer(&asking(asked), &ticked),
        Some(Answer::Text("--dry-run".into()))
    );
    assert_eq!(ticked_answer(&asking(asked), &two), None);
}

fn ticked_answer(kind: &RulingKind, body: &str) -> Option<Answer> {
    ticked(kind, body)
}

#[test]
fn a_candidate_that_asks_the_maintainer_is_no_answer_to_send_the_worker() {
    let asked = "Which flag?\n- Use `--check`\n- ask the maintainer.";
    let kind = asking(asked);
    let comment = rendered(&ruling(ticket(1), kind.clone()));
    let ticked = comment.replace("- [ ] B", "- [x] B");

    assert!(
        comment.contains("the maintainer answers in their own words"),
        "{comment}"
    );
    assert_eq!(
        answer_for(&kind, 'A', ""),
        Some(Answer::Text("Use `--check`".into()))
    );
    assert_eq!(answer_for(&kind, 'B', ""), None);
    assert_eq!(ticked_answer(&kind, &ticked), None);
}

#[test]
fn an_option_ending_on_code_keeps_its_backticks_and_one_wrapped_whole_loses_them() {
    let kind = asking("Which?\n- Use `--check`\n- `--dry-run`\n- `a` or `b`");

    assert_eq!(
        answer_for(&kind, 'A', ""),
        Some(Answer::Text("Use `--check`".into()))
    );
    assert_eq!(
        answer_for(&kind, 'B', ""),
        Some(Answer::Text("--dry-run".into()))
    );
    assert_eq!(
        answer_for(&kind, 'C', ""),
        Some(Answer::Text("`a` or `b`".into()))
    );
}

#[test]
fn a_question_that_lists_no_options_is_answered_by_reply_alone() {
    let asked = "Which name should the flag have?";

    assert_eq!(
        rendered(&ruling(ticket(1), asking(asked))),
        expected!("question-open.md")
    );
}

#[test]
fn a_stuck_ruling_takes_a_yes_or_a_no_and_carries_the_project_managers_words() {
    let kind = RulingKind::Stuck(Stuck::StillRed {
        head: "6b0606e9b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6".into(),
        checks: vec!["test".into(), "clippy".into()],
        fix_turns: Some(2),
        review: None,
        rerun: false,
    });
    let words = "The failure is a flaky network test, so one rerun should clear it.";

    assert_eq!(
        rendered(&said_by_the_pm(ruling(ticket(1), kind), words)),
        expected!("stuck-still-red.md")
    );
}

#[test]
fn a_still_red_ruling_offers_a_rerun_only_when_it_was_raised_with_one() {
    let offered = rendered(&ruling(ticket(1), still_red(true)));
    let withheld = rendered(&ruling(ticket(1), still_red(false)));

    assert!(
        offered.contains("Reruns the failed jobs of 6b0606e once more"),
        "{offered}"
    );
    assert!(!withheld.contains("Rerun"), "{withheld}");
}

#[test]
fn a_failed_turn_offers_a_restore_naming_the_files_it_puts_back() {
    let kind = RulingKind::Stuck(Stuck::TurnFailed {
        why: "agents' own files differ".into(),
        phase: Phase::Implement,
        retry: crate::work_item::Turn::Due,
        restore: vec![".claude/settings.json".into()],
        setup: false,
    });

    let card = rendered(&ruling(ticket(1), kind));

    assert!(
        card.contains("Restores the uncommitted change to .claude/settings.json"),
        "{card}"
    );
}

#[test]
fn a_stuck_ruling_that_a_no_cannot_answer_offers_only_a_yes() {
    let kind = RulingKind::Stuck(Stuck::AdoptFailed {
        why: "its branch is held by a worktree".into(),
    });

    assert_eq!(
        rendered(&ruling(ticket(1), kind)),
        expected!("stuck-adopt-failed.md")
    );
}

#[test]
fn an_agent_files_ruling_names_the_files() {
    let kind = RulingKind::AgentFiles {
        head: "6b0606e9b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6".into(),
        files: vec![".claude/settings.json".into(), ".mcp.json".into()],
        phase: Phase::Implement,
    };

    assert_eq!(
        rendered(&ruling(ticket(1), kind)),
        expected!("agent-files.md")
    );
}

#[test]
fn an_agent_fit_ruling_shows_both_records_and_the_suggested_kinds() {
    let kind = RulingKind::AgentFit(AgentFit {
        work_kind: "fix".into(),
        worse: "sonnet-high".into(),
        better: "opus-high".into(),
        measure: AgentMeasure::Hours {
            worse: 210,
            better: 95,
        },
        suggested_kinds: vec!["docs".into(), "mechanical".into()],
    });

    assert_eq!(
        rendered(&ruling(ticket(1), kind)),
        expected!("agent-fit.md")
    );
}

#[test]
fn a_foreign_change_ruling_says_what_changed() {
    assert_eq!(
        rendered(&ruling(ticket(1), changed_outside())),
        expected!("foreign-change.md")
    );
}

#[test]
fn a_follow_up_ruling_counts_the_findings_it_would_file() {
    let finding =
        |file: &str| Finding::new(Severity::Medium, file, 3, "unused variable", "dead code");
    let kind = RulingKind::FollowUp {
        findings: vec![finding("src/lib.rs"), finding("src/main.rs")],
        refused: None,
    };

    assert_eq!(
        rendered(&ruling(ticket(1), kind)),
        expected!("follow-up.md")
    );
}

#[test]
fn the_project_managers_words_with_no_pick_stand_alone_on_the_card() {
    let said = said_by_the_pm(ruling(ticket(1), merge()), "I could not tell.");

    assert!(rendered(&said).contains("\n\n**Project manager**: I could not tell.\n\n"));
}

#[test]
fn a_questions_card_opens_with_the_problem_and_shows_each_options_effect_and_the_boards_note() {
    let asked = "Problem: The push hook refuses two commits, and the worker cannot change the hook.\n\
                 Should the worker bypass the hook?\n\n\
                 It names #495.\n\
                 PM recommends: Wait\n\
                 PM board: #495 stays parked and 9 ready issues stay held behind it.\n\
                 PM says: the hook is the maintainer's.\n\
                 - Wait => The worker stops and the push waits for your fix of the hook\n\
                 - `Bypass` => The two commits publish as they are";

    let comment = rendered(&ruling(ticket(1), asking(asked)));

    assert_eq!(
        comment,
        "The push hook refuses two commits, and the worker cannot change the hook.\n\n\
         ### Should the worker bypass the hook?\n\n\
         It names #495.\n\n\
         | | Option | If you pick it |\n\
         |---|---|---|\n\
         | **A** | Wait | The worker stops and the push waits for your fix of the hook |\n\
         | **B** | `Bypass` | The two commits publish as they are |\n\
         | | Other | Reply with your own answer |\n\n\
         **On the board**\n\
         #495 stays parked and 9 ready issues stay held behind it.\n\n\
         **Project manager: A**. the hook is the maintainer's.\n\n\
         @octocat\n\
         - Tick one:\n\
         - [ ] A\n\
         - [ ] B\n\
         - Or reply with your answer, and start it with `71` if other rulings wait on this thread"
    );
}

#[test]
fn an_option_the_worker_gave_no_effect_keeps_the_phrase_it_always_had() {
    let asked = "Which flag?\n- `--dry-run` => Renames the flag everywhere\n- `--check`";

    let comment = rendered(&ruling(ticket(1), asking(asked)));

    assert!(
        comment.contains("| **A** | `--dry-run` | Renames the flag everywhere |"),
        "{comment}"
    );
    assert!(
        comment.contains("| **B** | `--check` | Sends the worker this answer |"),
        "{comment}"
    );
    assert!(!comment.contains("On the board"), "{comment}");
}

#[test]
fn the_effect_beside_an_option_is_no_part_of_the_answer_it_gives() {
    let kind = asking("Which flag?\n- `--dry-run` => Renames the flag\n- `--check` => Keeps it");

    assert_eq!(
        answer_for(&kind, 'A', ""),
        Some(Answer::Text("--dry-run".into()))
    );
    assert_eq!(named(&kind, 'B').as_deref(), Some("`--check`"));
    assert_eq!(chosen(&kind, &Answer::Text("--check".into())), Some('B'));
}

#[test]
fn a_ruling_kelpie_wrote_effects_for_shows_them_in_place_of_the_fixed_phrases() {
    let mut merge = ruling(ticket(1), merge());
    merge.effects = vec![
        "Merges it into main. CI passed and 2 reviewers read it. 3 work items wait for a slot."
            .into(),
        "Sends the worker back. Another round costs about $0.40.".into(),
    ];

    let comment = rendered(&merge);

    assert!(
        comment.contains(
            "| 👍 or `71 yes` | Merges it into main. CI passed and 2 reviewers read it. \
             3 work items wait for a slot. |"
        ),
        "{comment}"
    );
    assert!(
        comment.contains(
            "| 👎 or `71 no <note>` | Sends the worker back. Another round costs about $0.40. |"
        ),
        "{comment}"
    );
    assert!(
        comment.contains(
            "| `71 rework <note>` | Sends the worker back for a change the whole review \
             reads again |"
        ),
        "{comment}"
    );
}

#[test]
fn the_card_for_a_failing_test_says_what_a_yes_does() {
    let kind = RulingKind::Stuck(Stuck::TestStillFails {
        fix: crate::state::Fix::Review(crate::work_item::Review::first()),
        head: "6b0606e9b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6".into(),
        tests: vec![crate::state::FailingTest {
            test: "tests/flag.rs".into(),
            output: "assertion failed: flag".into(),
        }],
        prompt: "fix it".into(),
    });

    let card = rendered(&ruling(ticket(1), kind));

    assert!(
        card.contains("A reviewer's test still fails after two fixes. Send the worker back?"),
        "{card}"
    );
    assert!(
        card.contains("Sends the worker one more fix turn on the same tests"),
        "{card}"
    );
    assert!(
        card.contains("Sends the worker your note, ends the check and the review goes on"),
        "{card}"
    );
    assert!(!card.contains("assertion failed"), "{card}");
}

#[test]
fn a_contribution_ruling_names_each_line_and_offers_a_yes_that_trusts_and_a_no_that_does_not() {
    let kind = RulingKind::Contribution(crate::state::Held::Security {
        head: "6b0606e9b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6".into(),
        lines: vec![Finding {
            severity: Severity::High,
            file: "build.rs".into(),
            line: 3,
            what: "downloads and runs a script".into(),
            why: "it runs at build time".into(),
            test: None,
        }],
    });
    let card = rendered(&ruling(ticket(71), kind.clone()));

    assert!(
        card.contains("Trust this contribution at 6b0606e"),
        "{card}"
    );
    assert!(
        card.contains("- build.rs:3 downloads and runs a script: it runs at build time"),
        "{card}"
    );
    assert_eq!(phrases(&kind).len(), 2);
    assert!(phrases(&kind)[0].starts_with("Trusts the head"));
    assert!(
        !super::super::said(&kind).contains("build.rs"),
        "nothing found is posted"
    );
}

#[test]
fn a_no_edits_ruling_card_offers_leave_it_open_or_close_and_names_each_cause() {
    let kind = RulingKind::Contribution(crate::state::Held::NoEdits);
    let card = rendered(&ruling(ticket(72), kind));

    assert!(card.contains("Leave it open"), "{card}");
    assert!(card.contains("Close with thanks"), "{card}");
    assert!(card.contains("has been deleted"), "{card}");
    assert!(card.contains("belongs to an organization"), "{card}");
    assert!(!card.contains("  "), "no run of spaces: {card}");
}

#[test]
fn a_contribution_merge_card_offers_no_rework_when_the_fork_cannot_be_pushed_to() {
    let merge = |rework_off| {
        let kind = RulingKind::Contribution(crate::state::Held::Merge {
            head: "2a8f1e6".into(),
            rework_off,
        });
        rendered(&ruling(ticket(72), kind))
    };

    assert!(
        merge(false).contains("`71 rework <note>`"),
        "{}",
        merge(false)
    );
    let card = merge(true);
    assert!(!card.contains("rework"), "{card}");
    assert!(card.contains("👍 or `71 yes`"), "{card}");
    assert!(card.contains("👎 or `71 no <note>`"), "{card}");
}

#[test]
fn a_merge_ruling_lists_the_open_threads_above_a_nit_with_their_levels_and_places() {
    let found = vec![
        cubic(
            "P1",
            Severity::High,
            ".github/workflows/dog-release.yml",
            102,
            "A build script can swap the gh that runs next.",
        ),
        cubic(
            "P3",
            Severity::Low,
            "scripts/test_dog_index_update.py",
            198,
            "The test name claims more than it checks.",
        ),
    ];
    let comment = rendered(&ruling(
        ticket(1),
        merge_with(found, Some("1 nit left open (cubic)")),
    ));

    assert!(
        comment.contains("cubic left 1 P1 and 1 P3 threads open"),
        "{comment}"
    );
    assert!(
        comment.contains("P1 `.github/workflows/dog-release.yml:102`"),
        "{comment}"
    );
    assert!(
        comment.contains("A build script can swap the gh that runs next."),
        "{comment}"
    );
    assert!(
        comment.contains("P3 `scripts/test_dog_index_update.py:198`"),
        "{comment}"
    );
    assert!(!comment.contains("never hold a merge"), "{comment}");
}

#[test]
fn a_merge_ruling_with_only_open_nits_keeps_its_wording_with_the_count() {
    let kind = merge_with(Vec::new(), Some("2 nits left open (cubic)"));
    let comment = rendered(&ruling(ticket(1), kind));

    assert!(
        comment.contains(
            "Review bots left nits on it, which never hold a merge: 2 nits left open (cubic)."
        ),
        "{comment}"
    );
    assert!(!comment.contains("threads open"), "{comment}");
}

#[test]
fn an_open_high_thread_makes_the_merge_ruling_lead_with_it_and_recommend_send_back() {
    let found = vec![
        cubic(
            "P2",
            Severity::Medium,
            "src/a.rs",
            4,
            "A rerun leaves stale assets.",
        ),
        cubic(
            "P1",
            Severity::High,
            "src/b.rs",
            9,
            "The token is shared with the build.",
        ),
    ];
    let comment = rendered(&ruling(ticket(1), merge_with(found, None)));

    let lead = comment.find("high").expect("a high thread leads");
    assert!(lead < comment.find("cubic left").unwrap(), "{comment}");
    assert!(comment.contains("Kelpie recommends Send back"), "{comment}");

    let calm = vec![cubic(
        "P2",
        Severity::Medium,
        "src/a.rs",
        4,
        "A rerun leaves stale assets.",
    )];
    let comment = rendered(&ruling(ticket(1), merge_with(calm, None)));
    assert!(!comment.contains("recommends"), "{comment}");
}
