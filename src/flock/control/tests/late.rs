//! The verbs that change a runner, against a runner busy past shep's trigger
//! deadline: it takes the action afterwards, so a timeout is no failure

use super::*;

// A runner that held `start` (finishing) before its busy step began
const HELD: &str = r#"{"project":"koji","run":"finishing","work_items":[]}"#;

// A runner that answered `status` and then never answered the action
fn busy(shepherd: &FakeShepherd, before: &[&str]) {
    let mut says: Vec<Option<&str>> = before.iter().copied().map(Some).collect();
    says.push(None);
    shepherd.replies("koji", &says);
}

// The one line a change prints when its runner answers late.
fn one(lines: Result<Vec<String>, String>) -> String {
    let lines = lines.expect("a late answer is no error to retry");
    let [line] = lines.as_slice() else {
        panic!("{lines:?}")
    };
    line.clone()
}

async fn changed(action: &str, params: Option<&str>, format: Format) -> (String, Vec<String>) {
    let mut shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    busy(&shepherd, &[]);
    let client = client(&shepherd).await;
    let ran = in_time(change(
        &reach(),
        &client,
        &project("koji"),
        action,
        params,
        format,
    ))
    .await;
    (one(ran), asked(&shepherd.writes()))
}

#[tokio::test]
async fn a_drop_the_runner_answers_late_has_most_likely_been_taken() {
    let (line, asked) = changed("drop", Some("713"), Format::Plain).await;
    assert!(line.contains("was busy"), "{line}");
    assert!(line.contains("most likely been taken"), "{line}");
    assert!(line.contains("`shep kelpie status -p koji`"), "{line}");
    assert_eq!(asked, ["drop"]);
}

#[tokio::test]
async fn a_late_gate_names_the_status_check() {
    let (line, _) = changed("gate", Some("7"), Format::Plain).await;
    assert!(line.contains("`shep kelpie status -p koji`"), "{line}");
}

#[tokio::test]
async fn a_late_add_names_the_status_check() {
    let (line, _) = changed("add", Some("7"), Format::Plain).await;
    assert!(line.contains("`shep kelpie status -p koji`"), "{line}");
}

#[tokio::test]
async fn a_late_adopt_names_the_status_check() {
    let (line, _) = changed("adopt", Some("40"), Format::Plain).await;
    assert!(line.contains("`shep kelpie status -p koji`"), "{line}");
}

#[tokio::test]
async fn a_late_rework_names_the_status_check() {
    let (line, _) = changed("rework", Some("40"), Format::Plain).await;
    assert!(line.contains("`shep kelpie status -p koji`"), "{line}");
}

#[tokio::test]
async fn a_late_drain_names_the_status_check() {
    let (line, _) = changed("drain", None, Format::Plain).await;
    assert!(line.contains("`shep kelpie status -p koji`"), "{line}");
}

#[tokio::test]
async fn a_late_undrain_names_the_status_check() {
    let (line, _) = changed("undrain", None, Format::Plain).await;
    assert!(line.contains("`shep kelpie status -p koji`"), "{line}");
}

#[tokio::test]
async fn a_late_board_move_names_the_board_check() {
    let (line, _) = changed("board", Some("top 7"), Format::Plain).await;
    assert!(line.contains("`shep kelpie board -p koji`"), "{line}");
}

#[tokio::test]
async fn a_late_change_prints_an_object_with_json() {
    let (line, _) = changed("drop", Some("713"), Format::Json).await;
    let object: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(object["sheep"], "koji");
    let message = object["message"].as_str().unwrap();
    assert!(message.contains("most likely been taken"), "{message}");
}

#[tokio::test]
async fn a_change_the_runner_refuses_is_still_its_error() {
    let shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    shepherd.says("koji", &[r#"{"error":"no work item for #713 is open"}"#]);
    let client = client(&shepherd).await;
    let ran = in_time(change(
        &reach(),
        &client,
        &project("koji"),
        "drop",
        Some("713"),
        Format::Plain,
    ))
    .await;
    assert_eq!(ran, Err("no work item for #713 is open".into()));
}

#[tokio::test]
async fn a_rule_the_runner_answers_late_names_the_rulings_check() {
    let mut shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    busy(&shepherd, &[]);
    let client = client(&shepherd).await;
    let answers = shepherd.scratch("answers");
    let ruled = in_time(rule(
        &reach(),
        &client,
        &project("koji"),
        "14 yes",
        &answers,
    ))
    .await;
    let lines = ruled.expect("a late ruling is no error to retry");
    assert_eq!(lines[0], "ruling 14 on koji: yes");
    assert!(lines[1].contains("most likely been taken"), "{lines:?}");
    assert!(lines[1].contains("`shep kelpie rule -p koji`"), "{lines:?}");
    assert_eq!(asked(&shepherd.writes()), ["rule"]);
}

#[tokio::test]
async fn a_finish_the_runner_answers_late_names_the_status_check() {
    let mut shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    busy(&shepherd, &[HELD]);
    let client = client(&shepherd).await;
    let ran = in_time(finish(
        &client,
        &project("koji"),
        Format::Plain,
        FAST,
        &mut |_| {},
    ))
    .await;
    let line = one(ran);
    assert!(line.contains("most likely been taken"), "{line}");
    assert!(line.contains("`shep kelpie status -p koji`"), "{line}");
    assert_eq!(asked(&shepherd.writes()), ["status", "finish"]);
}

#[tokio::test]
async fn a_start_the_runner_answers_late_names_the_status_check() {
    let mut shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    shepherd.holds_dog("kelpie", true);
    busy(&shepherd, &[HELD]);
    let client = client(&shepherd).await;
    let line = one(in_time(start(&client, &project("koji"))).await);
    assert!(line.contains("most likely been taken"), "{line}");
    assert!(line.contains("`shep kelpie status -p koji`"), "{line}");
    assert_eq!(asked(&shepherd.writes()), ["status", "start"]);
}

#[tokio::test]
async fn status_of_a_project_that_does_not_answer_in_time_is_still_an_error() {
    let shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    busy(&shepherd, &[]);
    let client = client(&shepherd).await;
    let err = in_time(send(&reach(), &client, &project("koji"), "status", None)).await;
    assert_eq!(err, Err("koji's runner did not answer in time".into()));
}
