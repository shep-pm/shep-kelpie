use std::path::PathBuf;
use std::time::Duration;

use shep_client::shep_core::config::AppConfig;

use super::*;
use crate::settings::{ForgeSlug, LocalRound};
use crate::shepherd;
use crate::test::{FakeForge, FakeShepherd};

// Bounds every call against a fake shepherd, so a hang fails by name.
const PATIENCE: Duration = Duration::from_secs(10);
const EXAMPLE: &str = include_str!("../../../settings.example.toml");

/// A scratch checkout of a private repo with CI, a home with no qwen
/// script, a forge with one label of its own, and an empty flock
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
        let root = shepherd.scratch("hazels-lab");
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
                forge: ForgeSlug::try_from("Hazels-Lab/hazels-lab-website".to_owned()).unwrap(),
            },
            shepherd,
            forge,
            launch,
            name: ProjectName::try_from("hazels-lab").unwrap(),
        }
    }

    fn old_settings(&self) -> PathBuf {
        self.home
            .join(".kelpie/projects")
            .join(self.name.as_str())
            .join("settings.toml")
    }

    async fn add(&self) -> Result<Vec<String>, String> {
        let client = shepherd::connect(self.shepherd.home()).await.unwrap();
        let old = self.old_settings();
        let place = Place {
            checkout: &self.checkout,
            home: &self.home,
            old_settings: &old,
        };
        let added = add(&client, &self.forge, &self.launch, &self.name, place);
        tokio::time::timeout(PATIENCE, added)
            .await
            .expect("add neither ended nor failed in time")
    }

    /// The runner's table, read back as the runner reads it
    fn settings(&self) -> Settings {
        let (runner, _) = self.shepherd.sheep("hazels-lab").expect("a runner");
        let table = runner.dogs.get(DOG).expect("a kelpie table").as_map();
        Settings::from_table(table, "hazels-lab", &self.home, &self.home).unwrap()
    }
}

// Real sockets under a real clock: a paused one would time out the
// handshake while the fake's socket is merely waiting.
#[tokio::test]
async fn add_in_a_scratch_repo_writes_the_table_makes_the_labels_and_adds_the_runner() {
    let mut scene = Scene::new().await;
    scene.add().await.unwrap();

    let writes = scene.shepherd.writes();
    let [Request::Add { apps: runner }, Request::Add { apps: dog }] = writes.as_slice() else {
        panic!("{writes:?}");
    };
    assert_eq!(runner[0].args, ["runner", "hazels-lab"]);
    assert_eq!(dog[0].name, "kelpie-dog");
    let settings = scene.settings();
    assert_eq!(settings.repo, scene.checkout.root);
    assert_eq!(settings.forge.as_str(), "Hazels-Lab/hazels-lab-website");
    assert!(settings.ci, "the checkout has workflows");
    assert!(!settings.coderabbit.enabled, "the repo is private");
    assert_eq!(settings.review.local, LocalRound::Off {});
    assert_eq!(
        scene.forge.repo_labels_now(),
        ["bug", "ready-for-agent", "ready-for-human", "review please"]
    );
    let (_, running) = scene.shepherd.sheep("hazels-lab").unwrap();
    assert!(!running, "add starts nothing");
}

#[tokio::test]
async fn add_twice_changes_nothing() {
    let mut scene = Scene::new().await;
    scene.add().await.unwrap();
    // Reading the writes takes them, so only the second add's are left.
    scene.shepherd.writes();
    let before = scene.shepherd.sheep("hazels-lab");

    let lines = scene.add().await.unwrap();
    assert_eq!(scene.shepherd.writes(), []);
    assert_eq!(
        scene.forge.repo_labels_now(),
        ["bug", "ready-for-agent", "ready-for-human", "review please"]
    );
    assert_eq!(scene.shepherd.sheep("hazels-lab"), before);
    // Three labels, the runner and the dog, each already there.
    assert_eq!(lines.len(), 5, "{lines:?}");
    assert!(lines.iter().all(|l| l.contains("already")), "{lines:?}");
}

#[tokio::test]
async fn a_public_repo_with_no_ci_gets_coderabbit_and_no_ci() {
    let scene = Scene::new().await;
    scene.forge.set_visibility(Visibility::Public);
    std::fs::remove_dir_all(scene.checkout.root.join(".github")).unwrap();
    scene.add().await.unwrap();
    let settings = scene.settings();
    assert!(settings.coderabbit.enabled);
    assert!(!settings.ci);
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
        .holds(AppConfig::minimal("hazels-lab", "/srv/web"), true);
    let err = scene.add().await.unwrap_err();
    assert!(
        err.contains("`hazels-lab` is already in this flock"),
        "{err}"
    );
    assert_eq!(scene.shepherd.writes(), []);
    assert_eq!(scene.forge.repo_labels_now(), ["bug"]);
}

#[tokio::test]
async fn an_enabled_dog_that_holds_the_project_s_name_is_not_kelpie_s() {
    let scene = Scene::new().await;
    scene.shepherd.holds_dog("hazels-lab");
    let err = scene.add().await.unwrap_err();
    assert!(err.contains("is not kelpie's"), "{err}");
}

#[tokio::test]
async fn a_dog_under_its_old_name_is_replaced_and_kept_running() {
    let mut scene = Scene::new().await;
    let mut old = scene.launch.dog();
    old.name = "kelpie".into();
    scene.shepherd.holds(old, true);

    let lines = scene.add().await.unwrap();
    assert!(scene.shepherd.sheep("kelpie").is_none());
    let (dog, running) = scene.shepherd.sheep("kelpie-dog").unwrap();
    assert_eq!(dog.args, ["dog"]);
    assert!(running, "the book's dog stays up");
    assert!(
        lines.iter().any(|l| l.contains("replaces `kelpie`")),
        "{lines:?}"
    );
    let writes = scene.shepherd.writes();
    assert!(
        writes.iter().any(|w| matches!(w, Request::Delete { .. })),
        "{writes:?}"
    );
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
    let (runner, _) = scene.shepherd.sheep("hazels-lab").unwrap();
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
async fn a_project_cannot_take_kelpie_s_own_names() {
    for taken in ["kelpie", "kelpie-dog"] {
        let mut scene = Scene::new().await;
        scene.name = ProjectName::try_from(taken).unwrap();
        let err = scene.add().await.unwrap_err();
        assert!(err.contains("kelpie's own name"), "{taken}: {err}");
        assert_eq!(scene.shepherd.writes(), []);
    }
}

#[tokio::test]
async fn a_dog_under_both_names_is_refused_before_anything_changes() {
    let mut scene = Scene::new().await;
    let mut old = scene.launch.dog();
    old.name = "kelpie".into();
    scene.shepherd.holds(old, true);
    scene.shepherd.holds(scene.launch.dog(), true);
    let err = scene.add().await.unwrap_err();
    assert!(err.contains("one book needs one dog"), "{err}");
    assert_eq!(scene.shepherd.writes(), []);
    assert_eq!(scene.forge.repo_labels_now(), ["bug"]);
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
    let added_runner =
        |w: &Request| matches!(w, Request::Add { apps } if apps[0].name == "hazels-lab");
    assert!(!writes.iter().any(added_runner), "{writes:?}");
}
