use std::path::Path;

use super::*;

const FORMS: &str = "It takes `shep kelpie rule 14 yes` or `shep kelpie rule 14 no <note>`";
use crate::state::{ProjectState, Resume, RulingKind, StateStore, Stuck};

fn yes_or_no(id: u64) -> Ruling {
    Ruling {
        id,
        issue: Some(7),
        question: format!("Merge pull request #71? `shep kelpie rule {id} yes` merges it"),
        pull_request: Some(71),
        kind: RulingKind::from(Stuck::Closed),
        alerted: true,
    }
}

fn question(id: u64) -> Ruling {
    let asked = "Should it be `--dry-run` or `--check`?".to_owned();
    Ruling {
        question: format!("The worker on issue #9 asks:\n\n{asked}"),
        kind: RulingKind::Question {
            asked,
            resume: Resume::Nothing,
        },
        ..yes_or_no(id)
    }
}

// Project `name`'s state file under kelpie's home, holding `rulings`.
fn waiting(home: &Path, name: &str, rulings: Vec<Ruling>) {
    let mut state = ProjectState::new();
    state.last_ruling = rulings.iter().map(|r| r.id).max().unwrap_or(0);
    state.rulings = rulings;
    let path = home.join(name).join("state.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    StateStore::new(path).save(&state).unwrap();
}

fn project(name: &str) -> ProjectName {
    ProjectName::try_from(name).unwrap()
}

// Kelpie's home with ruling 14 waiting on koji and question 15 on rotom.
fn two_projects() -> (tempfile::TempDir, RulingIds) {
    let home = tempfile::tempdir().unwrap();
    waiting(home.path(), "koji", vec![yes_or_no(14)]);
    waiting(home.path(), "rotom", vec![question(15)]);
    let ids = RulingIds::under(home.path());
    (home, ids)
}

#[test]
fn a_merge_ruling_takes_a_rework_and_another_ruling_refuses_one() {
    let home = tempfile::tempdir().unwrap();
    let merge = Ruling {
        kind: RulingKind::Merge {
            head: "c0ffee".into(),
            unreviewed: None,
            open_threads: None,
            unread_head: false,
            note_fix: false,
            late_fix: false,
            nits: None,
        },
        ..yes_or_no(16)
    };
    waiting(home.path(), "koji", vec![yes_or_no(14), merge]);
    let ids = RulingIds::under(home.path());
    let answer = |args: &[&str]| prepare(&ids, None, args, None);
    assert_eq!(
        answer(&["16", "rework", "split", "the", "parser"]),
        Ok((project("koji"), "16 rework split the parser".into()))
    );
    assert_eq!(
        answer(&["16", "maybe"]),
        Err(
            "ruling 16 was not answered: it takes `yes`, `no <note>` or `rework <note>`. \
             It takes `shep kelpie rule 16 yes`, `shep kelpie rule 16 no <note>` or \
             `shep kelpie rule 16 rework <note>`"
                .into()
        )
    );
    assert_eq!(
        answer(&["14", "rework", "split", "it"]),
        Err(format!(
            "ruling 14 was not answered: it takes `yes`, or `no <note>`. {FORMS}"
        ))
    );
}

#[test]
fn each_answer_form_reads_right_for_its_ruling_s_kind() {
    let (_home, ids) = two_projects();
    let answer = |args: &[&str]| prepare(&ids, None, args, None);
    assert_eq!(
        answer(&["14", "yes"]),
        Ok((project("koji"), "14 yes".into()))
    );
    assert_eq!(
        answer(&["14", "no", "rename", "the", "flag"]),
        Ok((project("koji"), "14 no rename the flag".into()))
    );
    assert_eq!(
        answer(&["14", "no it's the wrong flag"]),
        Ok((project("koji"), "14 no it's the wrong flag".into()))
    );
    assert_eq!(
        answer(&["15", "use", "--dry-run"]),
        Ok((project("rotom"), "15 answer use --dry-run".into()))
    );
    assert_eq!(
        answer(&["14", "use", "--dry-run"]),
        Err(format!(
            "ruling 14 was not answered: it takes `yes`, or `no <note>`. {FORMS}"
        ))
    );
    assert_eq!(
        answer(&["14", "no"]),
        Err(format!(
            "ruling 14 was not answered: a no takes a note for the worker: `no <note>`. {FORMS}"
        ))
    );
    assert_eq!(
        answer(&["14"]),
        Err("ruling 14 takes `shep kelpie rule 14 yes` or `shep kelpie rule 14 no <note>`".into())
    );
}

#[test]
fn a_yes_to_a_ruling_that_asks_for_text_is_text() {
    let (_home, ids) = two_projects();
    let sent = prepare(&ids, None, &["15", "yes"], None);
    assert_eq!(sent, Ok((project("rotom"), "15 answer yes".into())));
}

#[test]
fn an_id_waiting_nowhere_or_named_badly_sends_nothing() {
    let (_home, ids) = two_projects();
    let err = prepare(&ids, None, &["16", "yes"], None).unwrap_err();
    assert_eq!(
        err,
        "no ruling 16 is waiting: `shep kelpie rule` lists those that are"
    );
    let err = prepare(&ids, None, &["#14", "yes"], None).unwrap_err();
    assert!(err.starts_with("\"#14\" is not a ruling's id"), "{err}");
    let err = prepare(&ids, Some(&project("rotom")), &["14", "yes"], None).unwrap_err();
    assert!(err.starts_with("no ruling 14 is waiting"), "{err}");
}

// Ids given before they were unique can still be open on two projects.
#[test]
fn an_id_open_on_two_projects_needs_the_project() {
    let home = tempfile::tempdir().unwrap();
    waiting(home.path(), "koji", vec![yes_or_no(3)]);
    waiting(home.path(), "rotom", vec![yes_or_no(3)]);
    let ids = RulingIds::under(home.path());
    let err = prepare(&ids, None, &["3", "yes"], None).unwrap_err();
    assert_eq!(
        err,
        "ruling 3 is waiting on koji and rotom, so name one: \
         `shep kelpie rule -p <project> 3 <answer>`"
    );
    let sent = prepare(&ids, Some(&project("rotom")), &["3", "yes"], None);
    assert_eq!(sent, Ok((project("rotom"), "3 yes".into())));
}

// A claimed id names its project, even when an older one reuses the number.
#[test]
fn a_claimed_id_goes_to_the_project_it_was_given_to() {
    let home = tempfile::tempdir().unwrap();
    let ids = RulingIds::under(home.path());
    assert_eq!(ids.claim("rotom", 0), 1);
    waiting(home.path(), "koji", vec![yes_or_no(1)]);
    waiting(home.path(), "rotom", vec![yes_or_no(1)]);
    let sent = prepare(&ids, None, &["1", "yes"], None);
    assert_eq!(sent, Ok((project("rotom"), "1 yes".into())));
}

#[test]
fn with_no_terminal_rule_lists_the_rulings_and_fails() {
    let (_home, ids) = two_projects();
    let err = prepare(&ids, None, &[], None).unwrap_err();
    assert_eq!(
        err,
        "Ruling 14 on koji:\n    Merge pull request #71? `shep kelpie rule 14 yes` merges it\n\n\
         Ruling 15 on rotom:\n    The worker on issue #9 asks:\n\n    \
         Should it be `--dry-run` or `--check`?\n\n\
         `shep kelpie rule <id> <answer>` answers one"
    );
    let empty = tempfile::tempdir().unwrap();
    let none = prepare(&RulingIds::under(empty.path()), None, &[], None);
    assert_eq!(none, Err("no rulings are waiting".into()));
}

#[test]
fn the_picker_answers_the_ruling_it_showed() {
    let (_home, ids) = two_projects();
    let mut input = "15\nuse --dry-run\n".as_bytes();
    let mut output = Vec::new();
    let ask = Some((
        &mut input as &mut dyn BufRead,
        &mut output as &mut dyn Write,
    ));
    let sent = prepare(&ids, None, &[], ask);
    assert_eq!(
        sent,
        Ok((project("rotom"), "15 answer use --dry-run".into()))
    );
    let shown = String::from_utf8(output).unwrap();
    assert!(shown.starts_with("Ruling 14 on koji:\n"), "{shown}");
    assert!(shown.contains("Ruling 15 on rotom:\n"), "{shown}");
    assert!(shown.ends_with("Which ruling? Your answer: "), "{shown}");
}

#[test]
fn the_picker_asks_again_until_the_answer_fits() {
    let (_home, ids) = two_projects();
    let mut input = "#14\n16\n14\nmaybe\nno rename it\n".as_bytes();
    let mut output = Vec::new();
    let ask = Some((
        &mut input as &mut dyn BufRead,
        &mut output as &mut dyn Write,
    ));
    let sent = prepare(&ids, Some(&project("koji")), &[], ask);
    assert_eq!(sent, Ok((project("koji"), "14 no rename it".into())));
    let shown = String::from_utf8(output).unwrap();
    assert!(!shown.contains("rotom"), "{shown}");
    assert!(
        shown.contains("Which ruling? [14] Type the id of one listed.\n"),
        "{shown}"
    );
    assert!(shown.contains("No ruling 16 is listed.\n"), "{shown}");
    assert!(
        shown.contains(&format!(
            "ruling 14 was not answered: it takes `yes`, or `no <note>`. {FORMS}\n"
        )),
        "{shown}"
    );
}

#[test]
fn the_picker_sends_nothing_on_an_empty_answer_or_the_input_s_end() {
    let (_home, ids) = two_projects();
    for typed in ["14\n\n", "14\n", ""] {
        let mut input = typed.as_bytes();
        let mut output = Vec::new();
        let ask = Some((
            &mut input as &mut dyn BufRead,
            &mut output as &mut dyn Write,
        ));
        let sent = prepare(&ids, None, &[], ask);
        assert_eq!(sent, Err("nothing was sent".into()), "{typed:?}");
    }
}

// Probed in review: with lab's state unreadable, an unclaimed 3 open on
// koji could be lab's too, so a yes to it must name the project.
#[test]
fn an_unclaimed_id_with_a_state_file_unread_needs_the_project() {
    let (home, ids) = two_projects();
    let broken = home.path().join("lab/state.json");
    std::fs::create_dir_all(broken.parent().unwrap()).unwrap();
    std::fs::write(&broken, "{").unwrap();
    let err = prepare(&ids, None, &["14", "yes"], None).unwrap_err();
    assert!(
        err.starts_with(
            "ruling 14 does not say which project it is on, and lab's rulings cannot be read, \
             so name the project: `shep kelpie rule -p <project> 14 <answer>`\n\
             lab's rulings cannot be read: "
        ),
        "{err}"
    );
    let sent = prepare(&ids, Some(&project("koji")), &["14", "yes"], None);
    assert_eq!(sent, Ok((project("koji"), "14 yes".into())));

    // A claimed id names its project, whatever cannot be read.
    assert_eq!(ids.claim("koji", 15), 16);
    waiting(home.path(), "koji", vec![yes_or_no(14), yes_or_no(16)]);
    let sent = prepare(&ids, None, &["16", "yes"], None);
    assert_eq!(sent, Ok((project("koji"), "16 yes".into())));
}

#[test]
fn the_picker_takes_the_answer_on_the_id_s_line() {
    let (_home, ids) = two_projects();
    for (typed, sent) in [
        ("14 no rename it\n", "14 no rename it"),
        ("14 koji\nyes\n", "14 yes"),
        ("14 maybe\nyes\n", "14 yes"),
    ] {
        let mut input = typed.as_bytes();
        let mut output = Vec::new();
        let ask = Some((
            &mut input as &mut dyn BufRead,
            &mut output as &mut dyn Write,
        ));
        let answered = prepare(&ids, None, &[], ask);
        assert_eq!(answered, Ok((project("koji"), sent.into())), "{typed:?}");
    }
}
