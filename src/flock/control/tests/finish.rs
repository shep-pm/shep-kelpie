//! `finish`, which lets the open work items end and picks nothing new, and
//! `start`, `pause` and `status` on a runner finishing

use super::*;

// A runner finishing with #7 and #9 still open
const FINISHING: &str = r#"{"project":"koji","run":"finishing",
    "work_items":[{"issue":"7"},{"issue":"9"}]}"#;

// `finish` against the fake shepherd, and what it said while it waited
async fn finished(shepherd: &FakeShepherd) -> (Result<Vec<String>, String>, Vec<String>) {
    let client = client(shepherd).await;
    let mut said = Vec::new();
    let say = &mut |line| said.push(line);
    let ran = in_time(finish(&client, &project("koji"), Format::Plain, FAST, say)).await;
    (ran, said)
}

#[tokio::test]
async fn finish_sends_the_runner_finish_and_names_the_items_still_open() {
    let mut shepherd = FakeShepherd::new();
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
    let shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    let waiting = r#"{"project":"koji","run":"finishing","work_items":[{"issue":"7"}],
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
    let mut shepherd = FakeShepherd::new();
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
    let mut shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    shepherd.says("koji", &[FINISHING]);
    shepherd.just_started("koji");
    let (lines, said) = finished(&shepherd).await;
    assert!(lines.unwrap()[0].starts_with("koji is finishing #7, #9"));
    assert_eq!(said, ["waiting: `koji` is starting"]);
    assert_eq!(asked(&shepherd.writes()), ["status", "status", "finish"]);
}

#[tokio::test]
async fn finishing_a_stopped_runner_prints_an_object_with_json() {
    let shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), false);
    let client = client(&shepherd).await;
    let say = &mut |_| {};
    let lines = in_time(finish(&client, &project("koji"), Format::Json, FAST, say)).await;
    let [line] = lines.unwrap().try_into().unwrap();
    let object: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        object,
        serde_json::json!({
            "sheep": "koji",
            "message": "koji's runner is stopped, so it has nothing to finish",
        })
    );
}

