//! `finish`, which lets the open work items end and picks nothing new, and
//! `start`, `pause` and `status` on a runner finishing

use super::*;

// A runner finishing with #7 and #9 still open
const FINISHING: &str = r#"{"project":"koji","run":"finishing",
    "work_items":[{"issue":7},{"issue":9}]}"#;

// `finish` against the fake shepherd, and what it said while it waited
async fn finished(shepherd: &FakeShepherd) -> (Result<Vec<String>, String>, Vec<String>) {
    let client = client(shepherd).await;
    let mut said = Vec::new();
    let say = &mut |line| said.push(line);
    let ran = in_time(finish(&client, &project("koji"), FAST, say)).await;
    (ran, said)
}

#[tokio::test]
async fn finish_sends_the_runner_finish_and_names_the_items_still_open() {
    let mut shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    shepherd.says("koji", &[FINISHING]);
    let (lines, said) = finished(&shepherd).await;
    assert_eq!(
        lines.unwrap(),
        [
            "koji is finishing #7, #9: the board picks nothing new, and the runner stops once \
             they end. `shep kelpie start koji` picks again"
        ]
    );
    assert_eq!(said, Vec::<String>::new());
    assert_eq!(
        asked(&shepherd.writes()),
        ["status", "finish"],
        "the runner stops itself"
    );
}

#[tokio::test]
async fn finish_names_the_adoptions_left_waiting_for_the_next_start() {
    let shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    let waiting = r#"{"project":"koji","run":"finishing","work_items":[{"issue":7}],
        "adopted":[{"pull_request":40,"by_label":false},{"pull_request":41,"by_label":true}]}"#;
    shepherd.says("koji", &[waiting]);
    let (lines, _) = finished(&shepherd).await;
    assert_eq!(
        lines.unwrap()[1],
        "adopted and waiting for a slot, #40, #41 stay waiting until `shep kelpie start koji`"
    );
}

#[tokio::test]
async fn finish_with_no_item_open_says_the_runner_stops_now() {
    let mut shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    shepherd.says(
        "koji",
        &[r#"{"project":"koji","run":"finishing","work_items":[]}"#],
    );
    let (lines, _) = finished(&shepherd).await;
    assert_eq!(
        lines.unwrap(),
        ["koji has no work item open, so its runner stops now"]
    );
    assert_eq!(asked(&shepherd.writes()), ["status", "finish"]);
}

#[tokio::test]
async fn finish_waits_for_a_runner_still_starting() {
    let mut shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    shepherd.says("koji", &[FINISHING]);
    shepherd.just_started("koji");
    let (lines, said) = finished(&shepherd).await;
    assert!(lines.unwrap()[0].starts_with("koji is finishing #7, #9"));
    assert_eq!(said, ["waiting: `koji` is starting"]);
    assert_eq!(asked(&shepherd.writes()), ["status", "status", "finish"]);
}

#[tokio::test]
async fn finishing_a_stopped_runner_changes_nothing() {
    let mut shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), false);
    let (lines, _) = finished(&shepherd).await;
    assert_eq!(
        lines.unwrap(),
        ["koji's runner is stopped, so it has nothing to finish"]
    );
    assert_eq!(asked(&shepherd.writes()), Vec::<String>::new());
}

#[tokio::test]
async fn finish_passes_on_a_runner_s_refusal() {
    let shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    shepherd.says("koji", &[r#"{"error":"unknown action `finish`"}"#]);
    let (err, _) = finished(&shepherd).await;
    assert_eq!(err.unwrap_err(), "unknown action `finish`");
}

#[tokio::test]
async fn finish_leaves_a_sheep_that_is_not_kelpie_s_alone() {
    let mut shepherd = FakeShepherd::new().await;
    shepherd.holds(AppConfig::minimal("koji", "/usr/bin/koji"), true);
    let (err, _) = finished(&shepherd).await;
    let err = err.unwrap_err();
    assert!(err.contains("no kelpie runner named koji"), "{err}");
    assert_eq!(asked(&shepherd.writes()), Vec::<String>::new());
}

#[tokio::test]
async fn start_on_a_runner_finishing_sends_it_start() {
    for run in ["finishing", "finished"] {
        let mut shepherd = FakeShepherd::new().await;
        runner(&shepherd, "koji", Path::new("/src/koji"), true);
        shepherd.holds_dog("kelpie", true);
        let held = format!(r#"{{"project":"koji","run":"{run}"}}"#);
        let started = r#"{"project":"koji"}"#;
        shepherd.says("koji", &[&held, started]);
        let client = client(&shepherd).await;
        let answer = in_time(start(&client, &project("koji"))).await;
        assert_eq!(answer, Ok(vec![started.to_owned()]), "{run}");
        assert_eq!(asked(&shepherd.writes()), ["status", "start"], "{run}");
    }
}

#[tokio::test]
async fn start_passes_on_a_runner_still_stopping_itself() {
    let shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    shepherd.holds_dog("kelpie", true);
    let refused = r#"{"error":"the runner finished and is stopping its sheep"}"#;
    shepherd.says("koji", &[r#"{"project":"koji","run":"finished"}"#, refused]);
    let client = client(&shepherd).await;
    let answer = in_time(start(&client, &project("koji"))).await;
    assert_eq!(
        answer,
        Err("the runner finished and is stopping its sheep".to_owned())
    );
}

#[tokio::test]
async fn pause_clears_finishing_just_before_its_stop() {
    let mut shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    let drained = r#"{"run":"finishing","work_items":[{"issue":7,"phase":{"state":"implement"}}],
        "draining":{"calls":[],"ceiling":3600}}"#;
    shepherd.says("koji", &[drained]);
    let client = client(&shepherd).await;
    let (koji, say) = (project("koji"), &mut |_| {});
    in_time(pause(&client, &koji, FAST, Interrupt::Never, say))
        .await
        .unwrap();
    assert_eq!(
        asked(&shepherd.writes()),
        ["status", "drain", "status", "status", "pausing", "stop"]
    );
}

#[tokio::test]
async fn status_marks_a_runner_finishing() {
    let shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    let both = r#"{"run":"finishing","draining":{"calls":[],"ceiling":3600}}"#;
    shepherd.says("koji", &[FINISHING, both]);
    let client = client(&shepherd).await;
    assert_eq!(
        in_time(status(&client)).await.unwrap(),
        [format!("koji (finishing): {FINISHING}")]
    );
    assert_eq!(
        in_time(status(&client)).await.unwrap(),
        [format!("koji (draining, finishing): {both}")]
    );
}
