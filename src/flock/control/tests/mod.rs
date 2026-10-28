use std::path::PathBuf;

use serde_json::Value;
use shep_client::shep_core::config::AppConfig;
use shep_client::shep_core::values::UpDuration;

use super::*;
use crate::flock::Launch;
use crate::shepherd::Connection;
use crate::test::{CALL_PATIENCE, FakeShepherd};

mod finish;
mod late;
mod pause;
mod plain;

use pause::{IN_TOOL_7, paused};

// A pause's waits, short so a test of one running out ends soon
const FAST: Patience = Patience {
    poll: Duration::from_millis(10),
    start: Duration::from_secs(10),
    merge: Duration::from_secs(10),
    margin: Duration::from_secs(10),
    tool: Duration::from_secs(10),
};
const EXAMPLE: &str = include_str!("../../../../settings.example.toml");

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
    table["git"]["checkout"] = Value::String(root.display().to_string());
    shepherd.holds(launch(shepherd).runner(&project(name), table), online);
}

// A reach to no socket, so a verb uses `shep trigger` and says nothing about it
fn reach() -> Reach {
    Reach::saying(Path::new("/no/kelpie"), |_| {})
}

async fn client(shepherd: &FakeShepherd) -> Box<dyn Connection> {
    shepherd.connect().await
}

// Every call against the fake shepherd, bounded so a hang fails by name.
async fn in_time<T>(call: impl Future<Output = T>) -> T {
    tokio::time::timeout(CALL_PATIENCE, call)
        .await
        .expect("the call neither ended nor failed in time")
}

fn project(name: &str) -> ProjectName {
    ProjectName::try_from(name).unwrap()
}

