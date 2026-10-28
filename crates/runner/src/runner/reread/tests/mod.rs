mod base;
mod contributions;
mod dependencies;
mod e2e;
mod hooks;
mod tickets;
mod triage;

use std::path::Path;

use crate::ports::Visibility;
use crate::settings::{Filing, GatewayName, Merging};
use crate::test::Rig;
use crate::webhook::{KelpieSettings, WebhookKind};

fn settings_with(rig: &Rig, edit: impl FnOnce(String) -> String) -> crate::settings::Settings {
    rig.edit_settings(edit);
    rig.settings()
}

#[test]
fn a_changed_setting_takes_effect_at_once() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    let next = settings_with(&rig, |s| {
        s.replace("merging = \"ask\"", "merging = \"auto\"")
            .replace("issues = \"ask\"", "issues = \"skip\"")
            .replace("fix_attempts = -1", "fix_attempts = 2")
            .replace("kickoff_hours = 8", "kickoff_hours = 6")
            .replace("active_items = 1", "active_items = 2")
    });
    let mut runner = runner.lock().unwrap();
    let line = runner.reread(next, rig.kelpie_settings()).unwrap();
    assert_eq!(
        line.as_deref(),
        Some("settings changed: git.merging, git.issues, ci, concurrency, pacing now in effect")
    );
    assert_eq!(runner.settings().concurrency.active_items.get(), 2);
    assert_eq!(runner.settings().git.merging, Merging::Auto);
    assert_eq!(runner.settings().git.issues, Filing::Skip);
    assert_eq!(runner.settings().ci.fix_attempts.cap(), Some(2));
    assert_eq!(runner.settings().pacing.kickoff_hours.get(), 6);
    assert_eq!(runner.status().merging, Merging::Auto);
}

#[test]
fn a_changed_budget_takes_effect_at_once() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    let next = settings_with(&rig, |s| s.replace("per_item_usd = 10", "per_item_usd = 4"));
    let mut runner = runner.lock().unwrap();
    assert_eq!(runner.settings().budget.per_item_usd.cents(), 1000);
    let line = runner.reread(next, rig.kelpie_settings()).unwrap();
    assert_eq!(
        line.as_deref(),
        Some("settings changed: budget now in effect")
    );
    assert_eq!(runner.settings().budget.per_item_usd.cents(), 400);
}

#[test]
fn a_changed_rulings_table_takes_effect_at_once() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    let next = settings_with(&rig, |s| {
        format!("{s}\n[app.dogs.kelpie.rulings]\npm_alone = [\"still-red\"]\n")
    });
    let mut runner = runner.lock().unwrap();
    let line = runner.reread(next, rig.kelpie_settings()).unwrap();
    assert_eq!(
        line.as_deref(),
        Some("settings changed: rulings now in effect")
    );
    let rulings = &runner.settings().rulings;
    assert!(rulings.pm_decides(crate::settings::RulingName::StillRed));
}

// A settings file with an entry the sandbox would refuse never becomes
// `Settings`, so `reread` is not handed it and the running ones stay.
#[test]
fn a_reread_with_a_bad_entry_keeps_the_running_settings() {
    let bad = "productionresultssa*.blob.core.windows.net";
    let worker = "allowed_domains = [\"crates.io\", \"index.crates.io\", \"static.crates.io\"]";
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    let before = runner.lock().unwrap().settings().clone();
    rig.edit_settings(|s| s.replace(worker, &format!("allowed_domains = [\"{bad}\"]")));
    let err = rig.try_settings().expect_err("the entry should be refused");
    assert!(err.to_string().contains(&format!("`{bad}`")), "{err}");
    assert_eq!(runner.lock().unwrap().settings(), &before);
}

#[test]
fn a_merge_kind_added_to_pm_alone_is_refused_and_the_settings_kept() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    let next = settings_with(&rig, |s| {
        format!("{s}\n[app.dogs.kelpie.rulings]\npm_alone = [\"merge\"]\n")
    });
    let mut runner = runner.lock().unwrap();
    let err = runner.reread(next, rig.kelpie_settings()).unwrap_err();
    assert!(err.to_string().contains("rulings.pm_alone"), "{err}");
    assert!(runner.settings().rulings.pm_decides.is_empty());
}

#[test]
fn the_same_settings_change_nothing() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    let mut runner = runner.lock().unwrap();
    assert_eq!(
        runner.reread(rig.settings(), rig.kelpie_settings()),
        Ok(None)
    );
}

