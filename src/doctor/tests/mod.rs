use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::*;
use crate::adapters::host::WindowsInstall;
use crate::flock::Launch;
use crate::github::AppTokens;
use crate::ports::{ForgeError, MeterError, Visibility};
use crate::shepherd::SHEP_VERSION;
use crate::test::{
    CALL_PATIENCE, FakeAlerts, FakeClock, FakeForge, FakeGithub, FakeMeter, FakeReviewer,
    FakeShepherd, git, project_table, unreachable_url,
};
use crate::tools::Tools;

mod tickets;

const EXAMPLE: &str = include_str!("../../../settings.example.toml");

// A URL no test would print by chance, so finding it in a report is a leak.
const SECRET: &str = "https://ntfy.example.invalid/kelpie-s3cr3t-topic";
const WEBHOOK: &str =
    "[webhook]\nkind = \"ntfy\"\nurl = \"https://ntfy.example.invalid/kelpie-s3cr3t-topic\"\n";

// A machine with a login token, no keychain and no gaps, unless it is given them
#[derive(Debug, Default)]
struct FakeHost {
    gaps: Vec<&'static str>,
    keychain: bool,
    token: bool,
    off_path: Vec<&'static str>,
    os: &'static str,
    // What `pi --version` answers, `crate::adapters::PI_MINIMUM` when unset
    pi_version: Option<Result<&'static str, &'static str>>,
    // What `goose --version` prints, `crate::adapters::GOOSE_MINIMUM` when unset
    goose_version: Option<Result<&'static str, &'static str>>,
    // When set, `PATH` holds exactly these programs and nothing else
    path: Option<Vec<&'static str>>,
    // What `windows_status` answers, a ready install with loopback closed when unset
    windows: Option<Result<WindowsInstall, &'static str>>,
}

impl Host for FakeHost {
    fn on_path(&self, program: &str) -> bool {
        match &self.path {
            Some(names) => names.contains(&program),
            None => !self.off_path.contains(&program),
        }
    }

    fn sandbox_gaps(&self) -> Vec<&'static str> {
        self.gaps.clone()
    }

    fn has_keychain(&self) -> bool {
        self.keychain
    }

    fn os(&self) -> &'static str {
        self.os
    }

    fn windows_status(&self) -> Result<WindowsInstall, String> {
        match &self.windows {
            Some(Ok(status)) => Ok(status.clone()),
            Some(Err(why)) => Err((*why).to_owned()),
            None => Ok(WindowsInstall::provisioned(false)),
        }
    }

    fn login_token(&self) -> bool {
        self.token
    }

    fn program_version(&self, program: &str) -> Result<String, String> {
        let (said, minimum) = match program {
            "goose" => (self.goose_version, crate::adapters::GOOSE_MINIMUM),
            _ => (self.pi_version, crate::adapters::PI_MINIMUM),
        };
        match said {
            // The real reader trims the line it takes, as `goose` needs.
            Some(answer) => answer.map(|v| v.trim().to_owned()).map_err(str::to_owned),
            None => Ok(minimum.to_owned()),
        }
    }
}

/// A shepherd holding one project, `koji`, on a machine with every piece in
/// place: a public repo with kelpie's labels, a logged-in `claude` and `gh`,
/// a sandbox, a webhook, kelpie's App installed on koji and golbat, the local round off and
/// no review bot listed
struct Scene {
    shepherd: FakeShepherd,
    forge: FakeForge,
    github: FakeGithub,
    app: AppTokens,
    meter: FakeMeter,
    codex_meter: FakeMeter,
    reviewer: FakeReviewer,
    alerts: FakeAlerts,
    host: FakeHost,
    clock: FakeClock,
    home: PathBuf,
    kelpie_home: PathBuf,
    old_home: PathBuf,
}

impl Scene {
    async fn new() -> Self {
        Self::with_koji(|_, _| {}).await
    }

