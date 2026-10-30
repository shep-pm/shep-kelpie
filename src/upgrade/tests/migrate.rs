//! The migrations an upgrade runs after the install and before the restarts

use super::*;
use crate::test::toml_path;

const STUB: &str = "---\nextends: kelpie:pm\n---\n";
const WROTE: &str = "pm.md: wrote claude-code claude-opus-5-5 medium";

// A stand-in build that answers `version --json`, and for `upgrade migrate`
// appends the homes it was told to `log`, prints `reply`, and exits `exit`.
fn migrating(kelpie: &str, log: &Path, reply: &str, exit: u8) -> String {
    let json = format!(r#"{{"kelpie":"{kelpie}","shep":"0.19.0","protocol":{PROTOCOL_VERSION}}}"#);
    format!(
        "#!/bin/sh\n\
         case \"$1 $2\" in\n\
         'version --json') echo '{json}' ;;\n\
         'upgrade migrate') echo \"$KELPIE_HOME $SHEP_HOME\" >> '{log}'; echo '{reply}'; \
         echo '{reply}' >&2; exit {exit} ;;\n\
         esac\n",
        log = log.display()
    )
}

fn stub_in(rig: &Rig) -> PathBuf {
    let agents = rig.home.join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    let stub = agents.join("pm.md");
    std::fs::write(&stub, STUB).unwrap();
    stub
}

const SETTLED: &str = "project koji: wrote rulings.pm_decides.rebase = true";

#[tokio::test]
async fn an_install_says_the_installed_build_s_own_migrations_before_the_restarts() {
    let rig = Rig::new().await;
    let log = rig.builds.join("migrated");
    let new = rig.builds.join("new");
    write_script(&new, &migrating("0.3.0", &log, SETTLED, 0));

    let (ran, said) = rig.install(&new).await;

    ran.unwrap();
    let (home, shep_home) = (rig.home.display(), rig.shepherd.home().display());
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        format!("{home} {shep_home}\n"),
        "the installed build migrated once, in these homes"
    );
    let at = |needle: &str| said.iter().position(|l| l.contains(needle));
    let settled = at(SETTLED).unwrap_or_else(|| panic!("{said:?}"));
    assert!(at("installed kelpie 0.3.0").unwrap() < settled, "{said:?}");
    assert!(settled < at("restarting onto it").unwrap(), "{said:?}");
}

#[tokio::test]
async fn an_install_whose_migration_fails_stops_before_the_restarts() {
    let mut rig = Rig::new().await;
    let log = rig.builds.join("migrated");
    let new = rig.builds.join("new");
    write_script(
        &new,
        &migrating("0.3.0", &log, "project koji: not migrated", 1),
    );

    let (ran, said) = rig.install(&new).await;

    let failed = ran.unwrap_err();
    assert!(
        failed.contains("the settings were not migrated"),
        "{failed}"
    );
    assert!(failed.contains("project koji: not migrated"), "{failed}");
    assert!(failed.contains("shep kelpie settings migrate"), "{failed}");
    assert!(failed.contains("shep kelpie upgrade --binary"), "{failed}");
    assert!(
        said.iter().all(|l| !l.contains("restarting onto it")),
        "{said:?}"
    );
    assert_eq!(rig.restarts(), Vec::<String>::new());
    assert_eq!(rig.installed_says(), "0.3.0 for shep 0.19.0");
    assert_eq!(says(&rig.previous()), "0.1.0 for shep 0.19.0");
}

