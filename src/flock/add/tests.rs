use std::path::PathBuf;
use std::time::Duration;

use shep_client::shep_core::config::AppConfig;

use super::*;
use crate::ports::Visibility;
use crate::runner::ProjectPaths;
use crate::settings::{AgentName, ForgeSlug};
use crate::shepherd;
use crate::test::{FakeForge, FakeShepherd};

// Bounds every call against a fake shepherd, so a hang fails by name.
const PATIENCE: Duration = Duration::from_secs(10);
const EXAMPLE: &str = include_str!("../../../settings.example.toml");

/// A scratch checkout of a private repo with CI, a home with no qwen
/// script, a forge with one label of its own, and a flock holding only the
/// adopted kelpie, running with its channel
struct Scene {
    shepherd: FakeShepherd,
    forge: FakeForge,
    checkout: Checkout,
    home: PathBuf,
    launch: Launch,
    name: ProjectName,
}

impl Scene {
    async fn new() -> Self {
        let shepherd = FakeShepherd::new().await;
        shepherd.holds_dog("kelpie", true);
        let root = shepherd.scratch("koji");
        std::fs::create_dir_all(root.join(".github/workflows")).unwrap();
        let forge = FakeForge::new(PathBuf::from("/nowhere"));
        forge.set_visibility(Visibility::Private);
        forge.set_repo_labels(&["bug"]);
        let launch = Launch {
            kelpie: "/opt/kelpie".into(),
            shep_home: shepherd.home().to_owned(),
            kelpie_home: None,
        };
        Self {
            home: shepherd.scratch("home"),
            checkout: Checkout {
                root,
                forge: ForgeSlug::try_from("shep-pm/koji-website".to_owned()).unwrap(),
            },
            shepherd,
            forge,
            launch,
            name: ProjectName::try_from("koji").unwrap(),
        }
    }

    fn agents(&self) -> PathBuf {
        self.home.join(".shep/kelpie/agents")
    }

    fn old_settings(&self) -> PathBuf {
        self.home
            .join(".kelpie/projects")
            .join(self.name.as_str())
            .join("settings.toml")
    }

    async fn add(&self) -> Result<Vec<String>, String> {
        let client = shepherd::connect(self.shepherd.home()).await.unwrap();
        let (old, agents) = (self.old_settings(), self.agents());
        let place = Place {
            checkout: &self.checkout,
            home: &self.home,
            old_settings: &old,
            agents: &agents,
        };
        let added = add(&client, &self.forge, &self.launch, &self.name, place);
        tokio::time::timeout(PATIENCE, added)
            .await
            .expect("add neither ended nor failed in time")
    }

    /// The runner's table, read back as the runner reads it
    fn settings(&self) -> Settings {
        let (runner, _) = self.shepherd.sheep("koji").expect("a runner");
        let table = runner.dogs.get(DOG).expect("a kelpie table").as_map();
        Settings::from_table(table, "koji", &self.home, &self.home).unwrap()
    }
}

