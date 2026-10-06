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
async fn start_brings_up_the_runner_then_reaches_it() {
    let mut shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), false);
    shepherd.holds_dog("kelpie", true);

    let client = client(&shepherd).await;
    let started = in_time(start(&client, &project("koji"))).await.unwrap();
    assert_eq!(started, [r#"{"action":"start","sheep":"koji"}"#]);
    assert!(shepherd.sheep("koji").unwrap().1);
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
    assert_eq!(restarts, ["koji"]);
}

#[tokio::test]
async fn start_leaves_a_running_runner_running() {
    let mut shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    shepherd.holds_dog("kelpie", true);
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
        assert!(err.starts_with("no kelpie runner named"), "{err}");
        let err = in_time(pause(&client, &project(name))).await.unwrap_err();
        assert!(err.starts_with("no kelpie runner named"), "{err}");
    }
    assert_eq!(shepherd.writes(), []);
    assert!(
        !shepherd.sheep("web").unwrap().1,
        "a stopped sheep stays stopped"
    );
}

// A runner with no dog is never granted a lease, so it is left stopped.
#[tokio::test]
async fn start_with_kelpie_s_dog_down_says_how_to_bring_it_up() {
    let mut shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), false);
    let client = client(&shepherd).await;
    let err = in_time(start(&client, &project("koji"))).await.unwrap_err();
    assert_eq!(
        err,
        "kelpie's dog is not enabled: `shep enable kelpie` runs it"
    );

    shepherd.holds_dog("kelpie", false);
    let err = in_time(start(&client, &project("koji"))).await.unwrap_err();
    assert!(
        err.contains("run `shep adopt /opt/kelpie --name kelpie`"),
        "{err}"
    );
    assert_eq!(shepherd.writes(), []);
    assert!(!shepherd.sheep("koji").unwrap().1);
}

// A dog that crash-looped to a stop grants nothing, whatever its channel.
#[tokio::test]
async fn start_with_kelpie_s_dog_stopped_says_to_read_its_bleats() {
    let mut shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), false);
    shepherd.holds_dog("kelpie", true);
    shepherd.stops("kelpie");
    let client = client(&shepherd).await;
    let err = in_time(start(&client, &project("koji"))).await.unwrap_err();
    assert_eq!(
        err,
        "kelpie's dog is not running: `shep bleats kelpie` says why"
    );
    assert_eq!(shepherd.writes(), []);
}

// Shep lists a dog that is up and never named itself as silent, and it
// holds nothing.
#[tokio::test]
async fn start_with_kelpie_s_dog_silent_says_so_and_how_to_bring_it_back() {
    let mut shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), false);
    shepherd.holds_dog("kelpie", true);
    shepherd.never_names("kelpie");
    let client = client(&shepherd).await;
    let err = in_time(start(&client, &project("koji"))).await.unwrap_err();
    assert!(err.starts_with("kelpie's dog is silent"), "{err}");
    assert!(err.contains("`shep bleats kelpie`"), "{err}");

    shepherd.gives_up_on("kelpie");
    let err = in_time(start(&client, &project("koji"))).await.unwrap_err();
    assert!(err.contains("shep has given up on it"), "{err}");
    assert!(err.contains("`shep restart kelpie`"), "{err}");
    assert_eq!(shepherd.writes(), []);
    assert!(!shepherd.sheep("koji").unwrap().1);
}

// Git in `folder`, with an identity so a commit needs no global config.
fn git(folder: &Path, args: &[&str]) {
    let ran = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=kelpie",
            "-c",
            "user.email=kelpie@example.invalid",
        ])
        .arg("-C")
        .arg(folder)
        .args(args)
        .output()
        .unwrap();
    assert!(ran.status.success(), "git {args:?}: {ran:?}");
}

#[tokio::test]
async fn a_command_reaches_the_project_whose_repo_holds_the_folder() {
    let shepherd = FakeShepherd::new().await;
    let home = PathBuf::from("/home/me");
    let repo = shepherd.scratch("repos/reactmap");
    git(&repo, &["init", "-q"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
    let deep = shepherd.scratch("repos/reactmap/src/deep");
    let worktree = shepherd.home().join("wt/reactmap/7");
    let at = worktree.to_str().unwrap();
    git(&repo, &["worktree", "add", "-q", "-b", "kelpie/7", at]);
    runner(&shepherd, "koji", &shepherd.scratch("repos/koji"), false);
    runner(&shepherd, "reactmap", &repo, false);
    let client = client(&shepherd).await;

    for folder in [&repo, &deep, &worktree.join(".")] {
        let found = in_time(project_here(&client, folder, &home)).await;
        assert_eq!(found, Ok(project("reactmap")), "{}", folder.display());
    }
    let elsewhere = shepherd.scratch("elsewhere");
    let err = in_time(project_here(&client, &elsewhere, &home))
        .await
        .unwrap_err();
    assert_eq!(
        err,
        format!(
            "no project's repo holds {}, so name one with `-p <project>`: koji, reactmap",
            elsewhere.display()
        )
    );
}

#[tokio::test]
async fn with_no_projects_the_folder_names_none() {
    let shepherd = FakeShepherd::new().await;
    let client = client(&shepherd).await;
    let folder = shepherd.scratch("elsewhere");
    let err = in_time(project_here(&client, &folder, Path::new("/home/me")))
        .await
        .unwrap_err();
    assert_eq!(
        err,
        "no projects: `shep kelpie add` in a checkout sets one up"
    );
}

#[tokio::test]
async fn a_trigger_goes_to_its_project_with_its_params() {
    let mut shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    runner(&shepherd, "reactmap", Path::new("/src/reactmap"), false);
    let client = client(&shepherd).await;
    let sent = in_time(send(
        &client,
        &project("koji"),
        "rule",
        Some("14 no fix it"),
    ))
    .await;
    assert_eq!(
        sent,
        Ok(vec![
            r#"{"action":"rule","params":"14 no fix it","sheep":"koji"}"#.to_owned()
        ])
    );
    let triggers: Vec<Request> = shepherd.writes();
    assert_eq!(
        triggers,
        [Request::Trigger {
            selector: SelectorSpec::Name("koji".into()),
            action: "rule".into(),
            params: Some("14 no fix it".into()),
        }]
    );
    let err = in_time(send(&client, &project("reactmap"), "gate", None)).await;
    assert_eq!(err, Err("reactmap's runner is not running".into()));
    let err = in_time(send(&client, &project("nope"), "drop", None)).await;
    assert_eq!(
        err,
        Err("no kelpie runner named nope in this flock: `shep kelpie add` sets one up".into())
    );
}

#[test]
fn a_runner_s_refusal_is_its_error() {
    assert_eq!(
        refusal(r#"{"error":"no ruling 3 is pending"}"#),
        Some("no ruling 3 is pending".into())
    );
    assert_eq!(refusal(r#"{"project":"koji","rulings":[]}"#), None);
    assert_eq!(refusal("unknown action: rule"), None);
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
    shepherd.holds_dog("kelpie", true);
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
async fn status_marks_a_draining_runner() {
    let shepherd = FakeShepherd::new().await;
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    let draining = r#"{"draining":{"calls":[],"ceiling":3600}}"#;
    shepherd.says("koji", &[draining]);
    let client = client(&shepherd).await;
    assert_eq!(
        in_time(status(&client)).await.unwrap(),
        [format!("koji (draining): {draining}")]
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