#[test]
fn the_checkout_and_the_remote_wait_for_the_next_start() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    let repo = rig.repo().display().to_string();
    let next = settings_with(&rig, |s| {
        s.replace(
            "remote = \"shep-pm/shep\"",
            "remote = \"shep-pm/elsewhere\"",
        )
        .replace(&repo, "/srv/elsewhere")
        .replace("kickoff_hours = 8", "kickoff_hours = 6")
    });
    let mut runner = runner.lock().unwrap();
    let line = runner.reread(next, rig.kelpie_settings()).unwrap();
    assert_eq!(
        line.as_deref(),
        Some(
            "settings changed: pacing now in effect; git.checkout and git.remote from the \
             runner's next start"
        )
    );
    assert_eq!(runner.remote().as_str(), "shep-pm/shep");
    assert_eq!(runner.settings().git.checkout, rig.repo());
}

#[test]
fn a_changed_maintainer_alone_takes_effect_at_once() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    rig.maintainer("octocat");
    let mut runner = runner.lock().unwrap();
    let line = runner.reread(rig.settings(), rig.kelpie_settings());
    assert_eq!(
        line.unwrap().as_deref(),
        Some("settings changed: git.maintainer now in effect")
    );
    let login = runner.settings().git.maintainer.clone().unwrap();
    assert_eq!(login.as_str(), "octocat");
}

fn paddock_at(rig: &Rig, url: &str) {
    let webhook = format!(
        "[webhook]\nkind = \"ntfy\"\nurl = \"{}\"\n",
        Rig::WEBHOOK_URL
    );
    let gateway = format!("[gateways.paddock]\nurl = \"{url}\"\nkey_env = \"PADDOCK_KEY\"\n");
    rig.set_kelpie_settings(&format!("{webhook}{gateway}"));
}

fn url_of_paddock(runner: &crate::runner::Runner) -> String {
    let gateway = runner.gateways().get(&paddock()).unwrap();
    gateway.url.as_str().to_owned()
}

fn paddock() -> GatewayName {
    GatewayName::try_from("paddock".to_owned()).unwrap()
}

#[test]
fn a_changed_gateway_alone_waits_for_the_next_start() {
    let rig = Rig::new("shep");
    paddock_at(&rig, "http://gpu:8700");
    let runner = rig.open().unwrap();
    paddock_at(&rig, "http://elsewhere:8700");
    let mut runner = runner.lock().unwrap();
    let line = runner.reread(rig.settings(), rig.kelpie_settings());
    assert_eq!(
        line.unwrap().as_deref(),
        Some("settings changed; gateways from the runner's next start")
    );
    assert_eq!(url_of_paddock(&runner), "http://gpu:8700");
    drop(runner);

    let restarted = rig.open().unwrap();
    assert_eq!(
        url_of_paddock(&restarted.lock().unwrap()),
        "http://elsewhere:8700"
    );
}

// The gateway's key is read from `CARGO_PKG_NAME`, which `cargo test` sets for its tests.
const LOCAL: &str = "---\nrole: implementer\nharness: pi\nmodel: m\neffort: low\n\
                     gateway: paddock\ncontext: 65536\n---\n";

fn list_local(rig: &Rig) {
    rig.write_agent("local", LOCAL);
    rig.implementers(&["sonnet-high", "local"]);
}

fn paddock_keyed_by_cargo(rig: &Rig) {
    let webhook = format!(
        "[webhook]\nkind = \"ntfy\"\nurl = \"{}\"\n",
        Rig::WEBHOOK_URL
    );
    let gateway = "[gateways.paddock]\nurl = \"http://gpu:8700\"\nkey_env = \"CARGO_PKG_NAME\"\n";
    rig.set_kelpie_settings(&format!("{webhook}{gateway}"));
}

#[test]
fn a_gateway_taken_away_from_a_listed_agent_is_refused_as_a_start_refuses_it() {
    let rig = Rig::new("shep");
    paddock_keyed_by_cargo(&rig);
    list_local(&rig);
    let runner = rig.open().unwrap();
    let webhook = format!(
        "[webhook]\nkind = \"ntfy\"\nurl = \"{}\"\n",
        Rig::WEBHOOK_URL
    );
    rig.set_kelpie_settings(&webhook);
    let mut runner = runner.lock().unwrap();
    let err = runner.reread(rig.settings(), rig.kelpie_settings());
    let err = err.unwrap_err().to_string();
    assert!(
        err.contains("local: gateway paddock is not in kelpie's settings"),
        "{err}"
    );
    assert!(runner.gateways().get(&paddock()).is_some());
}

