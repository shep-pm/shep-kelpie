use std::path::Path;

use super::*;

use crate::test::ticket;

const FORMS: &str = "It takes `shep kelpie rule 14 yes` or `shep kelpie rule 14 no <note>`";
const FORMS_427: &str = "It takes `shep kelpie rule 427 yes` or `shep kelpie rule 427 no <note>`";
use crate::state::{ProjectState, Resume, RulingKind, STATE_FILE, StateStore, Stuck};

// A ruling on issue `id`, which is its number.
fn yes_or_no(id: TicketId) -> Ruling {
    Ruling {
        issue: Some(id),
        alerted: true,
        ..Ruling::new(
            id,
            format!("Merge pull request #{id}? `shep kelpie rule {id} yes` merges it"),
            RulingKind::from(Stuck::Closed),
        )
    }
}

fn question(id: TicketId) -> Ruling {
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
    state.rulings = rulings;
    let path = home.join(name).join(STATE_FILE);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    StateStore::new(path).save(&state).unwrap();
}

fn project(name: &str) -> ProjectName {
    ProjectName::try_from(name).unwrap()
}

// Kelpie's home with ruling 14 waiting on koji and question 15 on rotom.
fn two_projects() -> (tempfile::TempDir, Projects) {
    let home = tempfile::tempdir().unwrap();
    waiting(home.path(), "koji", vec![yes_or_no(ticket(14))]);
    waiting(home.path(), "rotom", vec![question(ticket(15))]);
    let projects = Projects::under(home.path());
    (home, projects)
}

#[test]
fn a_merge_ruling_takes_a_rework_and_another_ruling_refuses_one() {
    let home = tempfile::tempdir().unwrap();
    let merge = Ruling {
        kind: RulingKind::Merge {
            head: "c0ffee".into(),
            unreviewed: None,
            open_threads: None,
            held_by: Vec::new(),
            unread_head: false,
            note_fix: false,
            late_fix: false,
            nits: None,
            disputes: None,
            rivals: Vec::new(),
        },
        ..yes_or_no(ticket(16))
    };
    waiting(home.path(), "koji", vec![yes_or_no(ticket(14)), merge]);
    let projects = Projects::under(home.path());
    let answer = |args: &[&str]| prepare(&projects, None, args, None);
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
    let (_home, projects) = two_projects();
    let answer = |args: &[&str]| prepare(&projects, None, args, None);
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
    let (_home, projects) = two_projects();
    let sent = prepare(&projects, None, &["15", "yes"], None);
    assert_eq!(sent, Ok((project("rotom"), "15 answer yes".into())));
}

#[test]
fn a_number_waiting_nowhere_or_named_badly_sends_nothing() {
    let (_home, projects) = two_projects();
    let nowhere = |named: &str| {
        format!(
            "no ruling on {named} is waiting: a ruling is named by its pull request's number \
             or its ticket's id, and `shep kelpie rule` lists those that are waiting"
        )
    };
    let err = prepare(&projects, None, &["16", "yes"], None).unwrap_err();
    assert_eq!(err, nowhere("#16"));
    let err = prepare(&projects, None, &["#16", "yes"], None).unwrap_err();
    assert_eq!(err, nowhere("#16"));
    for bad in ["1x", "#", "#x", "koji#", "#0", "koji#1x", "a b#7", "a#b#7"] {
        let err = prepare(&projects, None, &[bad, "yes"], None).unwrap_err();
        assert!(
            err.starts_with(&format!("{bad:?} is not a ruling")),
            "{err}"
        );
    }
    let err = prepare(&projects, Some(&project("rotom")), &["14", "yes"], None).unwrap_err();
    assert!(err.starts_with("no ruling on #14 is waiting"), "{err}");
}

// Rulings were named by a counter before: koji's ruling on pull request 71
// was 40. The number is the name now, and nothing answers to the counter.
#[test]
fn a_counter_id_that_no_longer_exists_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let ruling = Ruling {
        issue: Some(ticket(7)),
        pull_request: Some(71),
        ..yes_or_no(ticket(71))
    };
    waiting(home.path(), "koji", vec![ruling]);
    let projects = Projects::under(home.path());
    for old in ["40", "#40", "koji#40"] {
        let err = prepare(&projects, None, &[old, "yes"], None).unwrap_err();
        assert!(err.starts_with("no ruling on "), "{err}");
        assert!(err.contains("40 is waiting"), "{err}");
    }
    let err = prepare(&projects, Some(&project("koji")), &["40", "yes"], None).unwrap_err();
    assert!(err.starts_with("no ruling on #40 is waiting"), "{err}");
    let sent = prepare(&projects, None, &["71", "yes"], None);
    assert_eq!(sent, Ok((project("koji"), "71 yes".into())));
}

