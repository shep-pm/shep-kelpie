use std::path::PathBuf;

use shep_client::RequestError;
use shep_client::shep_core::config::{AppConfig, DogTable};
use shep_client::shep_core::protocol::HelloAck;

use super::*;
use crate::flock::glob::{Glob, Named};
use crate::home::ProjectPaths;
use crate::settings::table_of;
use crate::test::{CALL_PATIENCE, FakeShepherd};

const TABLE: &str = r#"
[git]
checkout = "CHECKOUT"
remote = "REMOTE"
merging = "ask"
issues = "ask"

[ci]
block = true
fix_attempts = -1

[concurrency]
active_items = 1
pending_rulings = 2

[budget]
per_item_usd = 10

[agents]
reviewers = ["standards"]

[pacing]
enabled = true
kickoff_hours = 8

[worker]
allowed_domains = []
build_env = {}
guard_hooks = []
turn_timeout = 60
"#;

/// A flock holding the runners `koji` and `eevee`, each with `TABLE` and a git
/// checkout of its own, and `rotom` with no table
struct Scene {
    shepherd: FakeShepherd,
    checkouts: [PathBuf; 2],
}

// kelpie's own section naming one gateway, whose key's variable this shell does not hold
const GATEWAY: &str = "[gateways.paddock]\nurl = \"http://gpu-box:8700\"\n\
                        key_env = \"KELPIE_TEST_KEY_NEVER_SET\"\n";

// A reviewer that runs on the gateway `paddock`, and one on `elsewhere`
const ON_GATEWAY: &str = "---\nrole: reviewer\nharness: pi\nmodel: qwen3.8:27b\n\
                          effort: medium\ngateway: paddock\ncontext: 65536\n---\nReview {{DIFF}}.\n";
const ON_ELSEWHERE: &str = "---\nrole: reviewer\nharness: pi\nmodel: qwen3.8:27b\n\
                            effort: medium\ngateway: elsewhere\ncontext: 65536\n---\nReview {{DIFF}}.\n";

// Writes `files` into the agent folder every project's settings are checked against.
fn write_agents(shepherd: &FakeShepherd, files: &[(&str, &str)]) {
    let folder = shepherd.scratch("home").join("kelpie").join("agents");
    std::fs::create_dir_all(&folder).unwrap();
    for (name, text) in files {
        std::fs::write(folder.join(name), text).unwrap();
    }
}

// A git checkout under `shepherd`'s scratch home whose `origin` is `remote` on GitHub.
fn checkout(shepherd: &FakeShepherd, folder: &str, remote: &str) -> PathBuf {
    let root = shepherd.scratch(folder);
    let url = format!("https://github.com/{remote}.git");
    for args in [
        vec!["init", "-q"],
        vec!["remote", "add", "origin", url.as_str()],
    ] {
        let status = crate::flock::git_in(&root).args(args).status().unwrap();
        assert!(status.success());
    }
    root
}

// Puts a runner named `name` with `table` as its settings, on a checkout of its own, in the flock
fn add_project(shepherd: &FakeShepherd, name: &str, table: &str) -> PathBuf {
    let root = checkout(
        shepherd,
        &format!("{name}-checkout"),
        &format!("shep-pm/{name}"),
    );
    let text = table
        .replace("CHECKOUT", &root.display().to_string())
        .replace("REMOTE", &format!("shep-pm/{name}"));
    let mut runner = AppConfig::minimal(name, "/opt/kelpie");
    let table = DogTable::from(table_of(&text).unwrap());
    runner.dogs.insert(DOG.to_owned(), table);
    shepherd.holds(runner, false);
    root
}

fn glob_of(text: &str) -> Glob {
    match Named::parse(text).unwrap() {
        Named::Glob(glob) => glob,
        Named::Project(name) => panic!("{name} read as a project"),
    }
}

// A connection whose write to `refused` fails, as a shepherd that closed would
#[derive(Debug)]
struct Refusing {
    inner: Box<dyn Connection>,
    refused: &'static str,
}

impl Connection for Refusing {
    fn send(
        &self,
        body: Request,
        deadline: Option<std::time::Duration>,
    ) -> crate::shepherd::Answer<'_> {
        match &body {
            Request::SetSheepDogSettings { name, .. } if name == self.refused => {
                Box::pin(async { Err(RequestError::Closed) })
            }
            _ => self.inner.send(body, deadline),
        }
    }

    fn daemon(&self) -> HelloAck {
        self.inner.daemon()
    }
}

impl Scene {
    async fn new() -> Self {
        Self::holding(TABLE).await
    }