    /// The flock as `new` makes it, `koji`'s table as `edit` changes it given the scene
    async fn with_koji(edit: impl FnOnce(&Self, &mut Map<String, Value>)) -> Self {
        let shepherd = FakeShepherd::new();
        let forge = FakeForge::new(PathBuf::from("/nowhere"));
        forge.set_repo_labels(&[
            "bug",
            "ready-for-agent",
            "ready-for-human",
            "in-progress",
            "review please",
        ]);
        shepherd.holds_section(WEBHOOK);
        shepherd.holds_dog("kelpie", true);
        let clock = FakeClock::at(1_000);
        let home = shepherd.scratch("home");
        let kelpie_home = shepherd.scratch("kelpie");
        let github = FakeGithub::new(clock.clone(), "shep-pm");
        github.registered(&kelpie_home, "shep-pm/koji");
        github.install("shep-pm/golbat", 8);
        let scene = Self {
            old_home: home.join(".kelpie"),
            home,
            app: github.tokens(&kelpie_home),
            github,
            kelpie_home,
            forge,
            meter: FakeMeter::idle(),
            codex_meter: FakeMeter::idle(),
            reviewer: FakeReviewer::default(),
            alerts: FakeAlerts::on(clock.clone()),
            host: FakeHost {
                token: true,
                ..FakeHost::default()
            },
            clock,
            shepherd,
        };
        crate::agents::write_bound_stubs(
            &scene.kelpie_home.join("agents"),
            "harness: claude-code\nmodel: claude-opus-5-5\neffort: high\n",
        );
        scene.runs("koji", |t| edit(&scene, t));
        scene
    }

    /// Puts a runner named `name` in the flock, with the default table, its
    /// `rulings.pm_decides` written in full, as `edit` changes it
    fn runs(&self, name: &str, edit: impl FnOnce(&mut Map<String, Value>)) {
        let mut table = project_table(EXAMPLE);
        let repo = self.home.join(name);
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(
            &repo,
            &["remote", "add", "origin", "https://github.com/o/r.git"],
        );
        table["git"]["checkout"] = json!(repo);
        table["git"]["remote"] = json!(format!("shep-pm/{name}"));
        table["agents"]["reviewers"] = json!(["defect-hunter"]);
        crate::flock::set::migrate::pm_decides(&mut table);
        edit(&mut table);
        let launch = Launch {
            kelpie: "/opt/kelpie".into(),
            shep_home: self.shepherd.home().to_owned(),
            kelpie_home: None,
        };
        let name = ProjectName::try_from(name).unwrap();
        self.shepherd.holds(launch.runner(&name, table), false);
    }

    /// Puts `.claude/launch.json` with one configuration on `origin/main` of
    /// the checkout of `name`, which is what plans a dev server for its turns
    fn launches(&self, name: &str) {
        let repo = self.home.join(name);
        std::fs::create_dir_all(repo.join(".claude")).unwrap();
        std::fs::write(
            repo.join(".claude/launch.json"),
            r#"{"configurations": [{"name": "web", "runtimeExecutable": "bun", "runtimeArgs": ["run", "dev"], "port": 5173}]}"#,
        )
        .unwrap();
        git(&repo, &["add", "--all"]);
        git(&repo, &["commit", "-q", "-m", "launch"]);
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    }

    async fn check(&self, ask: Ask<'_>) -> Report {
        let dial = self.shepherd.dial();
        let probes = Probes {
            meter: &self.meter,
            codex_meter: &|_: &Path| Box::new(self.codex_meter.clone()) as Box<dyn Meter>,
            forge: &self.forge,
            github: &self.app,
            reviewer: &self.reviewer,
            alerts: &self.alerts,
            host: &self.host,
            clock: &self.clock,
            shepherd: &*dial,
        };
        let here = Here {
            home: &self.home,
            kelpie_home: &self.kelpie_home,
            shep_home: self.shepherd.home(),
            old_home: Some(&self.old_home),
        };
        let done = check(self.shepherd.home(), probes, here, ask);
        tokio::time::timeout(CALL_PATIENCE, done)
            .await
            .expect("doctor neither ended nor failed in time")
    }

    async fn report(&self) -> Report {
        self.check(Ask::default()).await
    }
}

fn verdict<'r>(report: &'r Report, subject: &str) -> &'r Verdict {
    let mut found = report.lines.iter().filter(|l| l.subject == subject);
    let line = found
        .next()
        .unwrap_or_else(|| panic!("no `{subject}` line in {:#?}", report.render()));
    assert!(found.next().is_none(), "two `{subject}` lines");
    &line.verdict
}

fn ok(report: &Report, subject: &str) -> String {
    match verdict(report, subject) {
        Verdict::Ok(found) => found.clone(),
        other => panic!("`{subject}` is {other:?}"),
    }
}

/// What is wrong and the fix, for a line that is missing
fn missing(report: &Report, subject: &str) -> (String, String) {
    match verdict(report, subject) {
        Verdict::Missing { what, fix } => (what.clone(), fix.clone()),
        other => panic!("`{subject}` is {other:?}"),
    }
}

