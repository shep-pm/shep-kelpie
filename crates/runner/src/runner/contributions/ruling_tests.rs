//! The ruling a flagged head raises, through the runner's stand-ins

use std::sync::Mutex;

use serde_json::{Value, json};

use super::security_tests::{commit_on, contribute, contribution, reviews};
use super::tests::{picking, poll, posts};
use crate::ports::{CLEAN, PullRequestState, Role};
use crate::runner::{Runner, StepReport, step};
use crate::test::ticket;
use crate::test::{Rig, Scripted};

// What the security reviewer finds in a head it flags
const FOUND: &str = "HIGH|build.rs:3|downloads and runs a script|it runs at build time\n\
                     LOW|.github/workflows/ci.yml:0|a new workflow|it runs on every push";

fn write(folder: &std::path::Path, file: &str, text: &str) {
    std::fs::write(folder.join(file), text).unwrap();
}

// A pull request whose head the reviewer flags: its number is 30
pub(super) fn flagged() -> (Rig, Mutex<Runner>, String) {
    let (rig, runner) = picking("all");
    rig.claude.script([Scripted::Text(FOUND)]);
    let head = contribute(&rig, 30, |h| write(h, "build.rs", "fn main() {}\n"));
    assert_eq!(step(&runner).unwrap(), None);
    // The ruling reaches the webhook, and nothing else.
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(30) })
    );
    (rig, runner, head)
}

fn rulings(rig: &Rig, runner: &Mutex<Runner>) -> Vec<Value> {
    let status = rig.ask(runner, "status", None);
    status["rulings"].as_array().cloned().unwrap_or_default()
}

pub(super) fn rule(rig: &Rig, runner: &Mutex<Runner>, words: &str) -> Value {
    rig.ask(runner, "rule", Some(words))
}

#[test]
fn a_flagged_head_raises_one_ruling_naming_each_file_line_and_why() {
    let (rig, runner, head) = flagged();

    let waiting = rulings(&rig, &runner);
    assert_eq!(waiting.len(), 1);
    let ruling = &waiting[0];
    assert_eq!(
        (ruling["id"].clone(), ruling["issue"].clone()),
        ("30".into(), Value::Null)
    );
    assert_eq!(ruling["pull_request"], 30);
    assert_eq!(ruling["kind"]["kind"], "contribution");
    assert_eq!(ruling["kind"]["reason"], "security");
    assert_eq!(ruling["kind"]["head"], head);
    assert_eq!(ruling["kind"]["lines"].as_array().unwrap().len(), 2);
    let question = ruling["question"].as_str().unwrap();
    for named in [
        "build.rs:3",
        "downloads and runs a script",
        "it runs at build time",
        ".github/workflows/ci.yml a new workflow",
        "it runs on every push",
        &format!("shep kelpie rule 30 yes {}", &head[..7]),
    ] {
        assert!(question.contains(named), "{named}: {question}");
    }

    // Once per flagged head: later polls neither read it again nor raise another.
    poll(&rig, &runner);
    poll(&rig, &runner);
    assert_eq!(rulings(&rig, &runner).len(), 1);
    assert_eq!(reviews(&rig).len(), 1);
}

#[test]
fn a_clean_head_raises_no_ruling() {
    let (rig, runner) = picking("all");
    rig.claude.script([Scripted::Text(CLEAN)]);
    contribute(&rig, 30, |h| write(h, "one.txt", "one\n"));

    assert_eq!(step(&runner).unwrap(), None);
    poll(&rig, &runner);

    assert_eq!(rulings(&rig, &runner), Vec::<Value>::new());
    assert!(rig.alerts.posts().is_empty());
}

#[test]
fn the_ruling_is_not_posted_on_the_contributors_pull_request() {
    let (rig, runner, _) = flagged();
    for _ in 0..3 {
        poll(&rig, &runner);
    }

    assert_eq!(rulings(&rig, &runner).len(), 1);
    assert_eq!(posts(&rig), Vec::<String>::new(), "no App write");
    assert_eq!(rig.forge.comments(), []);
    assert!(rig.github.comments().is_empty());
    // It reaches the maintainer's webhook instead.
    let alerted = rig.alerts.posts();
    assert_eq!(alerted.len(), 1, "{alerted:?}");
    assert!(
        alerted[0].1.title.contains("ruling #30"),
        "{:?}",
        alerted[0].1
    );
    assert!(
        alerted[0].1.text.contains("build.rs:3"),
        "{:?}",
        alerted[0].1
    );
}