    /// A flock whose `koji` and `eevee` hold `table` as their settings
    async fn holding(table: &str) -> Self {
        Self::holding_each(table, table).await
    }

    /// A flock whose `koji` holds `koji` and `eevee` holds `eevee` as their settings
    async fn holding_each(koji: &str, eevee: &str) -> Self {
        let shepherd = FakeShepherd::new();
        let checkouts = [
            add_project(&shepherd, "koji", koji),
            add_project(&shepherd, "eevee", eevee),
        ];
        shepherd.holds(AppConfig::minimal("rotom", "/opt/kelpie"), false);
        crate::agents::write_bound_stubs(
            &shepherd.scratch("home").join("kelpie").join("agents"),
            "harness: claude-code\nmodel: claude-opus-5-5\neffort: high\n",
        );
        Self {
            shepherd,
            checkouts,
        }
    }

    /// A flock whose `koji` lists `local`, a reviewer on the gateway in
    /// kelpie's own settings, and holds `box`, a reviewer on a gateway of
    /// kelpie has none of
    async fn keyed_gateway() -> Self {
        let table = TABLE.replace(
            "reviewers = [\"standards\"]",
            "reviewers = [\"standards\", \"local\"]",
        );
        let scene = Self::holding(&table).await;
        scene.shepherd.holds_section(GATEWAY);
        write_agents(
            &scene.shepherd,
            &[("local.md", ON_GATEWAY), ("box.md", ON_ELSEWHERE)],
        );
        scene
    }

    async fn set(&self, key: &str, value: &str) -> Result<Vec<String>, String> {
        self.set_on("koji", key, value).await
    }

    async fn set_on(&self, name: &str, key: &str, value: &str) -> Result<Vec<String>, String> {
        let client = self.shepherd.connect().await;
        let home = self.shepherd.scratch("home");
        let name = ProjectName::try_from(name).unwrap();
        let paths = ProjectPaths::under(&home.join("kelpie"), self.shepherd.home(), &name);
        let place = Place {
            home: &home,
            paths: &paths,
        };
        let done = set(&client, &name, place, key, value);
        tokio::time::timeout(CALL_PATIENCE, done)
            .await
            .expect("set neither ended nor failed in time")
    }

    async fn set_all(&self, pattern: &str, key: &str, value: &str) -> Result<Vec<String>, String> {
        let client = self.shepherd.connect().await;
        self.set_all_via(&client, pattern, key, value).await
    }

    async fn set_all_via(
        &self,
        client: &dyn Connection,
        pattern: &str,
        key: &str,
        value: &str,
    ) -> Result<Vec<String>, String> {
        let home = self.shepherd.scratch("home");
        let kelpie_home = home.join("kelpie");
        let homes = Homes {
            home: &home,
            kelpie_home: &kelpie_home,
            shep_home: self.shepherd.home(),
        };
        let glob = glob_of(pattern);
        let done = set_all(client, &glob, homes, key, value);
        tokio::time::timeout(CALL_PATIENCE, done)
            .await
            .expect("set_all neither ended nor failed in time")
    }

    async fn get_all(&self, pattern: &str, key: &str) -> Result<Vec<String>, String> {
        let client = self.shepherd.connect().await;
        let glob = glob_of(pattern);
        let done = get_all(&client, &glob, key);
        tokio::time::timeout(CALL_PATIENCE, done)
            .await
            .expect("get_all neither ended nor failed in time")
    }

    async fn get(&self, name: &str, key: Option<&str>) -> Result<Vec<String>, String> {
        let client = self.shepherd.connect().await;
        let name = ProjectName::try_from(name).unwrap();
        let done = get(&client, &name, key);
        tokio::time::timeout(CALL_PATIENCE, done)
            .await
            .expect("get neither ended nor failed in time")
    }

    /// The table the shepherd holds for `koji`
    fn table(&self) -> Map<String, Value> {
        self.table_of("koji")
    }

    fn table_of(&self, name: &str) -> Map<String, Value> {
        let (sheep, _) = self.shepherd.sheep(name).unwrap();
        sheep.dogs.get(DOG).unwrap().as_map().clone()
    }
}

