//! `pause`, which stops a runner once its calls end, and `rule`, which
//! reaches a stopped runner through the answer it leaves

use super::*;

// A runner draining with #7's worker turn still running, whose calls are
// ended within the ceiling it gives
const RUNNING_7: &str = r#"{"work_items":[{"issue":7,"phase":{"state":"implement"}}],
    "draining":{"calls":[{"issue":7,"role":"worker"}],"ceiling":0}}"#;

// A runner draining with no call left
const DRAINED: &str = r#"{"work_items":[{"issue":7,"phase":{"state":"implement"}}],
    "draining":{"calls":[],"ceiling":3600}}"#;

// The same, with #7 held by the maintainer's own attached session
const ATTACHED: &str = r#"{"work_items":[{"issue":7,"phase":{"state":"implement"},
    "attached":{"pid":4242,"started":"Mon Oct  5 09:00:00 2026"}}],
    "draining":{"calls":[],"ceiling":3600}}"#;

// Merging #7 with no call running
const MERGING: &str = r#"{"work_items":[{"issue":7,"phase":{"state":"merge","head":"abc"}}],
    "draining":{"calls":[],"ceiling":3600}}"#;

// What a pause asked of the shepherd, by action, with a stop as `stop`
fn asked(writes: &[Request]) -> Vec<String> {
    (writes.iter())
        .filter_map(|w| match w {
            Request::Trigger { action, .. } => Some(action.clone()),
            Request::Stop { .. } => Some("stop".to_owned()),
            _ => None,
        })
        .collect()
}

async fn paused(
    shepherd: &FakeShepherd,
    patience: Patience,
    interrupt: Interrupt,
) -> (Result<Vec<String>, String>, Vec<String>) {
    let client = client(shepherd).await;
    let mut said = Vec::new();
    let say = &mut |line| said.push(line);
    let ran = in_time(pause(&client, &project("koji"), patience, interrupt, say)).await;
    (ran, said)
}

#[tokio::test]
async fn pause_stops_the_runner_once_its_calls_end() {
    let mut shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    shepherd.says("koji", &[RUNNING_7, RUNNING_7, RUNNING_7, DRAINED]);
    let (ran, said) = paused(&shepherd, FAST, Interrupt::Never).await;
    assert_eq!(
        ran.unwrap(),
        ["koji's runner is stopped: `shep kelpie start koji` runs it again"]
    );
    assert_eq!(
        asked(&shepherd.writes()),
        [
            "status", "drain", "drain", "drain", "status", "status", "stop"
        ],
        "drained until no call ran, the merge wait, then the stop"
    );
    assert!(
        !shepherd.sheep("koji").unwrap().1,
        "`shep ls` shows it stopped"
    );
    assert_eq!(
        said,
        [
            "draining `koji`, which starts no new call until it stops: if this pause stops \
             first, `shep kelpie undrain -p koji` lets it start them again",
            "waiting: `koji` is running #7's worker turn",
        ]
    );
}

#[tokio::test]
async fn pause_waits_out_a_merge_in_flight() {
    let mut shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    shepherd.says("koji", &[DRAINED, DRAINED, MERGING, DRAINED]);
    let (ran, said) = paused(&shepherd, FAST, Interrupt::Never).await;
    ran.unwrap();
    assert_eq!(
        asked(&shepherd.writes()),
        ["status", "drain", "status", "status", "status", "stop"]
    );
    assert!(
        said.contains(&"waiting: `koji` is merging #7".to_owned()),
        "{said:?}"
    );
}

#[tokio::test]
async fn pause_leaves_an_attached_session_running_and_says_so() {
    let mut shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    shepherd.says("koji", &[ATTACHED]);
    let (ran, _) = paused(&shepherd, FAST, Interrupt::Never).await;
    assert_eq!(
        ran.unwrap(),
        [
            "#7 is attached in your terminal: its session goes on, and the work item stays \
             held until it ends",
            "koji's runner is stopped: `shep kelpie start koji` runs it again",
        ]
    );
    assert_eq!(asked(&shepherd.writes()).last().unwrap(), "stop");
}

#[tokio::test]
async fn a_call_that_outlasts_its_ceiling_leaves_the_runner_running_and_undrained() {
    let mut shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    shepherd.says("koji", &[RUNNING_7]);
    let patience = Patience {
        margin: Duration::from_millis(100),
        ..FAST
    };
    let err = paused(&shepherd, patience, Interrupt::Never)
        .await
        .0
        .unwrap_err();
    assert_eq!(
        err,
        "`koji` is running #7's worker turn after 0s, so it was not stopped, and it was sent \
         `undrain`, so it starts calls again"
    );
    let asked = asked(&shepherd.writes());
    assert_eq!(asked.last().unwrap(), "undrain");
    assert!(!asked.contains(&"stop".to_owned()), "{asked:?}");
    assert!(shepherd.sheep("koji").unwrap().1);
}