fn subjects(report: &Report) -> Vec<&str> {
    report.lines.iter().map(|l| l.subject.as_str()).collect()
}

#[tokio::test]
async fn a_machine_with_everything_in_place_passes_and_changes_nothing() {
    let mut scene = Scene::new().await;
    scene.runs("golbat", |t| {
        t["agents"]["reviewers"] = json!(["defect-hunter", "coderabbit"]);
    });
    scene.shepherd.writes();
    let labels = scene.forge.repo_labels_now();

    let report = scene.report().await;

    assert!(report.passed(), "{:#?}", report.render());
    assert!(
        report
            .lines
            .iter()
            .all(|l| matches!(l.verdict, Verdict::Ok(_)))
    );
    assert_eq!(
        subjects(&report),
        [
            "claude",
            "gh",
            "sandbox",
            "shepherd",
            "dog",
            "golbat: checkout",
            "golbat: base",
            "golbat: tickets",
            "golbat: implementers",
            "golbat: push access",
            "golbat: labels",
            "golbat: pull request template",
            "golbat: coderabbit",
            "golbat: github app",
            "golbat: workers",
            "golbat: reads",
            "golbat: maintainer",
            "golbat: claude login",
            "golbat: reviewers",
            "golbat: rulings",
            "koji: checkout",
            "koji: base",
            "koji: tickets",
            "koji: implementers",
            "koji: push access",
            "koji: labels",
            "koji: pull request template",
            "koji: github app",
            "koji: workers",
            "koji: reads",
            "koji: maintainer",
            "koji: claude login",
            "koji: reviewers",
            "koji: rulings",
        ]
    );
    assert_eq!(scene.shepherd.writes(), []);
    assert_eq!(scene.forge.repo_labels_now(), labels);
    assert_eq!(scene.alerts.posts(), []);
    assert_eq!(
        report.render().last().unwrap(),
        "nothing a project needs is missing"
    );
    assert_eq!(
        ok(&report, "golbat: implementers"),
        "the agent board picks among opus-high, sonnet-high"
    );
}

#[tokio::test]
async fn a_listed_implementer_with_no_description_makes_its_line_unsure_naming_it() {
    let scene = Scene::new().await;
    scene.runs("golbat", |_| {});
    let agents = scene.kelpie_home.join("agents");
    let mine = "---\nrole: implementer\nharness: claude-code\nmodel: claude-opus-5-5\n\
                effort: high\n---\n";
    crate::test::write_in(&agents, "mine.md", mine);

    let report = scene.report().await;

    let Verdict::Unsure { what, next } = verdict(&report, "golbat: implementers") else {
        panic!("{:#?}", report.render());
    };
    assert_eq!(what, "mine has no description");
    let file = agents.join("mine.md");
    assert_eq!(
        next,
        &format!("add a `description` key to {}", file.display())
    );
}

#[tokio::test]
async fn an_md_file_named_for_no_agent_is_named_and_unsure() {
    let scene = Scene::new().await;
    scene.runs("golbat", |_| {});
    let agents = scene.kelpie_home.join("agents");
    crate::test::write_in(&agents, "README.md", "# What these are\n");
    let report = scene.report().await;
    let Verdict::Unsure { what, next } = verdict(&report, "golbat: implementers") else {
        panic!("{:#?}", report.render());
    };
    assert_eq!(
        what,
        &format!(
            "agent file {} is skipped: an agent's name is lowercase letters, digits and `-`",
            agents.join("README.md").display()
        )
    );
    assert!(next.contains("rename the file"), "{next}");
}

#[tokio::test]
async fn an_allow_that_holds_no_implementer_file_makes_the_implementers_line_missing() {
    let scene = Scene::new().await;
    scene.runs("golbat", |t| {
        t["agents"]["allow"] = json!(["defect-hunter"]);
    });
    let (what, fix) = missing(&scene.report().await, "golbat: implementers");
    assert_eq!(
        what,
        "no agent file whose role is `implementer` is one `agents.allow` holds"
    );
    assert!(
        fix.contains("add an implementer to `agents.allow`"),
        "{fix}"
    );
}