#[test]
fn a_yes_trusts_the_head_and_it_counts_as_passed() {
    let (rig, runner, head) = flagged();
    assert_eq!(contribution(&rig, &runner).get("passed"), None);

    let answered = rule(&rig, &runner, &format!("30 yes {}", &head[..7]));

    assert_eq!(answered.get("error"), None, "{answered}");
    assert_eq!(rulings(&rig, &runner), Vec::<Value>::new());
    assert_eq!(contribution(&rig, &runner)["passed"], head);
    // Trusted is passed: it is the head the next is read since, and is not read again.
    poll(&rig, &runner);
    assert_eq!(reviews(&rig).len(), 1);
    assert_eq!(posts(&rig), Vec::<String>::new());
}

#[test]
fn a_no_leaves_the_head_unpassed_and_posts_nothing() {
    let (rig, runner, _) = flagged();

    let answered = rule(&rig, &runner, "30 no not this one");

    assert_eq!(answered.get("error"), None, "{answered}");

    assert_eq!(rulings(&rig, &runner), Vec::<Value>::new());
    let held = contribution(&rig, &runner);
    assert_eq!(held.get("passed"), None);
    assert_eq!(held["security"]["head"], held["head"]);
    poll(&rig, &runner);
    poll(&rig, &runner);
    assert_eq!(
        rulings(&rig, &runner),
        Vec::<Value>::new(),
        "not asked again"
    );
    assert_eq!(reviews(&rig).len(), 1, "nothing ran on it");
    assert_eq!(posts(&rig), Vec::<String>::new());
    assert_eq!(rig.forge.comments(), []);
}

#[test]
fn a_push_withdraws_the_old_heads_ruling_and_a_late_yes_to_it_is_refused_as_stale() {
    let (rig, runner, old) = flagged();
    // The new head's read gives no verdict, so it raises nothing of its own.
    rig.claude.script([Scripted::Text("I could not tell.")]);
    let new = commit_on(&rig, 30, "topic/30", |h| write(h, "two.txt", "two\n"));
    assert_ne!(old, new);

    poll(&rig, &runner);

    assert_eq!(rulings(&rig, &runner), Vec::<Value>::new(), "withdrawn");
    assert_eq!(
        rule(&rig, &runner, &format!("30 yes {}", &old[..7])),
        json!({ "error": "ruling 30 is already answered, or was never raised" })
    );
    let held = contribution(&rig, &runner);
    assert_eq!(held["head"], new);
    assert_eq!(held.get("passed"), None, "neither head is trusted");
    assert_eq!(posts(&rig), Vec::<String>::new());
}

#[test]
fn a_yes_to_an_old_head_does_not_trust_the_new_one() {
    let (rig, runner) = picking("all");
    let first = contribute(&rig, 30, |h| write(h, "one.txt", "one\n"));
    rig.claude.script([Scripted::Text(CLEAN)]);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(contribution(&rig, &runner)["passed"], first);
    // The second head is flagged and waits, then a third replaces it.
    rig.claude.script([Scripted::Text(FOUND)]);
    let second = commit_on(&rig, 30, "topic/30", |h| {
        write(h, "build.rs", "fn main() {}\n")
    });
    poll(&rig, &runner);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(30) })
    );
    assert_eq!(rulings(&rig, &runner)[0]["kind"]["head"], second);
    rig.claude.script([Scripted::Text("I could not tell.")]);
    let third = commit_on(&rig, 30, "topic/30", |h| write(h, "three.txt", "three\n"));
    poll(&rig, &runner);

    let late = rule(&rig, &runner, &format!("30 yes {}", &second[..7]));

    assert!(late["error"].is_string(), "{late}");
    let held = contribution(&rig, &runner);
    assert_eq!(held["head"], third);
    assert_eq!(
        held["passed"], first,
        "neither the old flagged head nor the new one"
    );
}