// Koji's ruling parks issue 407 and is about pull request 427.
fn on_a_pull_request() -> (tempfile::TempDir, Projects) {
    let home = tempfile::tempdir().unwrap();
    let ruling = Ruling {
        issue: Some(ticket(407)),
        pull_request: Some(427),
        ..yes_or_no(ticket(427))
    };
    waiting(home.path(), "koji", vec![ruling]);
    waiting(home.path(), "rotom", vec![question(ticket(15))]);
    let projects = Projects::under(home.path());
    (home, projects)
}

#[test]
fn a_ruling_s_issue_and_its_pull_request_name_the_same_ruling() {
    let (_home, projects) = on_a_pull_request();
    let koji = Ok((project("koji"), "427 yes".into()));
    for named in ["427", "407", "#427", "#407", "koji#427", "koji#407"] {
        let sent = prepare(&projects, None, &[named, "yes"], None);
        assert_eq!(sent, koji, "{named}");
    }
    let sent = prepare(
        &projects,
        Some(&project("koji")),
        &["427", "no", "wrong"],
        None,
    );
    assert_eq!(sent, Ok((project("koji"), "427 no wrong".into())));
}

// A file from before the counter went can hold two rulings on one pull
// request: the one parked on took the number, and the other kept its id.
#[test]
fn a_number_names_the_ruling_that_took_it_not_one_an_older_kelpie_left() {
    let home = tempfile::tempdir().unwrap();
    let on = |id: TicketId| Ruling {
        issue: Some(ticket(407)),
        pull_request: Some(427),
        ..yes_or_no(id)
    };
    waiting(home.path(), "koji", vec![on(ticket(41)), on(ticket(427))]);
    let projects = Projects::under(home.path());
    let sent = prepare(&projects, None, &["427", "yes"], None);
    assert_eq!(sent, Ok((project("koji"), "427 yes".into())));
    // Alone, the one left is still answered by its pull request.
    waiting(home.path(), "koji", vec![on(ticket(41))]);
    let sent = prepare(&projects, None, &["427", "yes"], None);
    assert_eq!(sent, Ok((project("koji"), "41 yes".into())));
}

#[test]
fn a_number_answers_by_the_ruling_s_kind_and_says_which_ruling_it_found() {
    let (_home, projects) = on_a_pull_request();
    let err = prepare(&projects, None, &["427", "maybe"], None).unwrap_err();
    assert_eq!(
        err,
        format!("ruling 427 was not answered: it takes `yes`, or `no <note>`. {FORMS_427}")
    );
    let err = prepare(&projects, None, &["427"], None).unwrap_err();
    assert_eq!(
        err,
        "ruling 427 takes `shep kelpie rule 427 yes` or `shep kelpie rule 427 no <note>`"
    );
}

// Two projects number their issues from 1, so each can have a ruling on #12.
fn both_on_12() -> (tempfile::TempDir, Projects) {
    let home = tempfile::tempdir().unwrap();
    waiting(home.path(), "shep-kelpie", vec![yes_or_no(ticket(12))]);
    waiting(home.path(), "rotom", vec![yes_or_no(ticket(12))]);
    let projects = Projects::under(home.path());
    (home, projects)
}

#[test]
fn a_number_on_two_projects_needs_the_project_and_names_both() {
    let (_home, projects) = both_on_12();
    for bare in ["12", "#12"] {
        let err = prepare(&projects, None, &[bare, "yes"], None).unwrap_err();
        assert_eq!(
            err,
            "#12 has a ruling on rotom and shep-kelpie, so name one: \
             `shep kelpie rule <project>#12 <answer>`"
        );
    }
    let sent = prepare(&projects, None, &["shep-kelpie#12", "yes"], None);
    assert_eq!(sent, Ok((project("shep-kelpie"), "12 yes".into())));
    let sent = prepare(&projects, Some(&project("rotom")), &["12", "yes"], None);
    assert_eq!(sent, Ok((project("rotom"), "12 yes".into())));
    let err = prepare(
        &projects,
        Some(&project("rotom")),
        &["shep-kelpie#12", "yes"],
        None,
    );
    assert_eq!(
        err.unwrap_err(),
        "`-p rotom` and `shep-kelpie#12` name different projects"
    );
}