#[tokio::test]
async fn a_dog_that_never_named_itself_is_missing_and_told_its_fix() {
    let scene = Scene::new().await;
    scene.shepherd.never_names("kelpie");
    let report = scene.report().await;
    let (what, fix) = missing(&report, "dog");
    assert!(what.contains("silent"), "{what}");
    assert!(fix.contains("`shep bleats kelpie`"), "{fix}");
    assert!(!report.passed());

    scene.shepherd.gives_up_on("kelpie");
    let report = scene.report().await;
    let (what, fix) = missing(&report, "dog");
    assert!(what.contains("given up"), "{what}");
    assert!(fix.contains("`shep restart kelpie`"), "{fix}");
}

#[tokio::test]
async fn a_flock_with_no_dog_is_told_to_enable_it() {
    let scene = Scene::new().await;
    scene.shepherd.stops("kelpie");
    let report = scene.report().await;
    let (what, _) = missing(&report, "dog");
    assert_eq!(what, "kelpie's dog is not running");
}

#[tokio::test]
async fn claude_missing_or_logged_out_is_told_its_fix() {
    let scene = Scene::new().await;
    scene
        .meter
        .fail(MeterError::Spawn("No such file or directory".into()));
    let report = scene.report().await;
    let (what, fix) = missing(&report, "claude");
    assert!(what.contains("No such file or directory"), "{what}");
    assert!(fix.contains("install Claude Code"), "{fix}");
    assert!(!report.passed());

    scene.meter.fail(MeterError::Unreadable(
        "Not logged in · Please run /login\nmore".into(),
    ));
    let (what, fix) = missing(&scene.report().await, "claude");
    assert!(what.contains("Not logged in"), "{what}");
    assert!(!what.contains("more"), "only the first line: {what}");
    assert_eq!(fix, "run `claude`, then `/login`");
}

#[tokio::test]
async fn a_usage_claude_cannot_read_is_unsure_not_a_login_problem() {
    let scene = Scene::new().await;
    scene
        .meter
        .fail(MeterError::Unreadable("no `Current session:` line".into()));
    let report = scene.report().await;
    let Verdict::Unsure { next, .. } = verdict(&report, "claude") else {
        panic!("{:#?}", report.render());
    };
    assert!(!next.contains("/login"), "{next}");
    assert!(report.passed());
}

#[tokio::test]
async fn a_claude_that_does_not_answer_is_unsure_and_does_not_fail_the_run() {
    let scene = Scene::new().await;
    scene.meter.fail(MeterError::TimedOut);
    let report = scene.report().await;
    assert!(matches!(verdict(&report, "claude"), Verdict::Unsure { .. }));
    assert!(report.passed());
}

#[tokio::test]
async fn gh_missing_or_logged_out_is_told_its_fix() {
    let scene = Scene::new().await;
    scene
        .forge
        .set_viewer_down(ForgeError::Spawn("No such file or directory".into()));
    let (what, fix) = missing(&scene.report().await, "gh");
    assert!(what.contains("cannot run `gh`"), "{what}");
    assert!(fix.contains("https://cli.github.com"), "{fix}");

    let stderr = "You are not logged into any GitHub hosts.\nTo log in, run: gh auth login";
    scene
        .forge
        .set_viewer_down(ForgeError::Failed(stderr.into()));
    let (what, fix) = missing(&scene.report().await, "gh");
    assert!(
        what.contains("You are not logged into any GitHub hosts"),
        "{what}"
    );
    assert_eq!(fix, "run `gh auth login`");
}

#[tokio::test]
async fn a_sandbox_the_machine_lacks_names_what_it_lacks() {
    let mut scene = Scene::new().await;
    scene.host.os = "linux";
    scene.host.gaps = vec!["bwrap"];
    let report = scene.report().await;
    let (what, fix) = missing(&report, "sandbox");
    assert!(what.contains("bwrap"), "{what}");
    assert!(fix.contains("install bwrap"), "{fix}");
    assert!(fix.contains("sudo apt-get install bubblewrap"), "{fix}");
    assert!(!report.passed());

    // The macOS gap is `sandbox-exec`, which ships with macOS: no apt line there.
    scene.host.os = "macos";
    scene.host.gaps = vec!["sandbox-exec"];
    let report = scene.report().await;
    let (what, fix) = missing(&report, "sandbox");
    assert!(what.contains("sandbox-exec"), "{what}");
    assert!(
        fix.contains("ships with macOS at /usr/bin/sandbox-exec"),
        "{fix}"
    );
    assert!(!fix.contains("apt-get"), "{fix}");
    assert!(!report.passed());
}

