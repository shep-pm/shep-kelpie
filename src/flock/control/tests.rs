use std::path::PathBuf;

use serde_json::Value;
use shep_client::shep_core::config::AppConfig;

use super::*;
use crate::flock::Launch;
use crate::shepherd;
use crate::test::FakeShepherd;

// Bounds every call against a fake shepherd, so a hang fails by name.
const PATIENCE: Duration = Duration::from_secs(40);
const EXAMPLE: &str = include_str!("../../../settings.example.toml");

fn launch(shepherd: &FakeShepherd) -> Launch {
    Launch {
        kelpie: "/opt/kelpie".into(),
        shep_home: shepherd.home().to_owned(),
        kelpie_home: None,
    }
}

/// Registers project `name`'s runner for the checkout at `root`, up when `online`
fn runner(shepherd: &FakeShepherd, name: &str, root: &Path, online: bool) {
    let mut table = crate::test::project_table(EXAMPLE);
    table.insert("repo".into(), Value::String(root.display().to_string()));
    shepherd.holds(launch(shepherd).runner(&project(name), table), online);
}

async fn client(shepherd: &FakeShepherd) -> Client {
    shepherd::connect(shepherd.home()).await.unwrap()
}

// Every call against the fake shepherd, bounded so a hang fails by name.
async fn in_time<T>(call: impl Future<Output = T>) -> T {
    tokio::time::timeout(PATIENCE, call)
        .await
        .expect("the call neither ended nor failed in time")
}

fn project(name: &str) -> ProjectName {
    ProjectName::try_from(name).unwrap()
}

// Real sockets under a real clock: a paused one would time out the
// handshake while the fake's socket is merely waiting.
#[tokio::test]
async fn start_brings_up_the_dog_and_the_runner_then_reaches_the_runner() {
    let mut shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), false);
    shepherd.holds(launch(&shepherd).dog(), false);

    let client = client(&shepherd).await;
    let started = in_time(start(&client, &project("koji"))).await.unwrap();
    assert_eq!(started, [r#"{"action":"start","sheep":"koji"}"#]);
    assert!(shepherd.sheep("koji").unwrap().1);
    assert!(shepherd.sheep("kelpie-dog").unwrap().1);
    let restarts: Vec<_> = shepherd
        .writes()
        .into_iter()
        .filter_map(|w| match w {
            Request::Restart {
                selector: SelectorSpec::Name(name),
            } => Some(name),
            _ => None,
        })
        .collect();
    assert_eq!(restarts, ["kelpie-dog", "koji"]);
}

#[tokio::test]
async fn start_leaves_a_running_runner_running() {
    let mut shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    let client = client(&shepherd).await;
    in_time(start(&client, &project("koji"))).await.unwrap();
    let writes = shepherd.writes();
    assert!(
        !writes.iter().any(|w| matches!(w, Request::Restart { .. })),
        "{writes:?}"
    );
}

#[tokio::test]
async fn start_without_a_runner_says_to_add_one() {
    let shepherd = FakeShepherd::new().await;
    let client = client(&shepherd).await;
    let err = in_time(start(&client, &project("koji"))).await.unwrap_err();
    assert!(err.contains("`shep kelpie add`"), "{err}");
}

#[tokio::test]
async fn start_and_pause_leave_a_sheep_that_is_not_kelpie_s_alone() {
    let mut shepherd = FakeShepherd::new().await;
    shepherd.holds(AppConfig::minimal("web", "/srv/web"), false);
    // A table alone does not make a runner: kelpie did not start it.
    let mut other = AppConfig::minimal("koji", "/srv/koji");
    other.dogs = launch(&shepherd)
        .runner(&project("koji"), crate::test::project_table(EXAMPLE))
        .dogs;
    shepherd.holds(other, false);
    let client = client(&shepherd).await;
    for name in ["web", "koji"] {
        let err = in_time(start(&client, &project(name))).await.unwrap_err();
        assert!(err.starts_with("no shep-kelpie runner named"), "{err}");
        let err = in_time(pause(&client, &project(name))).await.unwrap_err();
        assert!(err.starts_with("no shep-kelpie runner named"), "{err}");
    }
    assert_eq!(shepherd.writes(), []);
    assert!(
        !shepherd.sheep("web").unwrap().1,
        "a stopped sheep stays stopped"
    );
}

#[tokio::test]
async fn start_with_kelpie_adopted_and_enabled_and_no_dog_says_to_disable_it() {
    let mut shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), false);
    shepherd.holds_dog("kelpie");
    let client = client(&shepherd).await;
    let err = in_time(start(&client, &project("koji"))).await.unwrap_err();
    assert!(err.contains("run `shep disable kelpie`"), "{err}");
    assert_eq!(shepherd.writes(), []);
}

#[tokio::test]
async fn the_project_here_is_the_one_whose_settings_name_this_checkout() {
    let shepherd = FakeShepherd::new().await;
    let home = PathBuf::from("/home/me");
    runner(&shepherd, "koji", Path::new("/src/koji"), false);
    runner(&shepherd, "reactmap", Path::new("/src/reactmap"), false);
    let client = client(&shepherd).await;
    let found = in_time(project_here(&client, Path::new("/src/reactmap"), &home)).await;
    assert_eq!(found, Ok(project("reactmap")));
    let err = in_time(project_here(&client, Path::new("/src/other"), &home))
        .await
        .unwrap_err();
    assert!(err.starts_with("no project runs from /src/other"), "{err}");
}

#[tokio::test]
async fn pause_reaches_a_running_runner_and_names_one_that_is_not() {
    let shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    runner(&shepherd, "reactmap", Path::new("/src/reactmap"), false);
    let client = client(&shepherd).await;
    let paused = in_time(pause(&client, &project("koji"))).await.unwrap();
    assert_eq!(paused, [r#"{"action":"pause","sheep":"koji"}"#]);
    let err = in_time(pause(&client, &project("reactmap")))
        .await
        .unwrap_err();
    assert_eq!(err, "reactmap's runner is not running");
}

#[tokio::test]
async fn status_reports_every_project() {
    let shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    runner(&shepherd, "reactmap", Path::new("/src/reactmap"), false);
    shepherd.holds(launch(&shepherd).dog(), true);
    let client = client(&shepherd).await;
    let lines = in_time(status(&client)).await.unwrap();
    assert_eq!(
        lines,
        [
            r#"koji: {"action":"status","sheep":"koji"}"#,
            "reactmap: not running",
        ]
    );
}

#[tokio::test]
async fn a_runner_still_taking_its_actions_is_starting() {
    let shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    let client = client(&shepherd).await;
    shepherd.just_started("koji");
    assert_eq!(in_time(status(&client)).await.unwrap(), ["koji: starting"]);
    // Each answer from a runner still starting uses the flag up.
    shepherd.just_started("koji");
    let err = in_time(pause(&client, &project("koji"))).await.unwrap_err();
    assert_eq!(err, "koji's runner is starting: ask again in a moment");
}