#[tokio::test]
async fn set_writes_a_word_and_a_list_and_get_shows_both() {
    let mut scene = Scene::new().await;
    scene.set("git.merging", "auto").await.unwrap();
    scene
        .set("worker.allowed_domains", r#"["crates.io"]"#)
        .await
        .unwrap();

    let writes = scene.shepherd.writes();
    assert_eq!(writes.len(), 2, "{writes:?}");
    assert_eq!(
        scene.get("koji", Some("git.merging")).await.unwrap(),
        [r#""auto""#]
    );
    assert_eq!(
        scene
            .get("koji", Some("worker.allowed_domains"))
            .await
            .unwrap(),
        [r#"["crates.io"]"#]
    );
    let whole = scene.get("koji", None).await.unwrap().join("\n");
    assert!(whole.contains("merging = \"auto\""), "{whole}");
    assert!(
        whole.contains("allowed_domains = [\"crates.io\"]"),
        "{whole}"
    );
}

#[tokio::test]
async fn set_reads_a_number_a_boolean_and_a_word_as_what_they_are() {
    let scene = Scene::new().await;
    scene.set("ci.fix_attempts", "3").await.unwrap();
    scene.set("ci.block", "true").await.unwrap();
    scene.set("agents.reviewers", "[]").await.unwrap();
    let table = scene.table();
    assert_eq!(table["ci"]["fix_attempts"], 3);
    assert_eq!(table["ci"]["block"], true);
    assert_eq!(table["agents"]["reviewers"], serde_json::json!([]));
}

#[tokio::test]
async fn set_leaves_every_other_key_as_it_was() {
    let scene = Scene::new().await;
    let before = scene.table();
    scene.set("git.merging", "auto").await.unwrap();
    let mut expected = before;
    expected["git"]["merging"] = "auto".into();
    assert_eq!(scene.table(), expected);
}

#[tokio::test]
async fn set_makes_the_table_a_key_is_in_when_the_settings_have_none() {
    let scene = Scene::new().await;
    scene
        .set("browser.domains", r#"["crates.io"]"#)
        .await
        .unwrap();
    assert_eq!(
        scene.table()["browser"]["domains"],
        serde_json::json!(["crates.io"])
    );
}

#[tokio::test]
async fn a_value_that_does_not_fit_is_refused_with_the_settings_reason_and_nothing_is_written() {
    let mut scene = Scene::new().await;
    let before = scene.table();
    for (key, value, reason) in [
        ("git.merging", "sometimes", "merging"),
        ("git.nonsense", "1", "nonsense"),
        ("nonsense", "1", "nonsense"),
        ("git.merging.deeper", "1", "git.merging"),
    ] {
        let refused = scene.set(key, value).await.unwrap_err();
        assert!(refused.contains(reason), "{key}: {refused}");
    }
    let refused = scene.set("planning", "true").await.unwrap_err();
    assert!(refused.contains("planning"), "{refused}");
    assert_eq!(scene.shepherd.writes(), []);
    assert_eq!(scene.table(), before);
}

#[tokio::test]
async fn the_refusal_is_the_one_from_settings_from_table() {
    let scene = Scene::new().await;
    let mut table = scene.table();
    table["git"]["merging"] = "sometimes".into();
    let home = scene.shepherd.scratch("home");
    let why = Settings::from_table(&table, "koji", &home, &home).unwrap_err();
    let refused = scene.set("git.merging", "sometimes").await.unwrap_err();
    assert_eq!(refused, why.to_string());
}

#[tokio::test]
async fn get_of_a_key_the_table_does_not_set_is_refused_naming_it() {
    let scene = Scene::new().await;
    for key in ["git.review", "nothing", "git.merging.deeper"] {
        let refused = scene.get("koji", Some(key)).await.unwrap_err();
        assert!(refused.contains(&format!("`{key}`")), "{refused}");
    }
}

#[tokio::test]
async fn a_project_with_no_table_is_refused_pointing_at_add() {
    let mut scene = Scene::new().await;
    let refused = scene
        .set_on("rotom", "git.merging", "auto")
        .await
        .unwrap_err();
    assert!(refused.contains("shep kelpie add rotom"), "{refused}");
    let refused = scene.get("rotom", None).await.unwrap_err();
    assert!(refused.contains("shep kelpie add rotom"), "{refused}");
    let refused = scene.get("absent", None).await.unwrap_err();
    assert!(refused.contains("shep kelpie add absent"), "{refused}");
    assert_eq!(scene.shepherd.writes(), []);
}

#[tokio::test]
async fn a_table_the_runner_would_refuse_to_run_is_refused_and_nothing_is_written() {
    let mut scene = Scene::new().await;
    let before = scene.table();
    for (key, value, reason) in [
        ("rulings.pm_alone", r#"["merge"]"#, "pm_alone"),
        ("agents.reviewers", r#"["no-such-agent"]"#, "no-such-agent"),
    ] {
        let refused = scene.set(key, value).await.unwrap_err();
        assert!(refused.contains(reason), "{key}: {refused}");
    }
    assert_eq!(scene.shepherd.writes(), []);
    assert_eq!(scene.table(), before);
}

#[tokio::test]
async fn a_checkout_or_repo_another_project_runs_is_refused_and_nothing_is_written() {
    let mut scene = Scene::new().await;
    let koji = scene.checkouts[0].display().to_string();
    for (key, value) in [
        ("git.checkout", koji.as_str()),
        ("git.remote", "shep-pm/koji"),
    ] {
        let refused = scene.set_on("eevee", key, value).await.unwrap_err();
        assert!(
            refused.contains("project `koji` already runs"),
            "{key}: {refused}"
        );
    }
    assert_eq!(scene.shepherd.writes(), []);
}

#[tokio::test]
async fn a_checkout_that_is_not_a_git_work_tree_is_refused() {
    let mut scene = Scene::new().await;
    let nowhere = scene.shepherd.home().join("nowhere");
    let refused = scene
        .set("git.checkout", &nowhere.display().to_string())
        .await
        .unwrap_err();
    assert!(refused.contains("is not a folder"), "{refused}");
    assert_eq!(scene.shepherd.writes(), []);
}

#[tokio::test]
async fn a_checkout_moved_to_one_no_other_project_runs_is_written_and_waits_for_a_start() {
    let scene = Scene::new().await;
    let spare = checkout(&scene.shepherd, "spare", "shep-pm/spare");
    let said = scene
        .set("git.checkout", &spare.display().to_string())
        .await
        .unwrap();
    assert!(said[0].ends_with("when it next starts"), "{said:?}");
    assert_eq!(
        scene.table()["git"]["checkout"],
        spare.display().to_string()
    );
    let said = scene.set("git.merging", "auto").await.unwrap();
    assert!(said[0].ends_with("at its next wake"), "{said:?}");
}

#[tokio::test]
async fn a_changed_hooks_table_is_taken_at_the_next_wake() {
    let scene = Scene::new().await;
    let said = scene.set("hooks", "[]").await.unwrap();
    assert!(said[0].ends_with("at its next wake"), "{said:?}");
}

#[tokio::test]
async fn a_project_on_a_keyed_gateway_needs_no_key_in_the_shell_that_runs_set() {
    let scene = Scene::keyed_gateway().await;
    let said = scene.set("concurrency.active_items", "2").await.unwrap();
    assert!(said[0].contains("concurrency.active_items = 2"), "{said:?}");
    assert_eq!(scene.table()["concurrency"]["active_items"], 2);
}

#[tokio::test]
async fn an_agent_on_a_gateway_kelpie_has_none_of_is_refused_naming_it_and_nothing_is_written() {
    let mut scene = Scene::keyed_gateway().await;
    let before = scene.table();
    let refused = scene
        .set("agents.reviewers", r#"["standards", "box"]"#)
        .await
        .unwrap_err();
    assert!(refused.contains("box"), "{refused}");
    assert!(
        refused.contains("gateway elsewhere is not in kelpie's settings"),
        "{refused}"
    );
    assert!(!refused.contains("KELPIE_TEST_KEY_NEVER_SET"), "{refused}");
    assert_eq!(scene.shepherd.writes(), []);
    assert_eq!(scene.table(), before);
}

const WAKE: &str = "a running runner takes it at its next wake";

#[tokio::test]
async fn a_glob_writes_every_project_whose_name_matches_and_no_other() {
    let mut scene = Scene::new().await;
    for name in ["shep", "shep-kelpie", "shepherd"] {
        add_project(&scene.shepherd, name, TABLE);
    }
    add_project(&scene.shepherd, "my-shep", TABLE);
    let (koji, eevee, other) = (
        scene.table(),
        scene.table_of("eevee"),
        scene.table_of("my-shep"),
    );
    let _ = scene.shepherd.writes();

    let said = scene
        .set_all("shep*", "git.merging", r#""auto""#)
        .await
        .unwrap();

    assert_eq!(
        said,
        ["shep", "shep-kelpie", "shepherd"]
            .map(|name| format!(r#"project {name}: git.merging = "auto"; {WAKE}"#))
    );
    assert_eq!(scene.shepherd.writes().len(), 3);
    for name in ["shep", "shep-kelpie", "shepherd"] {
        assert_eq!(scene.table_of(name)["git"]["merging"], "auto", "{name}");
    }
    assert_eq!(scene.table(), koji);
    assert_eq!(scene.table_of("eevee"), eevee);
    assert_eq!(scene.table_of("my-shep"), other);
}

#[tokio::test]
async fn a_glob_with_one_project_prints_the_line_a_plain_set_prints() {
    let scene = Scene::new().await;
    let one = scene.set_all("k*", "ci.fix_attempts", "3").await.unwrap();
    let plain = scene.set_on("eevee", "ci.fix_attempts", "3").await.unwrap();
    assert_eq!(one, [format!("project koji: ci.fix_attempts = 3; {WAKE}")]);
    assert_eq!(
        plain,
        [format!("project eevee: ci.fix_attempts = 3; {WAKE}")]
    );
}

#[tokio::test]
async fn one_project_refusing_the_value_means_no_project_is_written_and_the_error_names_it() {
    let mut scene = Scene::holding_each(
        TABLE,
        &TABLE.replace(r#"merging = "ask""#, r#"merging = "sometimes""#),
    )
    .await;
    let (koji, eevee) = (scene.table(), scene.table_of("eevee"));

    let refused = scene
        .set_all("*", "ci.fix_attempts", "3")
        .await
        .unwrap_err();

    assert!(refused.contains("project eevee"), "{refused}");
    assert!(refused.contains("merging"), "{refused}");
    assert!(!refused.contains("project koji"), "{refused}");
    assert_eq!(scene.shepherd.writes(), []);
    assert_eq!(scene.table(), koji);
    assert_eq!(scene.table_of("eevee"), eevee);
}

#[tokio::test]
async fn every_project_that_refuses_the_value_is_named() {
    let mut scene = Scene::new().await;
    let refused = scene
        .set_all("*", "git.merging", "sometimes")
        .await
        .unwrap_err();
    assert!(refused.contains("project eevee"), "{refused}");
    assert!(refused.contains("project koji"), "{refused}");
    assert_eq!(scene.shepherd.writes(), []);
}

#[tokio::test]
async fn a_glob_that_matches_no_project_is_an_error_and_nothing_is_written() {
    let mut scene = Scene::new().await;
    let refused = scene
        .set_all("nothing*", "git.merging", "auto")
        .await
        .unwrap_err();
    assert!(refused.contains("nothing*"), "{refused}");
    let refused = scene.get_all("nothing*", "git.merging").await.unwrap_err();
    assert!(refused.contains("nothing*"), "{refused}");
    assert_eq!(scene.shepherd.writes(), []);
}

#[tokio::test]
async fn a_write_that_fails_partway_says_which_projects_were_written_and_which_were_not() {
    let scene = Scene::holding(TABLE).await;
    add_project(&scene.shepherd, "zed", TABLE);
    let client = Refusing {
        inner: scene.shepherd.connect().await,
        refused: "koji",
    };

    let failed = scene
        .set_all_via(&client, "*", "ci.fix_attempts", "3")
        .await
        .unwrap_err();

    assert!(failed.contains("written: eevee"), "{failed}");
    assert!(failed.contains("not written: koji, zed"), "{failed}");
    assert_eq!(scene.table_of("eevee")["ci"]["fix_attempts"], 3);
    assert_eq!(scene.table()["ci"]["fix_attempts"], -1);
    assert_eq!(scene.table_of("zed")["ci"]["fix_attempts"], -1);
}

#[tokio::test]
async fn two_projects_moved_onto_one_checkout_are_refused_and_nothing_is_written() {
    let mut scene = Scene::new().await;
    let spare = checkout(&scene.shepherd, "spare", "shep-pm/spare");
    let refused = scene
        .set_all("*", "git.checkout", &spare.display().to_string())
        .await
        .unwrap_err();
    assert!(refused.contains("koji"), "{refused}");
    assert!(refused.contains("eevee"), "{refused}");
    assert_eq!(scene.shepherd.writes(), []);
}

#[tokio::test]
async fn get_prints_one_line_per_project_and_says_not_set_where_the_key_is_missing() {
    let scene = Scene::new().await;
    scene
        .set("browser.domains", r#"["crates.io"]"#)
        .await
        .unwrap();
    assert_eq!(
        scene.get_all("*", "browser.domains").await.unwrap(),
        ["project eevee: not set", r#"project koji: ["crates.io"]"#,]
    );
    assert_eq!(
        scene.get_all("[ek]*", "agents.reviewers").await.unwrap(),
        [
            r#"project eevee: ["standards"]"#,
            r#"project koji: ["standards"]"#,
        ]
    );
}

mod migrate;