#[tokio::test]
async fn a_shepherd_on_an_earlier_shep_names_both_versions_and_no_project_is_checked() {
    let shepherd = FakeShepherd::on("0.14.7");
    let mut scene = Scene::new().await;
    scene.shepherd = shepherd;

    let report = scene.report().await;

    let (what, _) = missing(&report, "shepherd");
    assert!(
        what.contains("shep 0.14.7") && what.contains(SHEP_VERSION),
        "{what}"
    );
    assert!(matches!(
        verdict(&report, "projects"),
        Verdict::Unsure { .. }
    ));
    assert!(!report.passed());
    assert!(subjects(&report).iter().all(|s| !s.starts_with("koji")));
}

#[tokio::test]
async fn no_shepherd_is_a_missing_shepherd_naming_its_home() {
    let scene = Scene::new().await;
    let elsewhere = tempfile::tempdir().unwrap();
    let dial = scene.shepherd.dial();
    let probes = Probes {
        meter: &scene.meter,
        codex_meter: &|_: &Path| Box::new(scene.codex_meter.clone()) as Box<dyn Meter>,
        forge: &scene.forge,
        github: &scene.app,
        reviewer: &scene.reviewer,
        alerts: &scene.alerts,
        host: &scene.host,
        clock: &scene.clock,
        shepherd: &*dial,
    };
    let here = Here {
        home: &scene.home,
        kelpie_home: &scene.kelpie_home,
        shep_home: elsewhere.path(),
        old_home: None,
    };
    let done = check(elsewhere.path(), probes, here, Ask::default());
    let report = tokio::time::timeout(CALL_PATIENCE, done)
        .await
        .expect("doctor neither ended nor failed in time");
    let (what, fix) = missing(&report, "shepherd");
    assert!(
        what.contains(&elsewhere.path().display().to_string()),
        "{what}"
    );
    assert!(fix.contains("start your shepherd"), "{fix}");
}

#[tokio::test]
async fn labels_a_repo_lacks_are_named_with_the_command_that_makes_them() {
    let scene = Scene::new().await;
    scene.forge.set_repo_labels(&["bug", "ready-for-agent"]);
    let (what, fix) = missing(&scene.report().await, "koji: labels");
    assert_eq!(what, "shep-pm/koji has no ready-for-human, in-progress");
    assert!(fix.starts_with("`shep kelpie add` in "), "{fix}");

    scene.runs("golbat", |t| {
        t["agents"]["reviewers"] = json!(["defect-hunter", "coderabbit"])
    });
    let (what, _) = missing(&scene.report().await, "golbat: labels");
    assert_eq!(
        what,
        "shep-pm/golbat has no ready-for-human, in-progress, review please"
    );
}

#[tokio::test]
async fn a_repo_the_account_cannot_push_to_is_told_how_to_get_access() {
    let scene = Scene::new().await;
    scene.forge.set_can_push(false);
    let report = scene.report().await;
    let (what, fix) = missing(&report, "koji: push access");
    assert!(what.contains("cannot push to shep-pm/koji"), "{what}");
    assert!(fix.contains("write access"), "{fix}");
    assert!(!report.passed());
}

#[tokio::test]
async fn coderabbit_on_a_repo_that_is_not_public_is_missing_its_plan() {
    let scene = Scene::new().await;
    scene.runs("golbat", |t| {
        t["agents"]["reviewers"] = json!(["defect-hunter", "coderabbit"])
    });
    scene.forge.set_visibility(Visibility::Private);
    let (what, fix) = missing(&scene.report().await, "golbat: coderabbit");
    assert!(what.contains("is not public"), "{what}");
    assert!(fix.contains("take `coderabbit` off"), "{fix}");
}

#[tokio::test]
async fn each_listed_review_bot_says_when_it_last_reviewed_on_the_repo() {
    let scene = Scene::new().await;
    scene.runs("golbat", |t| {
        t["agents"]["reviewers"] = json!(["defect-hunter", "coderabbit", "cubic"])
    });
    let report = scene.report().await;
    for (file, name) in [("coderabbit", "CodeRabbit"), ("cubic", "cubic")] {
        assert_eq!(
            ok(&report, &format!("golbat: {file}")),
            format!("{name} last reviewed on shep-pm/golbat at 2025-10-09 08:53 UTC")
        );
    }
}

