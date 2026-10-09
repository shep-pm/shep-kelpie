use std::fs;
use std::path::Path;

use serde_json::json;

use super::*;
use crate::ports::{AgentCall, Checks, Role};
use crate::runner::step;
use crate::test::{Rig, Scripted};

// Appends a `[skills]` table to the rig's settings, read when a runner next opens
fn choose(rig: &Rig, table: &str) {
    rig.edit_settings(|s| format!("{s}\n[app.dogs.kelpie.skills]\n{table}"));
}

// A plugin folder whose manifest names it `name`
fn plugin_folder(at: &Path, name: &str) {
    fs::create_dir_all(at.join(".claude-plugin")).unwrap();
    let manifest = json!({ "name": name }).to_string();
    fs::write(at.join(".claude-plugin/plugin.json"), manifest).unwrap();
}

fn skill_folder(at: &Path, text: &str) {
    fs::create_dir_all(at.join("references")).unwrap();
    fs::write(at.join("SKILL.md"), text).unwrap();
    fs::write(at.join("references/more.md"), "more\n").unwrap();
}

// Asserts `prompt` runs `command` under kelpie's headless rules, then asks `then`
#[track_caller]
fn assert_invoked(prompt: &str, command: &str, then: &str) {
    let rules = format!("{command} Kelpie runs this skill headless. Where the skill");
    assert!(prompt.starts_with(&rules), "{prompt}");
    assert!(prompt.contains(&format!("\n{then}")), "{prompt}");
}

fn call_of(rig: &Rig, role: Role) -> AgentCall {
    rig.claude
        .all_calls()
        .into_iter()
        .find(|c| c.role == role)
        .expect("the call ran")
}

#[test]
fn each_step_runs_its_default_skill_from_kelpies_own_copy() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    let plugin = rig.paths().skills.join("mattpocock");
    let worker = call_of(&rig, Role::Worker);
    let (implement, first) = ("/mattpocock:implement", "Your work item is issue #7: ");
    assert_invoked(&worker.prompt, implement, first);
    assert!(worker.prompt.contains("Run no /code-review"));
    assert_eq!(worker.plugin_dirs, std::slice::from_ref(&plugin));
    // A reviewer's prompt is its agent file's body, which runs no skill.
    let reviewer = call_of(&rig, Role::Reviewer);
    assert!(
        reviewer.prompt.starts_with("Review the change against"),
        "{}",
        reviewer.prompt
    );
    assert!(reviewer.plugin_dirs.is_empty());
    let seen = rig.claude.all_seen();
    let reviewed = seen.iter().find(|s| s.call.role == Role::Reviewer).unwrap();
    assert_eq!(
        reviewed.settings["permissions"]["deny"],
        crate::trim::deny_with_trim(&["Agent", "Task", "Bash"])
    );
    for step in Step::ALL {
        let skill = plugin.join("skills").join(step.default_skill());
        assert!(skill.join("SKILL.md").is_file(), "{step}");
    }

    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".into()]));
    rig.verdict(&runner);
    rig.claude.script([Scripted::Push("fix.txt", "fixed\n")]);
    step(&runner).unwrap();
    let fix = rig.claude.calls().pop().unwrap();
    let (ci, red) = (
        "/mattpocock:diagnosing-bugs",
        "CI failed on your pull request #71",
    );
    assert_invoked(&fix.prompt, ci, red);

    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["skills"][4],
        json!({ "step": "ci_fix", "skill": "/mattpocock:diagnosing-bugs", "fallback": null })
    );
    assert_eq!(status["skills"].as_array().unwrap().len(), Step::ALL.len());
}

#[test]
fn a_projects_skill_folder_replaces_the_default() {
    let (rig, runner, _) = Rig::with_pull_request_set("shep", |rig| {
        let folder = rig.home.path().join("house/build-it");
        skill_folder(&folder, "---\nname: build-it\n---\nBuild it.\n");
        choose(
            rig,
            &format!(
                "implement = {{ kind = \"path\", path = \"{}\" }}\n",
                folder.display()
            ),
        );
    });
    let worker = call_of(&rig, Role::Worker);
    let first = "Your work item is issue #7: ";
    assert_invoked(&worker.prompt, "/kelpie-implement:build-it", first);
    let own = rig.paths().skills.join("implement");
    assert!(
        worker.plugin_dirs.contains(&own),
        "{:?}",
        worker.plugin_dirs
    );
    assert_eq!(
        fs::read_to_string(own.join("skills/build-it/references/more.md")).unwrap(),
        "more\n"
    );
    let manifest = fs::read_to_string(own.join(".claude-plugin/plugin.json")).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&manifest).unwrap()["name"],
        "kelpie-implement"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["skills"][2]["skill"], "/kelpie-implement:build-it");
}

#[test]
fn a_skill_in_a_projects_plugin_replaces_the_default() {
    let (rig, _runner, _) = Rig::with_pull_request_set("shep", |rig| {
        let plugin = rig.home.path().join("house-plugin");
        plugin_folder(&plugin, "house");
        skill_folder(&plugin.join("skills/build-hard"), "Build hard.\n");
        choose(
            rig,
            &format!(
                "implement = {{ kind = \"plugin\", plugin = \"{}\", skill = \"build-hard\" }}\n",
                plugin.display()
            ),
        );
    });
    let worker = call_of(&rig, Role::Worker);
    let first = "Your work item is issue #7: ";
    assert_invoked(&worker.prompt, "/house:build-hard", first);
    let plugin = rig.home.path().join("house-plugin");
    assert!(
        worker.plugin_dirs.contains(&plugin),
        "{:?}",
        worker.plugin_dirs
    );
}