// A stand-in build that answers `version --json`, and anything else with `stderr` and `exit`.
fn answering_else(stderr: &str, exit: u8) -> String {
    let json = format!(r#"{{"kelpie":"0.0.9","shep":"0.19.0","protocol":{PROTOCOL_VERSION}}}"#);
    format!(
        "#!/bin/sh\n\
         case \"$1 $2\" in\n\
         'version --json') echo '{json}' ;;\n\
         *) echo '{stderr}' >&2; exit {exit} ;;\n\
         esac\n"
    )
}

#[tokio::test]
async fn an_install_of_a_build_without_the_step_says_so_and_restarts() {
    let mut rig = Rig::new().await;
    let new = rig.builds.join("new");
    write_script(
        &new,
        &answering_else("usage: shep-kelpie upgrade [--now]", 2),
    );

    let (ran, said) = rig.install(&new).await;

    ran.unwrap();
    assert!(said.iter().any(|l| l.contains("predates")), "{said:?}");
    assert_eq!(rig.restarts(), ["kelpie", "koji"]);
}

#[tokio::test]
async fn an_exit_that_is_not_an_old_build_s_usage_stops_before_the_restarts() {
    for (stderr, exit) in [
        ("shep-kelpie upgrade migrate: no shepherd", 2),
        ("usage: shep-kelpie add [<project>]", 2),
        ("usage: shep-kelpie upgrade [--now]", 1),
    ] {
        let mut rig = Rig::new().await;
        let new = rig.builds.join("new");
        write_script(&new, &answering_else(stderr, exit));

        let (ran, said) = rig.install(&new).await;

        let failed = ran.unwrap_err();
        assert!(failed.contains(stderr), "{failed}");
        assert!(said.iter().all(|l| !l.contains("predates")), "{said:?}");
        assert_eq!(rig.restarts(), Vec::<String>::new(), "{stderr} {exit}");
    }
}

#[tokio::test]
async fn a_bare_upgrade_migrates_in_process() {
    let rig = Rig::new().await;
    let stub = stub_in(&rig);
    rig.cargo_installs("0.3.0", "0.19.0");

    let (ran, said) = rig.upgrade(Action::Installed).await;

    ran.unwrap();
    assert!(said.iter().any(|l| l == WROTE), "{said:?}");
    let written = std::fs::read_to_string(stub).unwrap();
    assert!(written.contains("harness: claude-code\n"), "{written}");
    assert!(written.contains("model: claude-opus-5-5\n"), "{written}");
}

#[tokio::test]
async fn a_rollback_does_not_migrate() {
    let rig = Rig::new().await;
    let log = rig.builds.join("migrated");
    std::fs::remove_file(&rig.installed).unwrap();
    write_script(&rig.installed, &migrating("0.1.0", &log, WROTE, 0));
    let new = rig.build("new", "0.3.0", "0.19.0");
    rig.install(&new).await.0.unwrap();
    let stub = stub_in(&rig);

    let (ran, said) = rig.upgrade(Action::Rollback).await;

    ran.unwrap();
    assert!(!log.exists(), "the build put back was not asked to migrate");
    assert_eq!(std::fs::read_to_string(stub).unwrap(), STUB);
    assert!(said.iter().all(|l| l != WROTE), "{said:?}");
}

// Settings for koji that load, on a git checkout under `shepherd`'s scratch home. Its
// `agents.implementers` is read by nothing, as an older build wrote it.
fn loading(shepherd: &FakeShepherd) -> Map<String, Value> {
    let root = shepherd.scratch("koji-checkout");
    let status = crate::flock::git_in(&root).args(["init", "-q"]).status();
    assert!(status.unwrap().success());
    let text = format!(
        "[git]\ncheckout = {}\nremote = \"shep-pm/koji\"\nmerging = \"ask\"\n\
         issues = \"ask\"\n[ci]\nblock = true\nfix_attempts = -1\n\
         [concurrency]\nactive_items = 1\npending_rulings = 2\n[budget]\nper_item_usd = 10\n\
         [agents]\nimplementers = [\"sonnet-high\"]\nreviewers = [\"standards\"]\n\
         [pacing]\nenabled = true\nkickoff_hours = 8\n[worker]\nallowed_domains = []\n\
         build_env = {{}}\nguard_hooks = []\nturn_timeout = 60\n",
        toml_path(&root)
    );
    crate::settings::table_of(&text).unwrap()
}

#[tokio::test]
async fn a_bare_upgrade_writes_the_missing_kinds_into_a_table_that_loads() {
    let shepherd = FakeShepherd::on(crate::shepherd::SHEP_VERSION);
    let table = loading(&shepherd);
    let mut rig = Rig::holding_on(shepherd, false, table).await;
    crate::agents::write_bound_stubs(
        &rig.home.join("agents"),
        "harness: claude-code\nmodel: claude-opus-5-5\neffort: high\n",
    );
    rig.cargo_installs("0.3.0", "0.19.0");

    let (ran, said) = rig.upgrade(Action::Installed).await;

    ran.unwrap();
    let wrote = (said.iter())
        .find(|l| l.starts_with("project koji: wrote "))
        .unwrap_or_else(|| panic!("{said:?}"));
    assert!(
        wrote.contains("rulings.pm_decides.rebase = true"),
        "{wrote}"
    );
    assert!(said.iter().all(|l| !l.contains("not migrated")), "{said:?}");
    let (koji, _) = rig.shepherd.sheep("koji").unwrap();
    let written = koji.dogs.get(crate::shepherd::DOG).unwrap().as_map();
    assert_eq!(written["rulings"]["pm_decides"]["question"], false);
    assert_eq!(rig.restarts(), ["kelpie", "koji"]);
}

#[tokio::test]
async fn a_bare_upgrade_writes_no_agents_allow_or_slack() {
    let shepherd = FakeShepherd::on(crate::shepherd::SHEP_VERSION);
    let table = loading(&shepherd);
    let rig = Rig::holding_on(shepherd, false, table).await;
    crate::agents::write_bound_stubs(
        &rig.home.join("agents"),
        "harness: claude-code\nmodel: claude-opus-5-5\neffort: high\n",
    );
    rig.cargo_installs("0.3.0", "0.19.0");

    let (ran, _) = rig.upgrade(Action::Installed).await;

    ran.unwrap();
    let (koji, _) = rig.shepherd.sheep("koji").unwrap();
    let written = koji.dogs.get(crate::shepherd::DOG).unwrap().as_map();
    let agents = written["agents"].as_object().unwrap();
    assert_eq!(agents.get("allow"), None);
    assert_eq!(agents.get("slack"), None);
}

#[tokio::test]
async fn a_bare_upgrade_deletes_the_keys_the_agent_board_replaced_and_names_them() {
    let shepherd = FakeShepherd::on(crate::shepherd::SHEP_VERSION);
    let table = loading(&shepherd);
    let rig = Rig::holding_on(shepherd, false, table).await;
    crate::agents::write_bound_stubs(
        &rig.home.join("agents"),
        "harness: claude-code\nmodel: claude-opus-5-5\neffort: high\n",
    );
    rig.cargo_installs("0.3.0", "0.19.0");

    let (ran, said) = rig.upgrade(Action::Installed).await;

    ran.unwrap();
    assert!(
        said.iter().any(|l| l.starts_with("project koji: ")
            && l.contains("removed agents.implementers = [\"sonnet-high\"]")),
        "{said:?}"
    );
    let (koji, _) = rig.shepherd.sheep("koji").unwrap();
    let written = koji.dogs.get(crate::shepherd::DOG).unwrap().as_map();
    let agents = written["agents"].as_object().unwrap();
    assert_eq!(agents.get("implementers"), None);
    assert_eq!(agents.get("reviewers"), None);
}

#[tokio::test]
async fn a_bare_upgrade_moves_a_listed_reviewers_list_into_a_listed_allow() {
    let shepherd = FakeShepherd::on(crate::shepherd::SHEP_VERSION);
    let mut table = loading(&shepherd);
    table["agents"]["allow"] = serde_json::json!(["sonnet-high"]);
    let rig = Rig::holding_on(shepherd, false, table).await;
    crate::agents::write_bound_stubs(
        &rig.home.join("agents"),
        "harness: claude-code\nmodel: claude-opus-5-5\neffort: high\n",
    );
    rig.cargo_installs("0.3.0", "0.19.0");

    let (ran, said) = rig.upgrade(Action::Installed).await;

    ran.unwrap();
    let wrote = (said.iter())
        .find(|l| l.starts_with("project koji: "))
        .unwrap_or_else(|| panic!("{said:?}"));
    assert!(
        wrote.contains("agents.allow = [\"sonnet-high\", \"standards\"], contributions.reviewers"),
        "{wrote}"
    );
    assert!(
        wrote.contains("contributions.reviewers = [\"standards\"]; removed "),
        "{wrote}"
    );
    assert!(
        wrote.contains("agents.reviewers = [\"standards\"]"),
        "{wrote}"
    );
    let (koji, _) = rig.shepherd.sheep("koji").unwrap();
    let written = koji.dogs.get(crate::shepherd::DOG).unwrap().as_map();
    let agents = written["agents"].as_object().unwrap();
    assert_eq!(
        agents["allow"],
        serde_json::json!(["sonnet-high", "standards"])
    );
    assert_eq!(agents.get("reviewers"), None);
    assert_eq!(
        written["contributions"]["reviewers"],
        serde_json::json!(["standards"]),
        "a contribution keeps the reviewers the project named"
    );
}

#[tokio::test]
async fn a_bare_upgrade_whose_settings_are_refused_stops_before_the_restarts() {
    // A table missing its kinds that does not load, so the check refuses the migrated one.
    let mut table = Map::new();
    table.insert("forge".into(), Value::String("shep-pm/koji".into()));
    let shepherd = FakeShepherd::on(crate::shepherd::SHEP_VERSION);
    let mut rig = Rig::holding_on(shepherd, false, table).await;
    rig.cargo_installs("0.3.0", "0.19.0");

    let (ran, said) = rig.upgrade(Action::Installed).await;

    let failed = ran.unwrap_err();
    assert!(failed.contains("project koji: not migrated"), "{failed}");
    assert!(failed.contains("shep kelpie settings migrate"), "{failed}");
    assert!(
        said.iter().all(|l| !l.contains("restarting onto it")),
        "{said:?}"
    );
    assert_eq!(rig.restarts(), Vec::<String>::new());
}