// What a command asked of the shepherd, by action, with a stop as `stop`
fn asked(writes: &[Request]) -> Vec<String> {
    (writes.iter())
        .filter_map(|w| match w {
            Request::Trigger { action, .. } => Some(action.clone()),
            Request::Stop { .. } => Some("stop".to_owned()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn start_brings_up_the_runner_then_reaches_it() {
    let mut shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), false);
    shepherd.holds_dog("kelpie", true);

    let client = client(&shepherd).await;
    let started = in_time(start(&client, &project("koji"))).await.unwrap();
    assert_eq!(started, [r#"{"action":"status","sheep":"koji"}"#]);
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
async fn start_brings_up_a_runner_whose_entry_still_sets_kill_timeout() {
    let shepherd = FakeShepherd::new();
    let mut table = crate::test::project_table(EXAMPLE);
    table["git"]["checkout"] = Value::String("/src/koji".into());
    let mut entry = launch(&shepherd).runner(&project("koji"), table);
    entry.kill_timeout = UpDuration::from_millis(10_000);
    shepherd.holds(entry, false);
    shepherd.holds_dog("kelpie", true);

    let client = client(&shepherd).await;
    let started = in_time(start(&client, &project("koji"))).await.unwrap();
    assert_eq!(started, [r#"{"action":"status","sheep":"koji"}"#]);
    let (kept, running) = shepherd.sheep("koji").unwrap();
    assert!(running);
    assert_eq!(kept.kill_timeout.as_millis(), 10_000);
}

#[tokio::test]
async fn start_leaves_a_running_runner_running() {
    let mut shepherd = FakeShepherd::new();
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
    let shepherd = FakeShepherd::new();
    let client = client(&shepherd).await;
    let err = in_time(start(&client, &project("koji"))).await.unwrap_err();
    assert!(err.contains("`shep kelpie add`"), "{err}");
}

#[tokio::test]
async fn start_and_pause_leave_a_sheep_that_is_not_kelpie_s_alone() {
    let mut shepherd = FakeShepherd::new();
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
        let err = in_time(pause(
            &client,
            &project(name),
            FAST,
            Interrupt::Never,
            &mut |_| {},
        ))
        .await
        .unwrap_err();
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
    let mut shepherd = FakeShepherd::new();
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
    let mut shepherd = FakeShepherd::new();
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
    let mut shepherd = FakeShepherd::new();
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
    let shepherd = FakeShepherd::new();
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

// Sets project `name`'s `git.remote`, as a settings file may.
fn remote(shepherd: &FakeShepherd, name: &str, root: &Path, slug: &str) {
    let mut table = crate::test::project_table(EXAMPLE);
    table["git"]["checkout"] = Value::String(root.display().to_string());
    table["git"]["remote"] = Value::String(slug.to_owned());
    shepherd.holds(launch(shepherd).runner(&project(name), table), false);
}

// A plain clone of a repo: not any project's checkout, with `origin` at `url`.
fn clone_of(shepherd: &FakeShepherd, path: &str, url: &str) -> PathBuf {
    let folder = shepherd.scratch(path);
    git(&folder, &["init", "-q"]);
    git(&folder, &["remote", "add", "origin", url]);
    folder
}

#[tokio::test]
async fn a_plain_clone_reaches_the_one_project_whose_remote_is_its_origin() {
    let shepherd = FakeShepherd::new();
    let home = PathBuf::from("/home/me");
    remote(
        &shepherd,
        "koji",
        &shepherd.scratch("runner/koji"),
        "shep-pm/koji",
    );
    remote(
        &shepherd,
        "reactmap",
        &shepherd.scratch("runner/reactmap"),
        "shep-pm/reactmap",
    );
    let clone = clone_of(
        &shepherd,
        "mine/reactmap",
        "git@github.com:Shep-PM/ReactMap.git",
    );
    let deep = shepherd.scratch("mine/reactmap/src/deep");
    let client = client(&shepherd).await;

    for folder in [&clone, &deep] {
        let found = in_time(project_here(&client, folder, &home)).await;
        assert_eq!(found, Ok(project("reactmap")), "{}", folder.display());
    }
}

#[tokio::test]
async fn a_project_with_no_remote_set_matches_on_its_checkouts_origin() {
    let shepherd = FakeShepherd::new();
    let home = PathBuf::from("/home/me");
    let runner_checkout = clone_of(&shepherd, "runner/koji", "git@github.com:shep-pm/koji.git");
    let mut table = crate::test::project_table(EXAMPLE);
    table["git"]["checkout"] = Value::String(runner_checkout.display().to_string());
    table["git"].as_object_mut().unwrap().remove("remote");
    shepherd.holds(launch(&shepherd).runner(&project("koji"), table), false);
    let clone = clone_of(
        &shepherd,
        "mine/koji",
        "https://github.com/shep-pm/koji.git",
    );
    let client = client(&shepherd).await;

    let found = in_time(project_here(&client, &clone, &home)).await;
    assert_eq!(found, Ok(project("koji")));
}

#[tokio::test]
async fn a_clone_whose_origin_is_no_projects_remote_asks_for_a_name() {
    let shepherd = FakeShepherd::new();
    remote(
        &shepherd,
        "koji",
        &shepherd.scratch("runner/koji"),
        "shep-pm/koji",
    );
    let clone = clone_of(&shepherd, "mine/other", "https://github.com/shep-pm/other");
    let client = client(&shepherd).await;

    let err = in_time(project_here(&client, &clone, Path::new("/home/me")))
        .await
        .unwrap_err();
    assert_eq!(
        err,
        format!(
            "no project's repo holds {}, so name one with `-p <project>`: koji",
            clone.display()
        )
    );
}

#[tokio::test]
async fn a_clone_whose_origin_two_projects_share_asks_for_a_name() {
    let shepherd = FakeShepherd::new();
    remote(
        &shepherd,
        "koji",
        &shepherd.scratch("runner/koji"),
        "shep-pm/koji",
    );
    remote(
        &shepherd,
        "koji-fork",
        &shepherd.scratch("runner/fork"),
        "shep-pm/koji",
    );
    let clone = clone_of(
        &shepherd,
        "mine/koji",
        "https://github.com/shep-pm/koji.git",
    );
    let client = client(&shepherd).await;

    let err = in_time(project_here(&client, &clone, Path::new("/home/me")))
        .await
        .unwrap_err();
    assert_eq!(
        err,
        "koji, koji-fork all have shep-pm/koji as their remote, so name one with `-p <project>`"
    );
}

#[tokio::test]
async fn with_no_projects_the_folder_names_none() {
    let shepherd = FakeShepherd::new();
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
    let mut shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    runner(&shepherd, "reactmap", Path::new("/src/reactmap"), false);
    let client = client(&shepherd).await;
    let sent = in_time(send(
        &reach(),
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
    let err = in_time(send(&reach(), &client, &project("reactmap"), "gate", None)).await;
    assert_eq!(err, Err("reactmap's runner is not running".into()));
    let err = in_time(send(&reach(), &client, &project("nope"), "drop", None)).await;
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
async fn status_reports_every_project() {
    let shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    runner(&shepherd, "reactmap", Path::new("/src/reactmap"), false);
    shepherd.holds_dog("kelpie", true);
    let client = client(&shepherd).await;
    let lines = in_time(status(&reach(), &client, Format::Json))
        .await
        .unwrap();
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
    let shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    let draining = r#"{"draining":{"calls":[],"ceiling":3600}}"#;
    shepherd.says("koji", &[draining]);
    let client = client(&shepherd).await;
    assert_eq!(
        in_time(status(&reach(), &client, Format::Json))
            .await
            .unwrap(),
        [format!("koji (draining): {draining}")]
    );
}

#[tokio::test]
async fn a_tell_the_runner_never_answered_is_delivered_and_not_to_be_repeated() {
    let mut shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    shepherd.replies("koji", &[None]);
    let client = client(&shepherd).await;
    let told = in_time(tell(
        &reach(),
        &client,
        &project("koji"),
        "hold #4",
        Format::Plain,
    ))
    .await;
    let lines = told.expect("a note that arrived is no error to retry");
    let [line] = lines.as_slice() else {
        panic!("{lines:?}")
    };
    assert!(line.contains("did not answer in time"), "{line}");
    assert!(line.contains("the note may have arrived"), "{line}");
    assert!(line.contains("do not tell it again"), "{line}");
    assert!(line.contains("`shep bleats koji`"), "{line}");
    assert_eq!(asked(&shepherd.writes()), ["tell"]);
}

#[tokio::test]
async fn a_tell_the_runner_never_answered_prints_an_object_with_json() {
    let shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    shepherd.replies("koji", &[None]);
    let client = client(&shepherd).await;
    let told = in_time(tell(
        &reach(),
        &client,
        &project("koji"),
        "hold #4",
        Format::Json,
    ))
    .await;
    let [line] = told.unwrap().try_into().unwrap();
    let object: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(object["sheep"], "koji");
    let message = object["message"].as_str().unwrap();
    assert!(message.contains("the note may have arrived"), "{message}");
}

#[tokio::test]
async fn a_tell_the_runner_answered_says_it_was_told() {
    let shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    let client = client(&shepherd).await;
    let told = in_time(tell(
        &reach(),
        &client,
        &project("koji"),
        "hold #4",
        Format::Plain,
    ))
    .await;
    assert_eq!(
        told,
        Ok(vec![
            "told koji's project manager, which reads it on its next wake".to_owned()
        ])
    );
}

#[tokio::test]
async fn a_runner_that_refuses_a_tell_is_its_error() {
    let shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    shepherd.says("koji", &[r#"{"error":"koji has no project manager"}"#]);
    let client = client(&shepherd).await;
    let told = in_time(tell(
        &reach(),
        &client,
        &project("koji"),
        "hold #4",
        Format::Plain,
    ))
    .await;
    assert_eq!(told, Err("koji has no project manager".into()));
}

#[tokio::test]
async fn a_runner_still_taking_its_actions_is_starting() {
    let shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    let client = client(&shepherd).await;
    shepherd.just_started("koji");
    assert_eq!(
        in_time(status(&reach(), &client, Format::Json))
            .await
            .unwrap(),
        ["koji: starting"]
    );
    // Each answer from a runner still starting uses the flag up.
    shepherd.just_started("koji");
    let err = in_time(send(&reach(), &client, &project("koji"), "drop", None))
        .await
        .unwrap_err();
    assert_eq!(err, "koji's runner is starting: ask again in a moment");
}

#[tokio::test]
async fn the_flock_s_repos_include_a_project_whose_settings_do_not_parse() {
    let shepherd = FakeShepherd::new();
    remote(
        &shepherd,
        "koji",
        &shepherd.scratch("runner/koji"),
        "acme/koji",
    );
    let mut broken = crate::test::project_table(EXAMPLE);
    let checkout = shepherd.scratch("runner/vault").display().to_string();
    broken["git"]["checkout"] = Value::String(checkout);
    broken["git"]["remote"] = Value::String("acme/vault".to_owned());
    broken["git"]["merging"] = Value::String("no such mode".to_owned());
    shepherd.holds(launch(&shepherd).runner(&project("vault"), broken), false);
    let client = client(&shepherd).await;

    let found = in_time(repos(&client, Path::new("/home/me")))
        .await
        .unwrap();
    let mut names: Vec<&str> = found.iter().map(ForgeSlug::as_str).collect();
    names.sort_unstable();
    assert_eq!(names, ["acme/koji", "acme/vault"]);
}

#[tokio::test]
async fn the_flock_s_repos_are_refused_when_a_project_s_repo_cannot_be_told() {
    let shepherd = FakeShepherd::new();
    // A checkout that is no git checkout, and no `git.remote`.
    let mut table = crate::test::project_table(EXAMPLE);
    let checkout = shepherd.scratch("runner/vault").display().to_string();
    table["git"]["checkout"] = Value::String(checkout);
    table["git"].as_object_mut().unwrap().remove("remote");
    shepherd.holds(launch(&shepherd).runner(&project("vault"), table), false);
    let client = client(&shepherd).await;

    let err = in_time(repos(&client, Path::new("/home/me")))
        .await
        .unwrap_err();
    assert!(err.contains("project vault"), "{err}");
}

#[tokio::test]
async fn a_pause_stops_a_call_still_in_its_tool_after_the_limit() {
    let mut shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    shepherd.says("koji", &[IN_TOOL_7]);
    let patience = Patience {
        tool: Duration::from_millis(100),
        ..FAST
    };
    let (ran, said) = paused(&shepherd, patience, Interrupt::Never).await;
    assert_eq!(
        ran.unwrap(),
        ["koji's runner is stopped: `shep kelpie start koji` runs it again"]
    );
    assert_eq!(asked(&shepherd.writes()).last().unwrap(), "stop");
    assert!(
        said.contains(
            &"`koji` is still in a tool call of #7's worker turn, which ends at its next tool \
              call after 0s, so the stop cuts it short"
                .to_owned()
        ),
        "{said:?}"
    );
    assert!(!shepherd.sheep("koji").unwrap().1);
}