#[test]
fn a_skill_that_cannot_load_falls_back_to_kelpies_prompt_and_says_why() {
    let rig = Rig::new("shep");
    let gone = rig.home.path().join("gone");
    let plugin = rig.home.path().join("empty-plugin");
    plugin_folder(&plugin, "e");
    choose(
        &rig,
        &format!(
            "implement = {{ kind = \"path\", path = \"{}\" }}\n\
             ci_fix = {{ kind = \"plugin\", plugin = \"{}\", skill = \"debug\" }}\n",
            gone.display(),
            plugin.display()
        ),
    );
    let runner = rig.open().unwrap();
    let notices: Vec<String> = runner.lock().unwrap().skill_notices().collect();
    assert_eq!(
        notices,
        [
            format!(
                "the implement step's skill cannot load, so it runs kelpie's own prompt: \
                 {} holds no SKILL.md",
                gone.display()
            ),
            format!(
                "the ci_fix step's skill cannot load, so it runs kelpie's own prompt: \
                 the plugin in {} has no skill debug",
                plugin.display()
            ),
        ]
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["skills"][2]["skill"], json!(null));
    assert_eq!(
        status["skills"][2]["fallback"],
        format!("{} holds no SKILL.md", gone.display())
    );

    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap();
    let worker = call_of(&rig, Role::Worker);
    assert!(
        worker.prompt.starts_with("Your work item is issue #7: "),
        "{}",
        worker.prompt
    );
}

#[test]
fn a_step_set_to_none_runs_kelpies_prompt_with_no_notice() {
    let (rig, runner, _) = Rig::with_pull_request_set("shep", |rig| {
        choose(rig, "implement = { kind = \"none\" }\n");
    });
    let worker = call_of(&rig, Role::Worker);
    assert!(
        worker.prompt.starts_with("Your work item is issue #7: "),
        "{}",
        worker.prompt
    );
    assert_eq!(runner.lock().unwrap().skill_notices().count(), 0);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["skills"][2],
        json!({ "step": "implement", "skill": null, "fallback": null })
    );
}

#[test]
fn a_misspelt_step_or_skill_name_stops_the_runner() {
    let rig = Rig::new("shep");
    choose(&rig, "reveiw = { kind = \"none\" }\n");
    let err = rig.open().unwrap_err().to_string();
    assert!(err.contains("reveiw"), "{err}");

    let rig = Rig::new("shep");
    choose(
        &rig,
        "ci_fix = { kind = \"plugin\", plugin = \"/p\", skill = \"../up\" }\n",
    );
    let err = rig.open().unwrap_err().to_string();
    assert!(err.contains("must be a skill's name"), "{err}");
}

#[test]
fn a_changed_skill_takes_effect_without_a_restart() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    choose(&rig, "ci_fix = { kind = \"none\" }\n");
    let line = runner
        .lock()
        .unwrap()
        .reread(rig.settings(), rig.kelpie_settings())
        .unwrap();
    assert_eq!(
        line.as_deref(),
        Some("settings changed: skills now in effect")
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["skills"][4]["skill"], json!(null));
    assert_eq!(status["skills"][2]["skill"], "/mattpocock:implement");
}

#[test]
fn a_folder_of_the_projects_inside_kelpies_own_stops_the_runner() {
    let rig = Rig::new("shep");
    let mine = rig.paths().skills.join("ci_fix");
    skill_folder(&mine, "Review.\n");
    choose(
        &rig,
        "ci_fix = { kind = \"path\", path = \"skills/ci_fix\" }\n",
    );
    let err = rig.open().unwrap_err().to_string();
    assert!(
        err.starts_with("setting `skills`: the ci_fix step's "),
        "{err}"
    );
    assert!(mine.join("SKILL.md").is_file(), "the folder is left alone");

    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    choose(
        &rig,
        "ci_fix = { kind = \"plugin\", plugin = \"skills/mattpocock\", skill = \"x\" }\n",
    );
    let refused = runner
        .lock()
        .unwrap()
        .reread(rig.settings(), rig.kelpie_settings());
    assert!(refused.is_err(), "{refused:?}");
}

#[test]
fn a_projects_plugin_may_not_take_kelpies_own_names() {
    for name in ["mattpocock", "kelpie-ci_fix"] {
        let rig = Rig::new("shep");
        let plugin = rig.home.path().join("theirs");
        plugin_folder(&plugin, name);
        skill_folder(&plugin.join("skills/code-review"), "Review.\n");
        choose(
            &rig,
            &format!(
                "ci_fix = {{ kind = \"plugin\", plugin = \"{}\", skill = \"code-review\" }}\n",
                plugin.display()
            ),
        );
        let runner = rig.open().unwrap();
        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["skills"][4]["skill"], json!(null), "{name}");
        let fallback = status["skills"][4]["fallback"].as_str().unwrap();
        assert!(fallback.ends_with(&format!("is named {name}, which is kelpie's own")));
    }
}