#[tokio::test]
async fn a_review_bot_that_has_never_reviewed_on_the_repo_is_missing() {
    let scene = Scene::new().await;
    scene.runs("golbat", |t| {
        t["agents"]["reviewers"] = json!(["defect-hunter", "coderabbit"])
    });
    scene.forge.set_last_bot_review(None);
    let report = scene.report().await;
    let (what, fix) = missing(&report, "golbat: coderabbit");
    assert!(what.contains("has never reviewed"), "{what}");
    assert!(fix.contains("install it on shep-pm/golbat"), "{fix}");
    assert!(!report.passed());
}

// Writes kelpie's agent file `name.md` holding `text`, and lists it alone
// as golbat's reviewer.
fn reviewed_by(scene: &Scene, name: &str, text: &str) {
    crate::test::write_in(
        &scene.kelpie_home.join("agents"),
        &format!("{name}.md"),
        text,
    );
    let name = name.to_owned();
    scene.runs("golbat", |t| {
        t["agents"]["reviewers"] = json!([name]);
    });
}

#[tokio::test]
async fn a_local_command_that_is_not_there_is_named() {
    let scene = Scene::new().await;
    let mine = "---\nrole: reviewer\nharness: command\ncommand: ~/no-such-review.sh\n---\n";
    reviewed_by(&scene, "mine", mine);
    let report = scene.report().await;
    let (what, fix) = missing(&report, "golbat: reviewers");
    assert!(
        what.starts_with("mine: ") && what.contains("no-such-review.sh"),
        "{what}"
    );
    assert!(fix.contains("point its agent file"), "{fix}");
    assert!(!report.passed());
}

#[tokio::test]
async fn a_reviewer_with_no_agent_file_is_named() {
    let scene = Scene::new().await;
    scene.runs("golbat", |t| {
        t["agents"]["reviewers"] = json!(["fable"]);
    });
    let (what, fix) = missing(&scene.report().await, "golbat: reviewers");
    assert!(what.contains("fable, which has no agent file"), "{what}");
    assert!(fix.contains("take it off `agents.reviewers`"), "{fix}");
}

#[tokio::test]
async fn a_checkout_that_moved_is_missing_and_fails_the_run() {
    let scene = Scene::new().await;
    std::fs::rename(scene.home.join("koji"), scene.home.join("koji-moved")).unwrap();
    let report = scene.report().await;
    let (what, fix) = missing(&report, "koji: checkout");
    assert!(what.contains("is not a folder"), "{what}");
    assert!(fix.contains("shep kelpie add"), "{fix}");
    assert!(!report.passed());

    let plain = scene.home.join("koji");
    std::fs::create_dir(&plain).unwrap();
    let (what, _) = missing(&scene.report().await, "koji: checkout");
    assert!(what.contains("is not a git work tree"), "{what}");

    // In the right place, but with no `origin`: the checkout stays put.
    git(&plain, &["init", "-q"]);
    let (what, fix) = missing(&scene.report().await, "koji: checkout");
    assert!(
        what.ends_with("has no `origin` remote to cut branches from"),
        "{what}"
    );
    assert!(fix.starts_with("add an `origin` remote"), "{fix}");
}

#[tokio::test]
async fn ci_that_skips_drafts_is_missing_where_ci_blocks_and_not_where_it_does_not() {
    let scene = Scene::new().await;
    scene.runs("golbat", |t| {
        t["ci"]["block"] = json!(false);
    });
    let workflow = "on: pull_request\njobs:\n  test:\n    \
                    if: github.event.pull_request.draft == false\n";
    for project in ["koji", "golbat"] {
        let folder = scene.home.join(project).join(".github/workflows");
        crate::test::write_in(&folder, "ci.yml", workflow);
    }
    let report = scene.report().await;
    let (what, fix) = missing(&report, "koji: ci on drafts");
    assert!(what.contains("skips a draft (`ci.yml`)"), "{what}");
    assert!(fix.contains("set `ci.block` to false"), "{fix}");
    assert!(!report.passed());
    assert!(!subjects(&report).contains(&"golbat: ci on drafts"));
}