#[test]
fn held_agents_are_checked_against_the_new_settings_as_a_start_checks_them() {
    let rig = Rig::new("shep");
    paddock_keyed_by_cargo(&rig);
    list_local(&rig);
    let runner = rig.open().unwrap();
    // `local` runs on pi, which a start refuses beside guard hooks.
    let hook = "[[app.dogs.kelpie.worker.guard_hooks]]\nevent = \"PreToolUse\"\n\
                matcher = \"Bash\"\ncommand = \"~/check.sh\"\n";
    rig.edit_settings(|s| format!("{s}\n{hook}"));
    // A reviewer on `other`, a gateway the runner lacks, holds the new
    // list, which drops `local`.
    let reviewer = "---\nrole: reviewer\nharness: endpoint\ngateway: other\nmodel: coder\n\
                    context: 32768\n---\n";
    rig.write_agent("other", reviewer);
    rig.implementers(&["sonnet-high"]);
    rig.reviewers(&["claude", "other"]);
    let webhook = std::fs::read_to_string(rig.kelpie_settings_file()).unwrap();
    let other = "[gateways.other]\nurl = \"http://gpu:8701\"\nkey_env = \"CARGO_PKG_NAME\"\n";
    rig.set_kelpie_settings(&format!("{webhook}{other}"));
    let mut runner = runner.lock().unwrap();
    let err = runner.reread(rig.settings(), rig.kelpie_settings());
    let err = err.unwrap_err().to_string();
    assert!(
        err.contains("implementer local runs on pi, which cannot run `worker.guard_hooks`"),
        "{err}"
    );
    assert!(runner.settings().worker.guard_hooks.is_empty());
}

#[test]
fn agents_naming_a_gateway_added_with_them_wait_with_it_for_the_next_start() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    paddock_keyed_by_cargo(&rig);
    list_local(&rig);
    let mut runner = runner.lock().unwrap();
    let line = runner.reread(rig.settings(), rig.kelpie_settings());
    assert_eq!(
        line.unwrap().as_deref(),
        Some("settings changed; gateways and agents from the runner's next start")
    );
    assert_eq!(runner.settings().agents.implementers.len(), 1);
    drop(runner);

    let restarted = rig.open().unwrap();
    let restarted = restarted.lock().unwrap();
    assert_eq!(restarted.settings().agents.implementers.len(), 2);
    assert!(restarted.gateways().get(&paddock()).is_some());
}

#[test]
fn a_changed_reviewer_file_is_checked_as_a_start_checks_it_and_named() {
    let rig = Rig::new("shep");
    rig.reviewers(&["mine", "claude"]);
    let script = rig.home.path().join("review.sh");
    crate::test::write_script(&script, "#!/bin/sh\nexit 0\n");
    let define = |command: &str| {
        let text = format!("---\nrole: reviewer\nharness: command\ncommand: {command}\n---\n");
        rig.write_agent("mine", &text);
    };
    define(&script.display().to_string());
    let runner = rig.open().unwrap();
    let mut runner = runner.lock().unwrap();

    define("/nonexistent/review.sh");
    let err = runner
        .reread(rig.settings(), rig.kelpie_settings())
        .unwrap_err();
    assert!(
        err.to_string()
            .starts_with("setting `agents.reviewers`: mine: cannot run /nonexistent"),
        "{err}"
    );

    define("~/review.sh");
    let line = runner
        .reread(rig.settings(), rig.kelpie_settings())
        .unwrap();
    assert_eq!(
        line.as_deref(),
        Some("settings changed: agents now in effect"),
        "the same command, spelt from the home folder, in an edited file"
    );

    std::fs::remove_file(rig.paths().agents.join("mine.md")).unwrap();
    let err = runner
        .reread(rig.settings(), rig.kelpie_settings())
        .unwrap_err();
    assert!(
        err.to_string().contains("mine, which has no agent file"),
        "{err}"
    );
}