#[test]
fn a_number_from_a_clone_is_the_clone_s_project_s_when_it_has_one() {
    let (_home, projects) = both_on_12();
    let here = project("rotom");
    let sent = prepare_in(&projects, None, Some(&here), &["12", "yes"], None);
    assert_eq!(sent, Ok((project("rotom"), "12 yes".into())));
    // A clone with no ruling on it leaves the number to the others.
    let (_home, projects) = on_a_pull_request();
    let sent = prepare_in(
        &projects,
        None,
        Some(&project("rotom")),
        &["427", "yes"],
        None,
    );
    assert_eq!(sent, Ok((project("koji"), "427 yes".into())));
}

#[test]
fn a_number_waiting_on_no_project_says_so() {
    let (_home, projects) = on_a_pull_request();
    let err = prepare(&projects, None, &["koji#9", "yes"], None).unwrap_err();
    assert_eq!(
        err,
        "no ruling on koji#9 is waiting: a ruling is named by its pull request's number or \
         its ticket's id, and `shep kelpie rule` lists those that are waiting"
    );
}

#[test]
fn with_no_terminal_rule_lists_the_rulings_and_fails() {
    let (_home, projects) = two_projects();
    let err = prepare(&projects, None, &[], None).unwrap_err();
    assert_eq!(
        err,
        "Ruling 14 on koji:\n    Merge pull request #14? `shep kelpie rule 14 yes` merges it\n\n\
         Ruling 15 on rotom:\n    The worker on issue #9 asks:\n\n    \
         Should it be `--dry-run` or `--check`?\n\n\
         `shep kelpie rule <ruling> <answer>` answers one"
    );
    let empty = tempfile::tempdir().unwrap();
    let none = prepare(&Projects::under(empty.path()), None, &[], None);
    assert_eq!(none, Err("no rulings are waiting".into()));
}

#[test]
fn the_picker_answers_the_ruling_it_showed() {
    let (_home, projects) = two_projects();
    let mut input = "15\nuse --dry-run\n".as_bytes();
    let mut output = Vec::new();
    let ask = Some((
        &mut input as &mut dyn BufRead,
        &mut output as &mut dyn Write,
    ));
    let sent = prepare(&projects, None, &[], ask);
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
    let (_home, projects) = two_projects();
    let mut input = "#14\n16\n14\nmaybe\nno rename it\n".as_bytes();
    let mut output = Vec::new();
    let ask = Some((
        &mut input as &mut dyn BufRead,
        &mut output as &mut dyn Write,
    ));
    let sent = prepare(&projects, Some(&project("koji")), &[], ask);
    assert_eq!(sent, Ok((project("koji"), "14 no rename it".into())));
    let shown = String::from_utf8(output).unwrap();
    assert!(!shown.contains("rotom"), "{shown}");
    assert!(
        shown.contains("Which ruling? [14] Type the number of one listed.\n"),
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
    let (_home, projects) = two_projects();
    for typed in ["14\n\n", "14\n", ""] {
        let mut input = typed.as_bytes();
        let mut output = Vec::new();
        let ask = Some((
            &mut input as &mut dyn BufRead,
            &mut output as &mut dyn Write,
        ));
        let sent = prepare(&projects, None, &[], ask);
        assert_eq!(sent, Err("nothing was sent".into()), "{typed:?}");
    }
}

// Probed in review: with lab's state unreadable, koji's one ruling on #13
// could share the number with a ruling of lab's.
#[test]
fn a_number_with_a_state_file_unread_needs_the_project() {
    let home = tempfile::tempdir().unwrap();
    let on_13 = Ruling {
        issue: Some(ticket(13)),
        pull_request: None,
        ..yes_or_no(ticket(13))
    };
    waiting(home.path(), "koji", vec![on_13]);
    let broken = home.path().join("lab/state.json");
    std::fs::create_dir_all(broken.parent().unwrap()).unwrap();
    std::fs::write(&broken, "{").unwrap();
    let projects = Projects::under(home.path());
    for number in ["13", "#13"] {
        let err = prepare(&projects, None, &[number, "yes"], None).unwrap_err();
        assert!(
            err.starts_with(
                "#13 has a ruling on koji, and lab's rulings cannot be read, \
                 so name the project: `shep kelpie rule <project>#13 <answer>`\n"
            ),
            "{err}"
        );
    }
    // A number that names nothing readable may still be lab's.
    let err = prepare(&projects, None, &["#99", "yes"], None).unwrap_err();
    assert!(
        err.starts_with("no ruling on #99 is waiting, and lab's rulings cannot be read"),
        "{err}"
    );
    let sent = prepare(&projects, None, &["koji#13", "yes"], None);
    assert_eq!(sent, Ok((project("koji"), "13 yes".into())));
    let sent = prepare(&projects, Some(&project("koji")), &["13", "yes"], None);
    assert_eq!(sent, Ok((project("koji"), "13 yes".into())));
    // A clone's own project answers first, whatever cannot be read.
    let sent = prepare_in(
        &projects,
        None,
        Some(&project("koji")),
        &["13", "yes"],
        None,
    );
    assert_eq!(sent, Ok((project("koji"), "13 yes".into())));
}

#[test]
fn the_picker_takes_the_answer_on_the_number_s_line() {
    let (_home, projects) = two_projects();
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
        let answered = prepare(&projects, None, &[], ask);
        assert_eq!(answered, Ok((project("koji"), sent.into())), "{typed:?}");
    }
}