// Real sockets under a real clock: a paused one would time out the
// handshake while the fake's socket is merely waiting.
#[tokio::test]
async fn add_in_a_scratch_repo_writes_the_table_makes_the_labels_and_adds_the_runner() {
    let mut scene = Scene::new().await;
    scene.add().await.unwrap();

    let writes = scene.shepherd.writes();
    let [Request::Add { apps: runner }] = writes.as_slice() else {
        panic!("{writes:?}");
    };
    assert_eq!(runner[0].args, ["runner", "koji"]);
    let settings = scene.settings();
    assert_eq!(settings.repo, scene.checkout.root);
    assert_eq!(settings.forge.as_str(), "shep-pm/koji-website");
    assert!(settings.ci, "the checkout has workflows");
    let reviewers = settings.agents.reviewers.clone().unwrap();
    assert_eq!(
        reviewers,
        [AgentName::try_from("defect-hunter".to_owned()).unwrap()]
    );
    let listed: Vec<&str> = (settings.agents.implementers.iter())
        .map(|n| n.as_str())
        .collect();
    assert_eq!(listed, ["sonnet-high"]);
    let sonnet = std::fs::read_to_string(scene.agents().join("sonnet-high.md")).unwrap();
    assert_eq!(sonnet, include_str!("../../../agents/sonnet-high.md"));
    let opus = std::fs::read_to_string(scene.agents().join("opus-high.md")).unwrap();
    assert_eq!(opus, include_str!("../../../agents/opus-high.md"));
    let hunter = std::fs::read_to_string(scene.agents().join("defect-hunter.md")).unwrap();
    assert_eq!(hunter, include_str!("../../../agents/defect-hunter.md"));
    assert!(
        !scene.agents().join("qwen.md").exists(),
        "no qwen-review script, so no qwen reviewer"
    );
    for (bot, text) in [
        ("coderabbit", include_str!("../../../agents/coderabbit.md")),
        ("cubic", include_str!("../../../agents/cubic.md")),
        ("codex", include_str!("../../../agents/codex.md")),
    ] {
        let written = std::fs::read_to_string(scene.agents().join(format!("{bot}.md")));
        assert_eq!(
            written.unwrap(),
            text,
            "written, though no project lists it"
        );
    }
    assert_eq!(
        scene.forge.repo_labels_now(),
        [
            "bug",
            "ready-for-agent",
            "ready-for-human",
            "in-progress",
            "review please"
        ]
    );
    let (_, running) = scene.shepherd.sheep("koji").unwrap();
    assert!(!running, "add starts nothing");
}

#[tokio::test]
async fn where_the_qwen_review_script_is_installed_add_writes_qwen_and_lists_it_first() {
    let scene = Scene::new().await;
    let script = scene.home.join(".claude/scripts/qwen-review.sh");
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    crate::test::write_script(&script, "#!/bin/sh\nexit 0\n");
    scene.add().await.unwrap();

    let listed: Vec<String> = (scene.settings().agents.reviewers.unwrap().iter())
        .map(|n| n.as_str().to_owned())
        .collect();
    assert_eq!(listed, ["qwen", "defect-hunter"]);
    let qwen = std::fs::read_to_string(scene.agents().join("qwen.md")).unwrap();
    assert_eq!(qwen, include_str!("../../../agents/qwen.md"));
}

// The runner works its home out from the `SHEP_HOME` its entry carries.
#[tokio::test]
async fn add_from_a_checkout_puts_its_worktrees_under_the_project_s_folder() {
    let scene = Scene::new().await;
    scene.add().await.unwrap();

    let (runner, _) = scene.shepherd.sheep("koji").unwrap();
    let shep_home = scene.shepherd.home();
    assert_eq!(runner.env["SHEP_HOME"], shep_home.display().to_string());
    assert!(!runner.env.contains_key("KELPIE_HOME"), "{:?}", runner.env);
    assert_eq!(scene.settings().repo, scene.checkout.root);
    let paths = ProjectPaths::under(&crate::home::under(shep_home), shep_home, &scene.name);
    assert_eq!(paths.worktree(7), shep_home.join("kelpie/koji/worktrees/7"));
    assert_eq!(paths.build(7), shep_home.join("kelpie/koji/builds/7"));
    assert_eq!(paths.state, shep_home.join("kelpie/koji/state.json"));
}