#[test]
fn a_new_webhook_takes_effect_at_once() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    let text = "[webhook]\nkind = \"discord\"\nurl = \"https://discord.example/h\"\n";
    let kelpie = KelpieSettings::from_section(text).unwrap();
    let mut runner = runner.lock().unwrap();
    let line = runner.reread(rig.settings(), kelpie).unwrap();
    assert_eq!(
        line.as_deref(),
        Some("settings changed: webhook now in effect")
    );
    assert_eq!(runner.webhook.as_ref().unwrap().kind, WebhookKind::Discord);
}

#[test]
fn a_webhook_taken_away_takes_effect_at_once() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    let mut runner = runner.lock().unwrap();
    let line = runner
        .reread(rig.settings(), KelpieSettings::default())
        .unwrap();
    assert_eq!(
        line.as_deref(),
        Some("settings changed: webhook now in effect")
    );
    assert!(runner.webhook.is_none());
}

#[test]
fn a_change_that_does_not_hold_keeps_the_settings_the_runner_has() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    rig.forge.set_visibility(Visibility::Private);
    rig.coderabbit_on();
    let next = rig.settings();
    let mut runner = runner.lock().unwrap();
    let err = runner
        .reread(next, rig.kelpie_settings())
        .unwrap_err()
        .to_string();
    assert!(
        err.starts_with("setting `agents.reviewers`: coderabbit"),
        "{err}"
    );
    let listed = runner
        .settings()
        .agents
        .reviewers
        .clone()
        .unwrap_or_default();
    assert!(!listed.iter().any(|name| name.as_str() == "coderabbit"));
    let missing = settings_with(&rig, |s| {
        s.replace(
            crate::test::RIG_REVIEWERS_AND_CODERABBIT,
            crate::test::RIG_REVIEWERS,
        )
        .replace(
            "build_env = {}\n",
            "instructions_file = \"/nonexistent/w.md\"\n",
        )
    });
    let err = runner
        .reread(missing, rig.kelpie_settings())
        .unwrap_err()
        .to_string();
    assert!(err.contains("worker.instructions_file"), "{err}");
    assert_eq!(runner.settings().worker.instructions_file, None);
    assert!(Path::new(&rig.settings_file()).exists());
}

// An implementer on a local model, which the project does not list
const CODER: &str = "---\nrole: implementer\nharness: stand-in\nmodel: qwen3-coder\n\
                      effort: low\nusage: none\n---\n";

#[test]
fn every_implementer_agents_label_is_made_on_open_and_none_on_a_new_list_only() {
    let rig = Rig::new("shep");
    rig.forge.set_repo_labels(&["bug", "Agent:Sonnet-high"]);
    let runner = rig.open().unwrap();
    let made = ["bug", "Agent:Sonnet-high", "agent:opus-high"];
    assert_eq!(
        rig.forge.repo_labels_now(),
        made,
        "sonnet-high's is there already"
    );

    rig.implementers(&["sonnet-high", "opus-high"]);
    let mut runner = runner.lock().unwrap();
    runner
        .reread(rig.settings(), rig.kelpie_settings())
        .unwrap();
    assert_eq!(rig.forge.repo_labels_now(), made);

    rig.implementers(&["opus-high"]);
    runner
        .reread(rig.settings(), rig.kelpie_settings())
        .unwrap();
    assert_eq!(rig.forge.repo_labels_now(), made, "none is removed");
}

#[test]
fn an_unlisted_implementers_agent_label_is_made_too() {
    let rig = Rig::new("shep");
    rig.write_agent("coder", CODER);
    rig.open().unwrap();
    let labels = rig.forge.repo_labels_now();
    for label in ["agent:coder", "agent:opus-high", "agent:sonnet-high"] {
        assert!(labels.contains(&label.to_owned()), "{label}: {labels:?}");
    }
}

#[test]
fn a_fresh_repo_gets_every_implementers_label_when_the_runner_opens() {
    let rig = Rig::new("shep");
    rig.open().unwrap();
    assert_eq!(
        rig.forge.repo_labels_now(),
        ["agent:opus-high", "agent:sonnet-high"]
    );
}

#[test]
fn a_fallback_after_alone_is_no_change() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    let next = settings_with(&rig, |s| {
        s.replace(
            "\nimplementers = ",
            "\nfallback_after = 15\nimplementers = ",
        )
    });
    let mut runner = runner.lock().unwrap();
    let line = runner.reread(next, rig.kelpie_settings()).unwrap();
    assert_eq!(line, None, "nothing reads it");
}