// A project's own tickets are numbered apart from GitHub's, so `k12` is a
// ruling's id where the project keeps its tickets locally.
fn local(number: u64) -> TicketId {
    format!("k{number}").parse().unwrap()
}

fn on_a_local_ticket() -> Ruling {
    let id = local(12);
    Ruling {
        issue: Some(id),
        alerted: true,
        ..Ruling::new(
            id,
            format!("Should the branch merge? `shep kelpie rule {id} yes` merges it"),
            RulingKind::from(Stuck::Closed),
        )
    }
}

// A ruling raised before its pull request exists has no pull request yet, and
// a local id has no GitHub number of its own.
#[test]
fn a_local_ticket_answers_only_the_ruling_that_is_on_it() {
    let home = tempfile::tempdir().unwrap();
    waiting(home.path(), "koji", vec![yes_or_no(ticket(7))]);
    waiting(home.path(), "lab", vec![on_a_local_ticket()]);
    let projects = Projects::under(home.path());
    let sent = prepare(&projects, None, &["k12", "yes"], None);
    assert_eq!(sent, Ok((project("lab"), "k12 yes".into())));
    // A local id that names no ruling names nothing. It is not the ruling that
    // is waiting with no pull request yet.
    let err = prepare(&projects, None, &["k13", "yes"], None).unwrap_err();
    assert_eq!(
        err,
        "no ruling on k13 is waiting: a ruling is named by its pull request's number \
         or its ticket's id, and `shep kelpie rule` lists those that are waiting"
    );
    // A GitHub number still answers the ruling that has no pull request yet.
    let sent = prepare(&projects, None, &["7", "yes"], None);
    assert_eq!(sent, Ok((project("koji"), "7 yes".into())));
}

#[test]
fn a_local_ticket_two_projects_have_names_the_project_with_a_hash() {
    let home = tempfile::tempdir().unwrap();
    waiting(home.path(), "shep-kelpie", vec![on_a_local_ticket()]);
    waiting(home.path(), "rotom", vec![on_a_local_ticket()]);
    let projects = Projects::under(home.path());
    let err = prepare(&projects, None, &["k12", "yes"], None).unwrap_err();
    assert_eq!(
        err,
        "k12 has a ruling on rotom and shep-kelpie, so name one: \
         `shep kelpie rule <project>#k12 <answer>`"
    );
    // The command the message names is one `rule` reads.
    let sent = prepare(&projects, None, &["shep-kelpie#k12", "yes"], None);
    assert_eq!(sent, Ok((project("shep-kelpie"), "k12 yes".into())));
    let sent = prepare(&projects, Some(&project("rotom")), &["k12", "yes"], None);
    assert_eq!(sent, Ok((project("rotom"), "k12 yes".into())));
    let err = prepare(
        &projects,
        Some(&project("rotom")),
        &["shep-kelpie#k12", "yes"],
        None,
    );
    assert_eq!(
        err.unwrap_err(),
        "`-p rotom` and `shep-kelpie#k12` name different projects"
    );
}

#[test]
fn a_local_ticket_with_a_state_file_unread_names_the_project_with_a_hash() {
    let home = tempfile::tempdir().unwrap();
    waiting(home.path(), "koji", vec![on_a_local_ticket()]);
    let broken = home.path().join("lab/state.json");
    std::fs::create_dir_all(broken.parent().unwrap()).unwrap();
    std::fs::write(&broken, "{").unwrap();
    let projects = Projects::under(home.path());
    let err = prepare(&projects, None, &["k12", "yes"], None).unwrap_err();
    assert!(
        err.starts_with(
            "k12 has a ruling on koji, and lab's rulings cannot be read, \
             so name the project: `shep kelpie rule <project>#k12 <answer>`\n"
        ),
        "{err}"
    );
    let err = prepare(&projects, None, &["k99", "yes"], None).unwrap_err();
    assert!(
        err.starts_with("no ruling on k99 is waiting, and lab's rulings cannot be read"),
        "{err}"
    );
}