#[tokio::test]
async fn ci_that_never_runs_on_a_pull_request_is_missing_where_ci_blocks() {
    let scene = Scene::new().await;
    scene.runs("golbat", |t| {
        t["ci"]["block"] = json!(false);
    });
    let on_main =
        "on:\n  push:\n    branches: [main]\njobs:\n  test:\n    runs-on: ubuntu-latest\n";
    for project in ["koji", "golbat"] {
        let folder = scene.home.join(project).join(".github/workflows");
        crate::test::write_in(&folder, "release.yml", on_main);
    }
    let report = scene.report().await;
    let (what, fix) = missing(&report, "koji: ci on pull requests");
    assert!(
        what.contains("no workflow runs on a pull request"),
        "{what}"
    );
    assert!(!what.contains("  "), "{what}");
    assert!(fix.contains("`pull_request`"), "{fix}");
    assert!(fix.contains("set `ci.block` to false"), "{fix}");
    assert!(!report.passed());
    assert!(!subjects(&report).contains(&"golbat: ci on pull requests"));

    // One workflow that runs on a pull request, or no workflows folder, is nothing to say.
    let koji = scene.home.join("koji").join(".github/workflows");
    crate::test::write_in(&koji, "ci.yml", "on: pull_request\njobs:\n  t:\n");
    assert!(!subjects(&scene.report().await).contains(&"koji: ci on pull requests"));
    std::fs::remove_dir_all(scene.home.join("koji").join(".github")).unwrap();
    assert!(!subjects(&scene.report().await).contains(&"koji: ci on pull requests"));
}

#[tokio::test]
async fn a_repo_that_cannot_be_named_skips_only_the_forge_lines() {
    let scene = Scene::new().await;
    scene.runs("golbat", |t| {
        t["git"].as_object_mut().unwrap().remove("remote");
    });
    let off_github = "https://git.example.invalid/o/r.git";
    git(
        &scene.home.join("golbat"),
        &["remote", "set-url", "origin", off_github],
    );
    let report = scene.report().await;
    let (what, fix) = missing(&report, "golbat: checkout");
    assert!(what.starts_with("setting `git.remote`"), "{what}");
    assert!(fix.contains("set `git.remote`"), "{fix}");
    let subjects = subjects(&report);
    for forge in ["golbat: push access", "golbat: labels"] {
        assert!(!subjects.contains(&forge), "{forge}: {subjects:?}");
    }
    for local in ["golbat: implementers", "golbat: reviewers"] {
        assert!(subjects.contains(&local), "{local}: {subjects:?}");
    }
}

#[tokio::test]
async fn extra_instructions_the_runner_cannot_read_are_missing() {
    let scene = Scene::new().await;
    let file = scene.home.join("rules.md");
    scene.runs("golbat", |t| {
        t["worker"]["instructions_file"] = json!(file);
    });
    let report = scene.report().await;
    let (what, _) = missing(&report, "golbat: instructions");
    assert!(what.contains("worker.instructions_file"), "{what}");
    assert!(!subjects(&report).contains(&"koji: instructions"));
    assert!(!report.passed());

    std::fs::write(&file, "be kind\n").unwrap();
    ok(&scene.report().await, "golbat: instructions");
}

#[tokio::test]
async fn a_project_with_the_code_graph_on_is_missing_a_server_that_is_not_installed() {
    let scene = Scene::new().await;
    scene.runs("golbat", |t| t["code_graph"] = json!({ "enabled": true }));
    let report = scene.report().await;
    let (what, fix) = missing(&report, "golbat: code graph");
    assert!(what.contains("codebase-memory-mcp"), "{what}");
    assert_eq!(
        fix,
        "run `shep kelpie tools install`, or turn `code_graph.enabled` off"
    );
    assert!(
        !subjects(&report).contains(&"koji: code graph"),
        "off is not checked"
    );

    let server = Tools::under(&scene.kelpie_home).code_graph();
    std::fs::create_dir_all(server.parent().unwrap()).unwrap();
    std::fs::write(server, "").unwrap();
    ok(&scene.report().await, "golbat: code graph");
}

#[tokio::test]
async fn a_local_endpoint_nothing_answers_on_is_named() {
    let scene = Scene::new().await;
    let endpoint = format!(
        "---\nrole: reviewer\nharness: endpoint\nurl: {}\nmodel: qwen\ncontext: 8192\n---\n",
        unreachable_url()
    );
    reviewed_by(&scene, "gpu-box", &endpoint);
    let (what, _) = missing(&scene.report().await, "golbat: reviewers");
    assert!(what.contains("127.0.0.1:1"), "{what}");
}

