use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::*;
use crate::flock::Launch;
use crate::ports::{ForgeError, MeterError, Visibility};
use crate::shepherd::SHEP_VERSION;
use crate::test::{
    FakeAlerts, FakeClock, FakeForge, FakeMeter, FakeReviewer, FakeShepherd, git, project_table,
    unreachable_url,
};
use crate::tools::Tools;

// Bounds every call against a fake shepherd, so a hang fails by name.
const PATIENCE: Duration = Duration::from_secs(10);
const EXAMPLE: &str = include_str!("../../settings.example.toml");

// A URL no test would print by chance, so finding it in a report is a leak.
const SECRET: &str = "https://ntfy.example.invalid/kelpie-s3cr3t-topic";
const WEBHOOK: &str =
    "[webhook]\nkind = \"ntfy\"\nurl = \"https://ntfy.example.invalid/kelpie-s3cr3t-topic\"\n";

#[derive(Debug)]
struct FakeHost(Vec<&'static str>);

impl Host for FakeHost {
    fn sandbox_gaps(&self) -> Vec<&'static str> {
        self.0.clone()
    }
}

/// A shepherd holding one project, `koji`, on a machine with every piece in
/// place: a public repo with kelpie's labels, a logged-in `claude` and `gh`,
/// a sandbox, a webhook, the local round off and no review bot listed
struct Scene {
    shepherd: FakeShepherd,
    forge: FakeForge,
    meter: FakeMeter,
    codex_meter: FakeMeter,
    reviewer: FakeReviewer,
    alerts: FakeAlerts,
    host: FakeHost,
    clock: FakeClock,
    home: PathBuf,
    kelpie_home: PathBuf,
}

impl Scene {
    async fn new() -> Self {
        let shepherd = FakeShepherd::new().await;
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
        let scene = Self {
            home: shepherd.scratch("home"),
            kelpie_home: shepherd.scratch("kelpie"),
            forge,
            meter: FakeMeter::idle(),
            codex_meter: FakeMeter::idle(),
            reviewer: FakeReviewer::default(),
            alerts: FakeAlerts::on(clock.clone()),
            host: FakeHost(Vec::new()),
            clock,
            shepherd,
        };
        let srt = Tools::under(&scene.kelpie_home).sandbox();
        std::fs::create_dir_all(srt.parent().unwrap()).unwrap();
        std::fs::write(srt, "").unwrap();
        scene.runs("koji", |_| {});
        scene
    }

    /// Puts a runner named `name` in the flock, with the default table as `edit` changes it
    fn runs(&self, name: &str, edit: impl FnOnce(&mut Map<String, Value>)) {
        let mut table = project_table(EXAMPLE);
        let repo = self.home.join(name);
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(
            &repo,
            &["remote", "add", "origin", "https://github.com/o/r.git"],
        );
        table.insert("repo".into(), json!(repo));
        table.insert("forge".into(), json!(format!("shep-pm/{name}")));
        table["agents"]["reviewers"] = json!(["defect-hunter"]);
        edit(&mut table);
        let launch = Launch {
            kelpie: "/opt/kelpie".into(),
            shep_home: self.shepherd.home().to_owned(),
            kelpie_home: None,
        };
        let name = ProjectName::try_from(name).unwrap();
        self.shepherd.holds(launch.runner(&name, table), false);
    }