#[tokio::test]
async fn a_ctrl_c_while_the_runner_drains_undrains_it() {
    let mut shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    shepherd.says("koji", &[RUNNING_7]);
    let ctrl_c = Interrupt::After(Duration::from_millis(100));
    let err = paused(&shepherd, FAST, ctrl_c).await.0.unwrap_err();
    assert!(
        err.starts_with("stopped by Ctrl-C while `koji` was pausing"),
        "{err}"
    );
    assert!(err.contains("it was sent `undrain`"), "{err}");
    assert_eq!(asked(&shepherd.writes()).last().unwrap(), "undrain");
    assert!(shepherd.sheep("koji").unwrap().1);
}

#[tokio::test]
async fn pause_waits_for_a_runner_still_starting_then_drains_it() {
    let mut shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    shepherd.says("koji", &[DRAINED]);
    shepherd.just_started("koji");
    let (ran, said) = paused(&shepherd, FAST, Interrupt::Never).await;
    ran.unwrap();
    assert_eq!(
        asked(&shepherd.writes()),
        ["status", "status", "drain", "status", "status", "stop"]
    );
    assert_eq!(said[0], "waiting: `koji` is starting");
    assert!(
        !said.iter().any(|l| l.contains("from before `drain`")),
        "{said:?}"
    );
}

#[tokio::test]
async fn a_runner_that_stops_while_it_is_waited_on_is_reported_stopped() {
    let mut shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    shepherd.says("koji", &[RUNNING_7]);
    let stopped_elsewhere = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        shepherd.stops("koji");
    };
    let ((ran, _), ()) = tokio::join!(paused(&shepherd, FAST, Interrupt::Never), stopped_elsewhere);
    assert_eq!(
        ran.unwrap(),
        ["koji's runner stopped while it was being paused: `shep kelpie start koji` runs it again"]
    );
    let asked = asked(&shepherd.writes());
    assert!(
        !asked.iter().any(|a| a == "stop" || a == "undrain"),
        "{asked:?}"
    );
}

#[tokio::test]
async fn start_sends_a_runner_from_before_the_run_state_went_its_own_start() {
    let mut shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    shepherd.holds_dog("kelpie", true);
    let started = r#"{"project":"koji","run":"running"}"#;
    shepherd.says("koji", &[r#"{"project":"koji","run":"paused"}"#, started]);
    let client = client(&shepherd).await;
    let answer = in_time(start(&client, &project("koji"))).await;
    assert_eq!(answer, Ok(vec![started.to_owned()]));
    assert_eq!(asked(&shepherd.writes()), ["status", "start"]);

    shepherd.says("koji", &[r#"{"project":"koji"}"#]);
    in_time(start(&client, &project("koji"))).await.unwrap();
    assert_eq!(
        asked(&shepherd.writes()),
        ["status"],
        "a current runner gets no `start`"
    );
}

#[tokio::test]
async fn pausing_a_stopped_runner_changes_nothing() {
    let mut shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), false);
    let (ran, said) = paused(&shepherd, FAST, Interrupt::Never).await;
    assert_eq!(
        ran.unwrap(),
        ["koji's runner is stopped: `shep kelpie start koji` runs it"]
    );
    assert_eq!((shepherd.writes(), said), (vec![], vec![]));
}

#[tokio::test]
async fn a_ruling_reaches_a_running_runner_at_once() {
    let mut shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    let answers = shepherd.scratch("kelpie/koji/answers");
    let client = client(&shepherd).await;
    let ruled = in_time(rule(&client, &project("koji"), "14 yes", &answers)).await;
    assert_eq!(ruled, Ok(vec!["ruling 14 on koji: yes".to_owned()]));
    assert_eq!(asked(&shepherd.writes()), ["rule"]);
    assert!(!answers.join("14").exists());
}

#[tokio::test]
async fn a_ruling_for_a_stopped_runner_is_left_for_its_start() {
    let mut shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), false);
    let answers = shepherd.home().join("kelpie/koji/answers");
    let client = client(&shepherd).await;
    let ruled = in_time(rule(&client, &project("koji"), "14 no rename it", &answers)).await;
    assert_eq!(
        ruled,
        Ok(vec![
            "ruling 14 on koji: no rename it".to_owned(),
            "koji's runner is stopped, so it acts on the answer when it starts: \
             `shep kelpie start koji`"
                .to_owned(),
        ])
    );
    assert_eq!(
        std::fs::read_to_string(answers.join("14")).unwrap(),
        "14 no rename it"
    );
    assert!(
        !shepherd.sheep("koji").unwrap().1,
        "the answer started nothing"
    );
    assert_eq!(asked(&shepherd.writes()), ["rule"]);
}

#[tokio::test]
async fn a_runner_s_refusal_of_a_ruling_is_its_error() {
    let shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    shepherd.says("koji", &[r#"{"error":"no ruling 14 is pending"}"#]);
    let answers = shepherd.home().join("kelpie/koji/answers");
    let client = client(&shepherd).await;
    let err = in_time(rule(&client, &project("koji"), "14 yes", &answers)).await;
    assert_eq!(err, Err("no ruling 14 is pending".into()));
    assert!(!answers.exists());
}