#[tokio::test]
async fn finishing_a_stopped_runner_changes_nothing() {
    let mut shepherd = FakeShepherd::new();
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
    let shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    shepherd.says("koji", &[r#"{"error":"unknown action `finish`"}"#]);
    let (err, _) = finished(&shepherd).await;
    assert_eq!(err.unwrap_err(), "unknown action `finish`");
}

#[tokio::test]
async fn finish_leaves_a_sheep_that_is_not_kelpie_s_alone() {
    let mut shepherd = FakeShepherd::new();
    shepherd.holds(AppConfig::minimal("koji", "/usr/bin/koji"), true);
    let (err, _) = finished(&shepherd).await;
    let err = err.unwrap_err();
    assert!(err.contains("no kelpie runner named koji"), "{err}");
    assert_eq!(asked(&shepherd.writes()), Vec::<String>::new());
}

#[tokio::test]
async fn start_on_a_runner_finishing_sends_it_start() {
    for run in ["finishing", "finished"] {
        let mut shepherd = FakeShepherd::new();
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
    let shepherd = FakeShepherd::new();
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
    let mut shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    let drained = r#"{"run":"finishing","work_items":[{"issue":"7","phase":{"state":"implement"}}],
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
    let shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    let both = r#"{"run":"finishing","draining":{"calls":[],"ceiling":3600}}"#;
    shepherd.says("koji", &[FINISHING, both]);
    let client = client(&shepherd).await;
    assert_eq!(
        in_time(status(&reach(), &client, Format::Json))
            .await
            .unwrap(),
        [format!("koji (finishing): {FINISHING}")]
    );
    assert_eq!(
        in_time(status(&reach(), &client, Format::Json))
            .await
            .unwrap(),
        [format!("koji (draining, finishing): {both}")]
    );
}

// Who was sent which action, in order
fn sent(writes: &[Request]) -> Vec<(String, String)> {
    (writes.iter())
        .filter_map(|w| match w {
            Request::Trigger {
                selector: SelectorSpec::Name(sheep),
                action,
                ..
            } => Some((sheep.clone(), action.clone())),
            _ => None,
        })
        .collect()
}

// `finish_all` against the fake shepherd
async fn finished_all(shepherd: &FakeShepherd, format: Format) -> Result<FinishedAll, String> {
    let client = client(shepherd).await;
    let say = &mut |_| {};
    in_time(finish_all(&client, format, FAST, say)).await
}

#[tokio::test]
async fn finish_all_sends_every_projects_runner_finish() {
    let mut shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    runner(&shepherd, "rotom", Path::new("/src/rotom"), true);
    shepherd.says("koji", &[FINISHING]);
    shepherd.says(
        "rotom",
        &[r#"{"project":"rotom","run":"finishing","work_items":[]}"#],
    );
    let done = finished_all(&shepherd, Format::Plain).await.unwrap();
    assert_eq!(
        done.lines,
        [
            "koji is finishing #7, #9: the board picks nothing new, and the runner stops once \
             they end. `shep kelpie start koji` picks again",
            "rotom has no work item open, so its runner stops now",
        ]
    );
    assert_eq!(done.refused, Vec::<String>::new());
    let sent = sent(&shepherd.writes());
    for sheep in ["koji", "rotom"] {
        assert!(sent.contains(&(sheep.into(), "finish".into())), "{sheep}");
    }
}

#[tokio::test]
async fn finish_all_goes_on_past_a_runner_that_refuses() {
    let mut shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    runner(&shepherd, "rotom", Path::new("/src/rotom"), true);
    shepherd.says("koji", &[r#"{"error":"unknown action `finish`"}"#]);
    shepherd.says(
        "rotom",
        &[r#"{"project":"rotom","run":"finishing","work_items":[]}"#],
    );
    let done = finished_all(&shepherd, Format::Plain).await.unwrap();
    assert_eq!(done.refused, ["koji: unknown action `finish`"]);
    assert_eq!(
        done.lines,
        ["rotom has no work item open, so its runner stops now"]
    );
    let sent = sent(&shepherd.writes());
    assert!(
        sent.contains(&("rotom".into(), "finish".into())),
        "{sent:?}"
    );
}

#[tokio::test]
async fn finish_all_leaves_a_stopped_runner_alone() {
    let mut shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), false);
    runner(&shepherd, "rotom", Path::new("/src/rotom"), true);
    shepherd.says(
        "rotom",
        &[r#"{"project":"rotom","run":"finishing","work_items":[]}"#],
    );
    let done = finished_all(&shepherd, Format::Plain).await.unwrap();
    assert_eq!(done.refused, Vec::<String>::new());
    assert_eq!(
        done.lines,
        [
            "koji's runner is stopped, so it has nothing to finish",
            "rotom has no work item open, so its runner stops now",
        ]
    );
    let sent = sent(&shepherd.writes());
    assert!(sent.iter().all(|(sheep, _)| sheep != "koji"), "{sent:?}");
}

#[tokio::test]
async fn finish_all_prints_each_runner_s_answer_on_a_line_with_json() {
    let shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    runner(&shepherd, "rotom", Path::new("/src/rotom"), false);
    shepherd.says("koji", &[FINISHING]);
    let done = finished_all(&shepherd, Format::Json).await.unwrap();
    let [koji, rotom] = done.lines.try_into().unwrap();
    assert_eq!(koji, format!("koji (finishing): {FINISHING}"));
    assert_eq!(
        rotom,
        r#"rotom: {"message":"rotom's runner is stopped, so it has nothing to finish","sheep":"rotom"}"#
    );
}

#[tokio::test]
async fn finish_all_with_no_project_says_so() {
    let shepherd = FakeShepherd::new();
    let done = finished_all(&shepherd, Format::Plain).await.unwrap();
    assert_eq!(
        done.lines,
        ["no projects: `shep kelpie add` in a checkout sets one up"]
    );
}

#[tokio::test]
async fn finish_named_all_points_at_the_flag_when_no_project_is_called_that() {
    let shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    let client = client(&shepherd).await;
    let say = &mut |_| {};
    let ran = in_time(finish(&client, &project("all"), Format::Plain, FAST, say)).await;
    let err = ran.unwrap_err();
    assert!(err.contains("no kelpie runner named all"), "{err}");
    assert!(err.contains("finish --all finishes every project"), "{err}");
}

#[tokio::test]
async fn finish_all_goes_on_past_a_name_no_project_can_have() {
    let mut shepherd = FakeShepherd::new();
    let mut table = crate::test::project_table(EXAMPLE);
    table["git"]["checkout"] = Value::String("/src/odd".into());
    let mut odd = launch(&shepherd).runner(&project("koji"), table);
    odd.name = "odd name".into();
    shepherd.holds(odd, true);
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    runner(&shepherd, "rotom", Path::new("/src/rotom"), true);
    for name in ["koji", "rotom"] {
        let open = format!(r#"{{"project":"{name}","run":"finishing","work_items":[]}}"#);
        shepherd.says(name, &[&open]);
    }
    let done = finished_all(&shepherd, Format::Plain).await.unwrap();
    assert_eq!(done.refused.len(), 1, "{:?}", done.refused);
    assert!(
        done.refused[0].starts_with("odd name: "),
        "{:?}",
        done.refused
    );
    assert_eq!(
        done.lines,
        [
            "koji has no work item open, so its runner stops now",
            "rotom has no work item open, so its runner stops now",
        ]
    );
    let sent = sent(&shepherd.writes());
    assert!(
        sent.contains(&("rotom".into(), "finish".into())),
        "{sent:?}"
    );
}