    async fn check(&self, ask: Ask<'_>) -> Report {
        let probes = Probes {
            meter: &self.meter,
            codex_meter: &|_: &Path| Box::new(self.codex_meter.clone()) as Box<dyn Meter>,
            forge: &self.forge,
            reviewer: &self.reviewer,
            review_bot: &CodeRabbit,
            alerts: &self.alerts,
            host: &self.host,
            clock: &self.clock,
        };
        let here = Here {
            home: &self.home,
            kelpie_home: &self.kelpie_home,
            shep_home: self.shepherd.home(),
        };
        let done = check(self.shepherd.home(), probes, here, ask);
        tokio::time::timeout(PATIENCE, done)
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

#[tokio::test]
async fn a_project_that_spends_codex_checks_codex_answers_its_usage() {
    let scene = Scene::new().await;
    let codex = "---\nrole: implementer\nharness: stand-in\nmodel: gpt-5-codex\n\
                 effort: medium\nusage: codex\n---\n";
    crate::test::write_in(&scene.kelpie_home.join("agents"), "codex.md", codex);
    scene.runs("golbat", |t| {
        t.insert(
            "agents".into(),
            json!({ "implementers": ["sonnet-high", "codex"] }),
        );
    });
    let report = scene.report().await;
    let found = ok(&report, "golbat: codex usage");
    assert_eq!(found, "reads Codex usage: 5-hour window 0%, week 0%");
    assert!(!subjects(&report).contains(&"koji: codex usage"));

    let fixes = [
        (
            "cannot run codex: No such file or directory",
            "install the Codex CLI",
        ),
        ("codex refused: 402 Payment Required", "codex login"),
    ];
    for (reason, fix) in fixes {
        scene.codex_meter.fail(MeterError::Codex(reason.into()));
        let (what, said) = missing(&scene.report().await, "golbat: codex usage");
        assert_eq!(what, reason);
        assert!(said.contains(fix), "{said}");
    }
    scene
        .codex_meter
        .fail(MeterError::Codex("codex did not answer in time".into()));
    let report = scene.report().await;
    assert!(matches!(
        verdict(&report, "golbat: codex usage"),
        Verdict::Unsure { .. }
    ));
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
            "golbat: implementers",
            "golbat: push access",
            "golbat: labels",
            "golbat: coderabbit",
            "golbat: reviewers",
            "golbat: rulings",
            "koji: checkout",
            "koji: implementers",
            "koji: push access",
            "koji: labels",
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
        "an issue with no `agent:` label runs on sonnet-high"
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
    assert!(next.contains("rename it"), "{next}");
}

#[tokio::test]
async fn an_agent_file_that_cannot_be_used_or_an_implementer_without_one_is_missing() {
    let scene = Scene::new().await;
    scene.runs("golbat", |t| {
        t.insert("agents".into(), json!({ "implementers": ["fable"] }));
    });
    let (what, fix) = missing(&scene.report().await, "golbat: implementers");
    assert!(
        what.contains("names fable, which has no agent file"),
        "{what}"
    );
    assert!(fix.contains("write the agent file"), "{fix}");

    let agents = scene.kelpie_home.join("agents");
    crate::test::write_in(&agents, "fable.md", "role: implementer\n");
    let (what, fix) = missing(&scene.report().await, "golbat: implementers");
    let file = agents.join("fable.md");
    assert!(
        what.starts_with(&format!("agent file {}: ", file.display())),
        "{what}"
    );
    assert!(fix.contains("correct the file"), "{fix}");
}

// Shep lists a dog that is up and never named itself as silent, then gives
// up on it, and a dog in either state holds no lease.
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
    scene.host = FakeHost(vec!["bwrap", "socat"]);
    let report = scene.report().await;
    let (what, fix) = missing(&report, "sandbox");
    assert!(what.contains("bwrap and socat"), "{what}");
    assert!(fix.contains("bubblewrap"), "{fix}");
    assert!(!report.passed());
}

#[tokio::test]
async fn a_machine_without_the_sandbox_runtime_is_told_to_install_it() {
    let scene = Scene::new().await;
    std::fs::remove_file(Tools::under(&scene.kelpie_home).sandbox()).unwrap();
    let report = scene.report().await;
    let (what, fix) = missing(&report, "sandbox");
    assert!(what.contains("no runner starts"), "{what}");
    assert_eq!(fix, "run `shep kelpie tools install`");
    assert!(!report.passed());
}

#[tokio::test]
async fn a_shepherd_on_another_minor_names_both_versions_and_no_project_is_checked() {
    let shepherd = FakeShepherd::on("0.13.0").await;
    let mut scene = Scene::new().await;
    scene.shepherd = shepherd;

    let report = scene.report().await;

    let (what, _) = missing(&report, "shepherd");
    assert!(
        what.contains("shep 0.13.0") && what.contains(SHEP_VERSION),
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
    let probes = Probes {
        meter: &scene.meter,
        codex_meter: &|_: &Path| Box::new(scene.codex_meter.clone()) as Box<dyn Meter>,
        forge: &scene.forge,
        reviewer: &scene.reviewer,
        review_bot: &CodeRabbit,
        alerts: &scene.alerts,
        host: &scene.host,
        clock: &scene.clock,
    };
    let here = Here {
        home: &scene.home,
        kelpie_home: &scene.kelpie_home,
        shep_home: elsewhere.path(),
    };
    let done = check(elsewhere.path(), probes, here, Ask::default());
    let report = tokio::time::timeout(PATIENCE, done)
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
async fn coderabbit_that_has_never_commented_is_unsure_and_does_not_fail_the_run() {
    let scene = Scene::new().await;
    scene.runs("golbat", |t| {
        t["agents"]["reviewers"] = json!(["defect-hunter", "coderabbit"])
    });
    scene.forge.set_review_bot_seen(false);
    let report = scene.report().await;
    let Verdict::Unsure { what, next } = verdict(&report, "golbat: coderabbit") else {
        panic!("{:#?}", report.render());
    };
    assert!(what.contains("has not commented"), "{what}");
    assert!(next.contains("install it on shep-pm/golbat"), "{next}");
    assert!(report.passed());
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
async fn rulings_with_no_webhook_are_fine_and_say_where_they_appear() {
    let scene = Scene::new().await;
    scene.shepherd.holds_section("");
    let found = ok(&scene.report().await, "koji: rulings");
    assert!(found.contains("only in the log"), "{found}");
    assert!(found.contains("shep kelpie rule"), "{found}");
}

#[tokio::test]
async fn an_old_settings_file_with_no_table_or_section_is_missing_and_named() {
    let scene = Scene::new().await;
    scene.shepherd.holds_section("");
    let project = scene.kelpie_home.join("golbat");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("settings.toml"), "forge = \"a/b\"\n").unwrap();
    std::fs::write(scene.kelpie_home.join("settings.toml"), WEBHOOK).unwrap();
    let report = scene.report().await;

    let (what, fix) = missing(&report, "golbat: settings");
    assert_eq!(
        what,
        format!(
            "there is no [app.dogs.kelpie] table on the golbat sheep, and kelpie no longer \
             reads {}: move its keys into the table",
            project.join("settings.toml").display()
        )
    );
    assert!(fix.contains("`[app.dogs.kelpie]` table"), "{fix}");
    let (what, _) = missing(&report, "kelpie settings");
    assert!(
        what.contains("kelpie no longer reads") && what.ends_with("move its keys into the section"),
        "{what}"
    );
    assert!(!report.passed());

    // Where the table and the section are set, the old files are not mentioned.
    scene.shepherd.holds_section(WEBHOOK);
    std::fs::remove_dir_all(&project).unwrap();
    let report = scene.report().await;
    assert!(!subjects(&report).contains(&"golbat: settings"));
    assert!(!subjects(&report).contains(&"kelpie settings"));
}

#[tokio::test]
async fn a_test_alert_posts_once_to_the_webhook_and_only_when_asked() {
    let scene = Scene::new().await;
    scene.report().await;
    assert_eq!(scene.alerts.posts(), []);

    let ask = Ask {
        test_alert: true,
        ..Ask::default()
    };
    let report = scene.check(ask).await;

    assert!(ok(&report, "test alert").starts_with("posted"));
    let posts = scene.alerts.posts();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0].0.url.expose(), SECRET);
    assert_eq!(posts[0].1.title, "kelpie doctor");
    assert!(posts[0].1.reply.is_none());
    let printed = report.render().join("\n");
    assert!(!printed.contains("s3cr3t"), "{printed}");
}

#[tokio::test]
async fn a_test_alert_the_webhook_refuses_is_missing_and_never_prints_its_url() {
    let scene = Scene::new().await;
    scene.alerts.set_down(true);
    let ask = Ask {
        test_alert: true,
        ..Ask::default()
    };
    let report = scene.check(ask).await;
    let (what, fix) = missing(&report, "test alert");
    assert!(what.contains("HTTP 503"), "{what}");
    assert!(fix.contains("webhook.url"), "{fix}");
    assert!(!report.render().join("\n").contains("s3cr3t"));
    assert!(!report.passed());
}

#[tokio::test]
async fn a_test_alert_with_no_webhook_says_there_is_none_and_posts_nothing() {
    let scene = Scene::new().await;
    scene.shepherd.holds_section("");
    let ask = Ask {
        test_alert: true,
        ..Ask::default()
    };
    let report = scene.check(ask).await;
    let Verdict::Unsure { what, .. } = verdict(&report, "test alert") else {
        panic!("{:#?}", report.render());
    };
    assert_eq!(what, "there is no webhook to post to");
    assert_eq!(scene.alerts.posts(), []);
}

#[tokio::test]
async fn kelpie_settings_that_do_not_parse_are_missing_and_never_quote_the_url() {
    let scene = Scene::new().await;
    scene
        .shepherd
        .holds_section(&format!("{WEBHOOK}oops = 1\n"));
    let report = scene.report().await;
    let (what, _) = missing(&report, "kelpie settings");
    assert!(!what.contains("s3cr3t"), "{what}");
    assert!(subjects(&report).contains(&"koji: labels"));
    assert!(!subjects(&report).contains(&"koji: rulings"));
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