#[tokio::test]
async fn add_twice_changes_nothing() {
    let mut scene = Scene::new().await;
    scene.add().await.unwrap();
    // Reading the writes takes them, so only the second add's are left.
    scene.shepherd.writes();
    let before = scene.shepherd.sheep("koji");

    let mine = "---\nrole: implementer\nharness: claude-code\nmodel: claude-sonnet-6\n\
                effort: high\n---\n";
    std::fs::write(scene.agents().join("sonnet-high.md"), mine).unwrap();

    let lines = scene.add().await.unwrap();
    assert_eq!(scene.shepherd.writes(), []);
    let kept = std::fs::read_to_string(scene.agents().join("sonnet-high.md")).unwrap();
    assert_eq!(kept, mine, "add never writes over an agent file");
    assert_eq!(
        scene.forge.repo_labels_now(),
        [
            "bug",
            "ready-for-agent",
            "ready-for-human",
            "in-progress",
            "review please"
        ]
    );
    assert_eq!(scene.shepherd.sheep("koji"), before);
    // Four labels, kelpie's agent files and the runner, each already there.
    assert_eq!(lines.len(), 6, "{lines:?}");
    assert!(lines.iter().all(|l| l.contains("already")), "{lines:?}");
}

#[tokio::test]
async fn a_public_repo_with_no_ci_gets_no_ci_and_still_lists_no_review_bot() {
    let scene = Scene::new().await;
    scene.forge.set_visibility(Visibility::Public);
    std::fs::remove_dir_all(scene.checkout.root.join(".github")).unwrap();
    scene.add().await.unwrap();
    let settings = scene.settings();
    assert!(!settings.ci);
    let reviewers = settings.agents.reviewers.unwrap();
    assert_eq!(reviewers, [AgentName::kelpies("defect-hunter")]);
}

#[tokio::test]
async fn another_default_branch_is_refused_before_anything_changes() {
    let mut scene = Scene::new().await;
    scene.forge.set_default_branch("trunk");
    let err = scene.add().await.unwrap_err();
    assert!(err.contains("default branch is trunk"), "{err}");
    assert_eq!(scene.shepherd.writes(), []);
    assert_eq!(scene.forge.repo_labels_now(), ["bug"]);
}

#[tokio::test]
async fn a_sheep_that_is_not_kelpie_s_keeps_its_name() {
    let mut scene = Scene::new().await;
    scene
        .shepherd
        .holds(AppConfig::minimal("koji", "/srv/web"), true);
    let err = scene.add().await.unwrap_err();
    assert!(err.contains("`koji` is already in this flock"), "{err}");
    assert_eq!(scene.shepherd.writes(), []);
    assert_eq!(scene.forge.repo_labels_now(), ["bug"]);
}

#[tokio::test]
async fn an_enabled_dog_that_holds_the_project_s_name_is_not_kelpie_s() {
    let scene = Scene::new().await;
    scene.shepherd.holds_dog("koji", false);
    let err = scene.add().await.unwrap_err();
    assert!(err.contains("is not kelpie's"), "{err}");
}

#[tokio::test]
async fn a_flockfile_runner_or_a_broken_table_for_this_checkout_is_refused() {
    let mut scene = Scene::new().await;
    let root = scene.checkout.root.display().to_string();
    // A Flockfile runner with no table, whose old file names this checkout.
    let mut flockfile = scene
        .launch
        .runner(&ProjectName::try_from("old").unwrap(), Map::new());
    flockfile.dogs.clear();
    scene.shepherd.holds(flockfile, true);
    let file = scene.home.join(".kelpie/projects/old/settings.toml");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, format!("repo = {root:?}\n")).unwrap();
    let err = scene.add().await.unwrap_err();
    assert!(err.starts_with("project `old` already runs"), "{err}");
    assert_eq!(scene.shepherd.writes(), []);

    // A table that no longer parses still names its checkout.
    let mut scene = Scene::new().await;
    let mut broken = Map::new();
    broken.insert(
        "repo".into(),
        Value::String(scene.checkout.root.display().to_string()),
    );
    scene.shepherd.holds(
        scene
            .launch
            .runner(&ProjectName::try_from("broken").unwrap(), broken),
        true,
    );
    let err = scene.add().await.unwrap_err();
    assert!(err.starts_with("project `broken` already runs"), "{err}");
    assert_eq!(scene.shepherd.writes(), []);
}