#[test]
fn a_yes_naming_the_old_flagged_head_does_not_trust_the_new_flagged_one() {
    let (rig, runner, old) = flagged();
    rig.claude.script([Scripted::Text(FOUND)]);
    let new = commit_on(&rig, 30, "topic/30", |h| {
        write(h, "two.rs", "fn two() {}\n")
    });
    poll(&rig, &runner);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(30) })
    );
    let waiting = rulings(&rig, &runner);
    assert_eq!(waiting.len(), 1, "the old head's ruling is withdrawn");
    assert_eq!(waiting[0]["kind"]["head"], new);

    let late = rule(&rig, &runner, &format!("30 yes {}", &old[..7]));

    assert!(
        late["error"].as_str().unwrap().contains(&new[..7]),
        "{late}"
    );
    assert_eq!(rulings(&rig, &runner).len(), 1, "the new head still waits");
    assert_eq!(contribution(&rig, &runner).get("passed"), None);
    let right = rule(&rig, &runner, &format!("30 yes {}", &new[..7]));
    assert_eq!(right.get("error"), None, "{right}");
    assert_eq!(contribution(&rig, &runner)["passed"], new);
}

#[test]
fn a_yes_that_names_no_head_trusts_nothing() {
    let (rig, runner, head) = flagged();

    for words in ["30 yes", "30 yes abc", "30 yes 6b0606e0000"] {
        let refused = rule(&rig, &runner, words);
        assert!(refused["error"].is_string(), "{words}: {refused}");
    }

    assert_eq!(rulings(&rig, &runner).len(), 1);
    assert_eq!(contribution(&rig, &runner).get("passed"), None);
    let upper = rule(
        &rig,
        &runner,
        &format!("30 yes {}", head[..7].to_uppercase()),
    );
    assert_eq!(upper.get("error"), None, "{upper}");
    assert_eq!(contribution(&rig, &runner)["passed"], head);
}

#[test]
fn the_project_manager_is_not_asked_to_answer_it() {
    let (rig, runner) = picking("all");
    rig.edit_settings(|s| {
        let listed = "[app.dogs.kelpie.agents]\n";
        assert!(s.contains(listed), "the example's agents table moved");
        s.replace(listed, &format!("{listed}pm = \"pm\"\n"))
    });
    drop(runner);
    let runner = rig.open().unwrap();
    rig.claude.script([Scripted::Text(FOUND)]);
    contribute(&rig, 30, |h| write(h, "build.rs", "fn main() {}\n"));

    for _ in 0..4 {
        rig.next_look();
        step(&runner).unwrap();
    }

    let waiting = rulings(&rig, &runner);
    assert_eq!(waiting.len(), 1, "the maintainer is asked");
    let asked: Vec<_> = (rig.claude.all_seen().into_iter())
        .filter(|s| s.call.role == Role::Pm && s.call.prompt.contains("waits on your answer"))
        .collect();
    assert_eq!(asked.len(), 0, "the project manager was not asked");
    assert_eq!(
        rig.alerts.posts().len(),
        1,
        "it reaches the webhook at once"
    );
}

#[test]
fn pm_alone_naming_security_is_refused_at_load() {
    let rig = Rig::new("security-refused");
    rig.edit_settings(|s| format!("{s}\n[app.dogs.kelpie.rulings]\npm_alone = [\"security\"]\n"));

    let err = match rig.open() {
        Err(e) => e.to_string(),
        Ok(_) => panic!("the runner opened"),
    };

    assert!(err.contains("rulings.pm_alone"), "{err}");
    assert!(err.contains("security"), "{err}");
}

#[test]
fn a_closed_pull_request_withdraws_its_ruling_silently() {
    for state in [PullRequestState::Closed, PullRequestState::Merged] {
        let (rig, runner, _) = flagged();
        assert_eq!(rulings(&rig, &runner).len(), 1);

        rig.forge.set_state(30, state);
        poll(&rig, &runner);

        assert_eq!(rulings(&rig, &runner), Vec::<Value>::new(), "{state:?}");
        assert_eq!(posts(&rig), Vec::<String>::new(), "{state:?}");
        assert_eq!(rig.forge.comments(), [], "{state:?}");
    }
}