#[tokio::test]
async fn a_review_list_that_can_run_names_its_reviewers_in_order() {
    let scene = Scene::new().await;
    let script = scene.home.join("review.sh");
    crate::test::write_script(&script, "#!/bin/sh\nexit 0\n");
    let mine = format!(
        "---\nrole: reviewer\nharness: command\ncommand: {}\n---\n",
        script.display()
    );
    crate::test::write_in(&scene.kelpie_home.join("agents"), "mine.md", &mine);
    scene.runs("golbat", |t| {
        t["agents"]["reviewers"] = json!(["mine", "defect-hunter"]);
    });
    let report = scene.report().await;
    assert_eq!(
        ok(&report, "koji: reviewers"),
        "each pull request is read by defect-hunter"
    );
    assert_eq!(
        ok(&report, "golbat: reviewers"),
        "each pull request is read by mine, then defect-hunter"
    );
}

#[tokio::test]
async fn a_named_project_is_the_only_one_checked() {
    let scene = Scene::new().await;
    scene.runs("golbat", |_| {});
    let golbat = ProjectName::try_from("golbat").unwrap();
    let ask = Ask {
        project: Some(&golbat),
        ..Ask::default()
    };
    let report = scene.check(ask).await;
    assert!(subjects(&report).contains(&"golbat: labels"));
    assert!(subjects(&report).iter().all(|s| !s.starts_with("koji")));

    let unknown = ProjectName::try_from("xilriws").unwrap();
    let ask = Ask {
        project: Some(&unknown),
        ..Ask::default()
    };
    let report = scene.check(ask).await;
    let (what, fix) = missing(&report, "xilriws");
    assert_eq!(what, "no kelpie runner has this name");
    assert!(fix.contains("shep kelpie add"), "{fix}");
}

#[tokio::test]
async fn a_table_that_does_not_load_is_the_project_s_one_line() {
    let scene = Scene::new().await;
    scene.runs("golbat", |t| {
        t.insert("misspelt".into(), json!(true));
    });
    let report = scene.report().await;
    let (what, fix) = missing(&report, "golbat: settings");
    assert!(what.contains("misspelt"), "{what}");
    assert!(fix.contains("[app.dogs.kelpie]"), "{fix}");
    assert!(subjects(&report).iter().all(|s| *s != "golbat: labels"));
    assert!(subjects(&report).contains(&"koji: labels"));
}

#[test]
fn the_last_line_counts_what_is_missing() {
    let line = |verdict| Line {
        subject: "x".into(),
        verdict,
    };
    let gone = || Verdict::Missing {
        what: "w".into(),
        fix: "f".into(),
    };
    let report = Report {
        lines: vec![line(gone())],
    };
    assert_eq!(report.render()[0], "MISSING  x: w. Fix: f");
    assert_eq!(report.render()[1], "1 thing a project needs is missing");
    let report = Report {
        lines: vec![line(gone()), line(gone())],
    };
    assert_eq!(report.render()[2], "2 things a project needs are missing");
}

#[test]
fn arguments_are_a_project_and_the_test_alert_flag_in_either_order() {
    let args = |a: &[&str]| read_args(&a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>());
    assert_eq!(args(&[]), Ok((None, false)));
    assert_eq!(args(&["--test-alert"]), Ok((None, true)));
    let (project, alert) = args(&["koji", "--test-alert"]).unwrap();
    assert_eq!((project.unwrap().as_str(), alert), ("koji", true));
    let (project, alert) = args(&["--test-alert", "koji"]).unwrap();
    assert_eq!((project.unwrap().as_str(), alert), ("koji", true));
    for bad in [&["--fix"][..], &["a", "b"], &["Not A Name"]] {
        assert!(args(bad).is_err(), "{bad:?}");
    }
}

#[tokio::test]
async fn a_project_folder_left_in_the_old_home_is_missing_for_that_project_alone() {
    let scene = Scene::new().await;
    scene.runs("golbat", |_| {});
    assert!(!subjects(&scene.report().await).contains(&"koji: old home"));
    let left = scene.old_home.join("projects/koji");
    std::fs::create_dir_all(&left).unwrap();

    let report = scene.report().await;

    let (what, fix) = missing(&report, "koji: old home");
    assert!(what.contains(&left.display().to_string()), "{what}");
    assert!(what.contains("move "), "{what}");
    assert!(fix.contains("shep kelpie start koji"), "{fix}");
    assert!(!subjects(&report).contains(&"golbat: old home"));
    assert!(!report.passed());
}

mod base;
mod browser;
mod e2e;
mod github;
mod harnesses;
mod implementers;
mod plugins;
mod pm_decides;
mod registries;
mod reviewers;
mod sandbox;
mod template;
mod unbound;
mod webhook;
