use std::path::PathBuf;

use shep_client::shep_core::config::AppConfig;
use shep_client::shep_core::values::UpDuration;

use super::*;
use crate::home::ProjectPaths;
use crate::ports::Visibility;
use crate::settings::{AgentName, BaseBranch, ForgeSlug, RulingName};
use crate::state::STATE_FILE;
use crate::test::{CALL_PATIENCE, FakeForge, FakeShepherd, ticket};

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
        let shepherd = FakeShepherd::new();
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

    fn written(&self, agent: &str) -> String {
        std::fs::read_to_string(self.agents().join(format!("{agent}.md"))).unwrap()
    }

    fn old_home(&self) -> PathBuf {
        self.home.join(".kelpie")
    }

    fn folder(&self) -> PathBuf {
        self.home.join(".shep/kelpie").join(self.name.as_str())
    }

    async fn add(&self) -> Result<Vec<String>, String> {
        self.add_on(None).await
    }

    async fn add_on(&self, base: Option<&str>) -> Result<Vec<String>, String> {
        let client = self.shepherd.connect().await;
        let (folder, agents, old) = (self.folder(), self.agents(), self.old_home());
        let place = Place {
            checkout: &self.checkout,
            home: &self.home,
            folder: &folder,
            agents: &agents,
            old_home: Some(&old),
            base,
        };
        let added = add(&client, &self.forge, &self.launch, &self.name, place);
        tokio::time::timeout(CALL_PATIENCE, added)
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

fn stub(agent: &str) -> String {
    format!("---\nextends: kelpie:{agent}\n---\n")
}

#[tokio::test]
async fn add_in_a_scratch_repo_writes_the_table_makes_the_labels_and_adds_the_runner() {
    let mut scene = Scene::new().await;
    let lines = scene.add().await.unwrap();
    assert_eq!(
        lines[lines.len() - 2..],
        [
            "runner `koji`: added with its settings, stopped",
            "`shep kelpie start koji` runs it",
        ]
    );

    let writes = scene.shepherd.writes();
    let [Request::Add { apps: runner }] = writes.as_slice() else {
        panic!("{writes:?}");
    };
    assert_eq!(runner[0].args, ["runner", "koji"]);
    let shep_s_own = AppConfig::minimal("koji", "/opt/kelpie").kill_timeout;
    assert_eq!(
        runner[0].kill_timeout.as_millis(),
        shep_s_own.as_millis(),
        "add wrote a kill_timeout"
    );
    let settings = scene.settings();
    assert_eq!(settings.git.checkout, scene.checkout.root);
    let remote = settings.git.remote.as_ref().map(ForgeSlug::as_str);
    assert_eq!(remote, Some("shep-pm/koji-website"));
    assert!(settings.ci.block, "the checkout has workflows");
    let reviewers = settings.agents.reviewers.clone().unwrap();
    assert_eq!(
        reviewers,
        [AgentName::try_from("defect-hunter-lite".to_owned()).unwrap()]
    );
    assert_eq!(settings.agents.gone_keys(), Vec::<&str>::new());
    for name in ["sonnet-high", "opus-high"] {
        assert_eq!(scene.written(name), stub(name));
    }
    assert!(
        !scene.agents().join("qwen.md").exists(),
        "no qwen-review script, so no qwen reviewer"
    );
    for bot in ["coderabbit", "cubic", "codex"] {
        assert_eq!(
            scene.written(bot),
            stub(bot),
            "written, though no project lists it"
        );
    }
    let shipped = scene.agents().join(".defaults");
    for (name, text) in [
        (
            "sonnet-high",
            include_str!("../../../agents/sonnet-high.md"),
        ),
        ("triager", include_str!("../../../agents/triager.md")),
        ("pm", include_str!("../../../agents/pm.md")),
    ] {
        let kept = std::fs::read_to_string(shipped.join(format!("{name}.md"))).unwrap();
        assert!(
            kept.ends_with(text.strip_prefix("---\n").unwrap()),
            "{name}"
        );
        let file = std::fs::metadata(shipped.join(format!("{name}.md"))).unwrap();
        assert!(file.permissions().readonly(), "{name}");
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
async fn where_the_qwen_review_script_is_installed_add_still_lists_defect_hunter_lite_alone() {
    let scene = Scene::new().await;
    let script = scene.home.join(".claude/scripts/qwen-review.sh");
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    crate::test::write_script(&script, "#!/bin/sh\nexit 0\n");
    scene.add().await.unwrap();

    let listed = scene.settings().agents.reviewers.unwrap();
    assert_eq!(listed, [AgentName::kelpies("defect-hunter-lite")]);
    assert_eq!(scene.written("qwen"), stub("qwen"));
}

#[tokio::test]
async fn where_kelpie_holds_a_paddock_gateway_add_lists_iq3_xxs_review_first() {
    let scene = Scene::new().await;
    scene.shepherd.holds_section(
        "[gateways.paddock]\nurl = \"http://gpu-box:8700\"\nkey_env = \"PADDOCK_KEY\"\n",
    );
    scene.add().await.unwrap();

    let listed = scene.settings().agents.reviewers.unwrap();
    let names = ["iq3-xxs-review", "defect-hunter-lite"].map(AgentName::kelpies);
    assert_eq!(listed, names);
    for agent in ["iq3-xxs-review", "defect-hunter-lite"] {
        assert_eq!(scene.written(agent), stub(agent));
    }
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
    assert_eq!(scene.settings().git.checkout, scene.checkout.root);
    let paths = ProjectPaths::under(&crate::home::under(shep_home), shep_home, &scene.name);
    assert_eq!(
        paths.worktree(ticket(7)),
        shep_home.join("kelpie/koji/worktrees/7")
    );
    assert_eq!(
        paths.build(ticket(7)),
        shep_home.join("kelpie/koji/builds/7")
    );
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
    // Four labels, kelpie's agent files and the runner, each already there,
    // then how to start it.
    let (last, already) = lines.split_last().unwrap();
    assert_eq!(already.len(), 6, "{lines:?}");
    assert!(already.iter().all(|l| l.contains("already")), "{lines:?}");
    assert_eq!(last, "`shep kelpie start koji` runs it");
}

#[tokio::test]
async fn a_public_repo_with_no_ci_gets_no_ci_and_still_lists_no_review_bot() {
    let scene = Scene::new().await;
    scene.forge.set_visibility(Visibility::Public);
    std::fs::remove_dir_all(scene.checkout.root.join(".github")).unwrap();
    scene.add().await.unwrap();
    let settings = scene.settings();
    assert!(!settings.ci.block);
    let reviewers = settings.agents.reviewers.unwrap();
    assert_eq!(reviewers, [AgentName::kelpies("defect-hunter-lite")]);
}

#[tokio::test]
async fn add_writes_stubs_for_the_whole_agents_and_none_for_a_baseline() {
    let scene = Scene::new().await;
    scene.add().await.unwrap();
    for baseline in ["standards", "spec", "pm", "defect-hunter", "triager"] {
        assert!(
            !scene.agents().join(format!("{baseline}.md")).exists(),
            "{baseline} is a baseline: its stub comes from `agents new`"
        );
    }
    let reviewers = scene.settings().agents.reviewers.unwrap();
    assert_eq!(reviewers, [AgentName::kelpies("defect-hunter-lite")]);
}

#[tokio::test]
async fn a_new_project_s_table_holds_every_kind_of_ruling_at_its_default() {
    let scene = Scene::new().await;
    scene.add().await.unwrap();
    let (runner, _) = scene.shepherd.sheep("koji").expect("a runner");
    let table = runner
        .dogs
        .get(DOG)
        .expect("a kelpie table")
        .as_map()
        .clone();
    let map = table["rulings"]["pm_decides"].as_object().unwrap();
    for kind in RulingName::ALL {
        let written = map.get(kind.as_str()).and_then(Value::as_bool);
        assert_eq!(written, kind.pm_default(), "{kind:?}");
    }
    assert!(scene.settings().rulings.pm_decides(RulingName::StillRed));
}

#[tokio::test]
async fn a_repo_whose_default_branch_is_main_gets_it_written_as_the_base() {
    let scene = Scene::new().await;
    scene.add().await.unwrap();
    assert_eq!(scene.settings().git.base, Some(base("main")));
}

#[tokio::test]
async fn a_default_branch_other_than_main_is_the_base_the_project_is_added_with() {
    let scene = Scene::new().await;
    scene.forge.set_default_branch("trunk");
    scene.add().await.unwrap();
    assert_eq!(scene.settings().git.base(), "trunk");
}

#[tokio::test]
async fn base_next_on_a_repo_whose_default_is_main_writes_it() {
    let scene = Scene::new().await;
    scene.forge.set_branches(&["next"]);
    scene.add_on(Some("next")).await.unwrap();
    assert_eq!(scene.settings().git.base, Some(base("next")));
}

#[tokio::test]
async fn a_base_the_repo_does_not_have_is_refused_before_anything_changes() {
    let mut scene = Scene::new().await;
    let err = scene.add_on(Some("next")).await.unwrap_err();
    assert!(err.contains("koji-website has no branch `next`"), "{err}");
    assert_eq!(scene.shepherd.writes(), []);
    assert_eq!(scene.forge.repo_labels_now(), ["bug"]);
}

#[tokio::test]
async fn a_base_that_is_no_branch_name_is_refused() {
    let mut scene = Scene::new().await;
    let err = scene.add_on(Some("--next")).await.unwrap_err();
    assert!(err.contains("must be a branch name"), "{err}");
    assert_eq!(scene.shepherd.writes(), []);
}

#[tokio::test]
async fn adding_again_with_another_base_is_refused_and_leaves_the_table() {
    let scene = Scene::new().await;
    scene.forge.set_branches(&["next"]);
    scene.add().await.unwrap();
    let err = scene.add_on(Some("next")).await.unwrap_err();
    assert!(err.contains("works against `main`, not `next`"), "{err}");
    assert!(err.contains("shep kelpie set git.base next"), "{err}");
    assert_eq!(scene.settings().git.base(), "main");
}

#[tokio::test]
async fn adding_again_with_the_base_it_has_changes_nothing() {
    let scene = Scene::new().await;
    scene.forge.set_branches(&["next"]);
    scene.add_on(Some("next")).await.unwrap();
    let again = scene.add_on(Some("next")).await.unwrap();
    assert!(
        again.contains(&"runner `koji`: already there with its settings".to_owned()),
        "{again:?}"
    );
}

fn base(branch: &str) -> BaseBranch {
    BaseBranch::try_from(branch.to_owned()).unwrap()
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
async fn a_broken_table_for_this_checkout_is_refused() {
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
async fn another_checkout_of_a_repo_a_project_reads_from_its_origin_is_refused() {
    let mut scene = Scene::new().await;
    let theirs = scene.shepherd.scratch("koji-elsewhere");
    crate::test::git(&theirs, &["init", "-q"]);
    let origin = "git@github.com:shep-pm/koji-website.git";
    crate::test::git(&theirs, &["remote", "add", "origin", origin]);
    let mut table = crate::test::project_table(include_str!("../../../settings.example.toml"));
    table["git"]["checkout"] = Value::String(theirs.display().to_string());
    let git = table["git"].as_object_mut().unwrap();
    git.remove("remote");
    let older = ProjectName::try_from("older").unwrap();
    scene
        .shepherd
        .holds(scene.launch.runner(&older, table), true);
    let err = scene.add().await.unwrap_err();
    assert!(err.starts_with("project `older` already runs"), "{err}");
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
    scene.shepherd = FakeShepherd::new();
    let lines = scene.add().await.unwrap();
    assert_eq!(
        lines[lines.len() - 2..],
        [
            "kelpie's dog is not enabled: `shep enable kelpie` runs it",
            "`shep kelpie start koji` runs it",
        ]
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
    scene.shepherd = FakeShepherd::new();
    scene.shepherd.holds_dog("kelpie", false);
    let lines = scene.add().await.unwrap();
    let last = &lines[lines.len() - 2];
    assert!(
        last.ends_with(
            "run `shep adopt /opt/kelpie --name kelpie`, then `shep disable kelpie` and \
             `shep enable kelpie`"
        ),
        "{last}"
    );
}

#[tokio::test]
async fn a_runner_entry_that_still_sets_kill_timeout_is_taken_as_it_is() {
    let mut scene = Scene::new().await;
    let mut runner = scene.launch.runner(&scene.name, Map::new());
    runner.dogs.clear();
    runner.kill_timeout = UpDuration::from_millis(10_000);
    scene.shepherd.holds(runner, true);
    let lines = scene.add().await.unwrap();
    assert!(
        lines
            .iter()
            .any(|l| l.ends_with("already there, and given its settings")),
        "{lines:?}"
    );
    let (kept, running) = scene.shepherd.sheep("koji").unwrap();
    assert_eq!(kept.kill_timeout.as_millis(), 10_000);
    assert!(running);
    let writes = scene.shepherd.writes();
    let added_runner = |w: &Request| matches!(w, Request::Add { apps } if apps[0].name == "koji");
    assert!(!writes.iter().any(added_runner), "{writes:?}");
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
    assert_eq!(scene.settings().git.checkout, scene.checkout.root);
    let writes = scene.shepherd.writes();
    let added_runner = |w: &Request| matches!(w, Request::Add { apps } if apps[0].name == "koji");
    assert!(!writes.iter().any(added_runner), "{writes:?}");
}

#[tokio::test]
async fn add_warns_when_the_old_home_still_holds_the_runner_s_folder_and_still_adds_it() {
    let scene = Scene::new().await;
    let left = scene.old_home().join("projects/koji");
    std::fs::create_dir_all(&left).unwrap();
    std::fs::write(left.join(STATE_FILE), "{}").unwrap();

    let lines = scene.add().await.unwrap();

    let warning = lines.iter().find(|l| l.starts_with("warning:")).unwrap();
    assert!(warning.contains("`koji` will refuse to start"), "{warning}");
    assert!(warning.contains("move "), "{warning}");
    assert!(warning.contains(&left.display().to_string()), "{warning}");
    assert!(
        warning.contains(&scene.folder().display().to_string()),
        "{warning}"
    );
    assert!(
        scene.shepherd.sheep("koji").is_some(),
        "the runner is added"
    );

    let clean = Scene::new().await;
    let lines = clean.add().await.unwrap();
    assert!(
        lines.iter().all(|l| !l.starts_with("warning:")),
        "{lines:?}"
    );
}

#[tokio::test]
async fn a_runner_is_added_to_stay_stopped_after_a_refusal_the_maintainer_must_clear() {
    let scene = Scene::new().await;
    scene.add().await.unwrap();
    let (runner, _) = scene.shepherd.sheep("koji").expect("a runner");
    assert_eq!(runner.stop_exit_codes, [78]);
}

#[tokio::test]
async fn a_runner_registered_before_the_refusal_code_is_given_it_and_keeps_its_own_codes() {
    let mut scene = Scene::new().await;
    let mut runner = scene.launch.runner(&scene.name, Map::new());
    runner.dogs.clear();
    runner.stop_exit_codes = vec![3];
    scene.shepherd.holds(runner, true);

    let lines = scene.add().await.unwrap();

    assert!(
        lines.iter().any(|l| l.contains("now stays stopped")),
        "{lines:?}"
    );
    let (kept, running) = scene.shepherd.sheep("koji").unwrap();
    assert_eq!(kept.stop_exit_codes, [3, 78]);
    assert!(running, "the runner is not restarted");

    scene.shepherd.writes();
    let lines = scene.add().await.unwrap();
    assert!(lines.iter().all(|l| !l.contains("now stays stopped")));
    let writes = scene.shepherd.writes();
    assert!(
        !writes
            .iter()
            .any(|w| matches!(w, Request::SetSheepField { .. })),
        "{writes:?}"
    );
}