#[tokio::test]
async fn a_checkout_or_repo_another_project_runs_is_refused() {
    let mut scene = Scene::new().await;
    scene.add().await.unwrap();
    scene.shepherd.writes();
    scene.name = ProjectName::try_from("koji-again").unwrap();
    let err = scene.add().await.unwrap_err();
    assert!(err.starts_with("project `koji` already runs"), "{err}");
    assert_eq!(scene.shepherd.writes(), []);
}

#[tokio::test]
async fn a_project_s_file_from_before_the_tables_becomes_its_table() {
    let scene = Scene::new().await;
    let mut table = crate::test::project_table(EXAMPLE);
    let root = scene.checkout.root.display().to_string();
    table.insert("repo".into(), Value::String(root));
    table.insert(
        "forge".into(),
        Value::String(scene.checkout.forge.as_str().into()),
    );
    table.insert("merge_authority".into(), Value::String("auto".into()));
    let file = scene.old_settings();
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, toml::to_string(&table).unwrap()).unwrap();

    scene.add().await.unwrap();
    let (runner, _) = scene.shepherd.sheep("koji").unwrap();
    assert_eq!(runner.dogs.get(DOG).unwrap().as_map(), &table);
}

#[tokio::test]
async fn a_file_for_another_checkout_is_refused() {
    let mut scene = Scene::new().await;
    let file = scene.old_settings();
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        toml::to_string(&crate::test::project_table(EXAMPLE)).unwrap(),
    )
    .unwrap();
    let err = scene.add().await.unwrap_err();
    assert!(err.contains("not this checkout"), "{err}");
    assert_eq!(scene.shepherd.writes(), []);
}

#[tokio::test]
async fn a_project_cannot_take_kelpie_s_own_name() {
    let mut scene = Scene::new().await;
    scene.name = ProjectName::try_from("kelpie").unwrap();
    let err = scene.add().await.unwrap_err();
    assert!(err.contains("kelpie's own name"), "{err}");
    assert_eq!(scene.shepherd.writes(), []);
}

// Its runner would never be granted a lease, so `add` says how to start it.
#[tokio::test]
async fn add_says_when_kelpie_s_dog_is_not_enabled() {
    let mut scene = Scene::new().await;
    scene.shepherd = FakeShepherd::new().await;
    let lines = scene.add().await.unwrap();
    assert_eq!(
        lines.last().unwrap(),
        "kelpie's dog is not enabled: `shep enable kelpie` runs it"
    );
    let writes = scene.shepherd.writes();
    assert!(
        matches!(writes.as_slice(), [Request::Add { apps }] if apps[0].name == "koji"),
        "{writes:?}"
    );
}

#[tokio::test]
async fn add_says_how_to_give_an_old_adoption_the_channel() {
    let mut scene = Scene::new().await;
    scene.shepherd = FakeShepherd::new().await;
    scene.shepherd.holds_dog("kelpie", false);
    let lines = scene.add().await.unwrap();
    let last = lines.last().unwrap();
    assert!(
        last.ends_with(
            "run `shep adopt /opt/kelpie --name kelpie`, then `shep disable kelpie` and \
             `shep enable kelpie`"
        ),
        "{last}"
    );
}

#[tokio::test]
async fn a_runner_from_a_flockfile_with_no_table_is_given_one() {
    let mut scene = Scene::new().await;
    let mut runner = scene.launch.runner(&scene.name, Map::new());
    runner.dogs.clear();
    scene.shepherd.holds(runner, true);
    let lines = scene.add().await.unwrap();
    assert!(
        lines
            .iter()
            .any(|l| l.ends_with("already there, and given its settings")),
        "{lines:?}"
    );
    assert_eq!(scene.settings().repo, scene.checkout.root);
    let writes = scene.shepherd.writes();
    let added_runner = |w: &Request| matches!(w, Request::Add { apps } if apps[0].name == "koji");
    assert!(!writes.iter().any(added_runner), "{writes:?}");
}
