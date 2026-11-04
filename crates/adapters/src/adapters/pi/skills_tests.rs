//! The skills a pi call loads from the plugins kelpie wrote for its steps

use std::path::Path;

use super::tests::{FRESH_ID, URL, WORKER_ID, World, id, strings, values};
use super::*;
use crate::adapters::skills;
use crate::test::NO_PROGRAM;

// The names of the skill folders in `dir`, sorted.
fn held(dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    found.sort();
    found
}

#[test]
fn a_worker_loads_only_the_skills_kelpie_wrote_for_its_steps() {
    let w = World::new();
    let skills =
        crate::skills::Skills::load(&crate::settings::StepSkills::default(), &w.path("skills"));
    let mut call = w.fenced(URL, Path::new("/k/kelpie"), Session::New(id(WORKER_ID)));
    call.plugin_dirs = skills.plugin_dirs().to_vec();
    call.prompt = "/mattpocock:implement Implement issue #7".into();
    let argv = strings(&call);
    let given = values(&argv, "--skill");
    // pi is given every skill folder of the plugins kelpie wrote, and no
    // other, beside its own step skill's folder.
    for dir in skills::skill_dirs(&call) {
        assert!(given.contains(&dir.to_str().unwrap()), "{argv:?}");
    }
    assert_eq!(given.len(), skills::skill_dirs(&call).len() + 1, "{argv:?}");

    assert_eq!(
        held(&w.path("skills/mattpocock/skills")),
        [
            "code-review",
            "codebase-design",
            "diagnosing-bugs",
            "implement",
            "pr",
            "tdd",
        ]
    );
}

#[test]
fn a_steps_skill_runs_as_pis_own_slash_command() {
    let w = World::new();
    let plugin = w.path("skills/mattpocock");
    std::fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
    std::fs::create_dir_all(plugin.join("skills/implement")).unwrap();
    std::fs::write(
        plugin.join(".claude-plugin/plugin.json"),
        r#"{"name":"mattpocock"}"#,
    )
    .unwrap();
    std::fs::write(plugin.join("skills/implement/SKILL.md"), "---\n").unwrap();
    let mut call = w.call(URL, Role::Worker, Session::New(id(FRESH_ID)));
    call.plugin_dirs = vec![plugin.clone()];
    call.prompt = "/mattpocock:implement Implement issue #7".into();
    let argv = strings(&call);
    assert_eq!(argv.last().unwrap(), "/skill:implement Implement issue #7");
    let at = argv.iter().position(|a| a == "--skill").unwrap();
    assert_eq!(
        argv[at + 1],
        plugin.join("skills/implement").to_str().unwrap()
    );

    call.prompt = "/mattpocock:tdd Implement issue #7".into();
    let argv = strings(&call);
    assert_eq!(argv.last().unwrap(), "/mattpocock:tdd Implement issue #7");
    assert!(!argv.iter().any(|a| a == "--skill"));
}

// A plugin `name` holding a skill folder for each of `skills`
fn plugin(w: &World, name: &str, skills: &[&str]) -> PathBuf {
    let dir = w.path(&format!("plugins/{name}"));
    std::fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
    std::fs::write(
        dir.join(".claude-plugin/plugin.json"),
        format!(r#"{{"name":"{name}"}}"#),
    )
    .unwrap();
    for skill in skills {
        std::fs::create_dir_all(dir.join("skills").join(skill)).unwrap();
        std::fs::write(dir.join("skills").join(skill).join("SKILL.md"), "---\n").unwrap();
    }
    dir
}

#[test]
fn every_plugins_skills_load_after_the_steps_own_skill_folder() {
    let w = World::new();
    let steps = plugin(&w, "mattpocock", &["implement", "tdd"]);
    let style = plugin(&w, "style", &["house"]);
    let mut call = w.fenced(URL, Path::new("/k/kelpie"), Session::New(id(WORKER_ID)));
    call.plugin_dirs = vec![steps.clone(), style.clone()];
    call.prompt = "/mattpocock:implement Implement issue #7".into();
    let argv = strings(&call);
    assert_eq!(
        values(&argv, "--skill"),
        [
            steps.join("skills/implement").to_str().unwrap(),
            steps.join("skills").to_str().unwrap(),
            style.join("skills").to_str().unwrap(),
        ]
    );
    let no_skills = argv.iter().position(|a| a == "--no-skills").unwrap();
    let first = argv.iter().position(|a| a == "--skill").unwrap();
    assert!(no_skills < first, "{argv:?}");
}

#[test]
fn the_wording_skill_loads_with_the_steps_skills_on_a_worker_call() {
    let w = World::new();
    let skills =
        crate::skills::Skills::load(&crate::settings::StepSkills::default(), &w.path("skills"));
    let mut call = w.fenced(URL, Path::new("/k/kelpie"), Session::New(id(WORKER_ID)));
    call.plugin_dirs = skills.plugin_dirs().to_vec();
    call.prompt = "/mattpocock:implement Implement issue #7".into();
    let wording = w.path("skills/kelpie-wording/skills");
    assert!(wording.join("wording/SKILL.md").is_file());
    assert!(
        values(&strings(&call), "--skill").contains(&wording.to_str().unwrap()),
        "{:?}",
        strings(&call)
    );
}

#[test]
fn a_call_with_no_tools_loads_its_steps_skill_and_no_other() {
    let w = World::new();
    let steps = plugin(&w, "mattpocock", &["implement"]);
    let style = plugin(&w, "style", &["house"]);
    let mut call = w.call(URL, Role::Reviewer, Session::New(id(FRESH_ID)));
    call.plugin_dirs = vec![steps.clone(), style];
    call.prompt = "/mattpocock:implement Implement issue #7".into();
    let argv = strings(&call);
    assert!(argv.iter().any(|a| a == "--no-tools"), "{argv:?}");
    assert_eq!(
        values(&argv, "--skill"),
        [steps.join("skills/implement").to_str().unwrap()]
    );
}

#[test]
fn every_kind_of_call_that_loads_a_plugins_skills_keeps_read_and_trusts_no_project() {
    let w = World::new();
    let steps = plugin(&w, "mattpocock", &["implement"]);
    let style = plugin(&w, "style", &["house"]);
    let kinds = [
        Tools::Work,
        Tools::Review,
        Tools::Answer,
        Tools::Pm,
        Tools::Retro,
    ];
    for tools in kinds {
        for reads in [false, true] {
            let mut call = w.call(URL, Role::Reviewer, Session::New(id(FRESH_ID)));
            call.tools = tools;
            call.plugin_dirs = vec![steps.clone(), style.clone()];
            call.prompt = "/mattpocock:implement Implement issue #7".into();
            if reads {
                call.reach.read = vec![w.path("extra")];
            }
            let argv = strings(&call);
            let which = format!("{tools:?}, reading {reads}: {argv:?}");
            assert!(!argv.iter().any(|a| a == "--approve"), "{which}");
            assert!(argv.iter().any(|a| a == "--no-approve"), "{which}");
            if values(&argv, "--skill").len() > 1 {
                let [names] = values(&argv, "--tools").try_into().expect(&which);
                assert!(names.split(',').any(|t| t == "read"), "{which}");
            }
        }
    }
}

#[test]
fn the_sandbox_reads_every_plugin_folder_a_call_loads_skills_from() {
    let w = World::new();
    let steps = plugin(&w, "mattpocock", &["implement"]);
    let style = plugin(&w, "style", &["house"]);
    let mut call = w.fenced(URL, Path::new("/k/kelpie"), Session::New(id(WORKER_ID)));
    call.plugin_dirs = vec![steps.clone(), style.clone()];
    let upstream = PiCli::on(&Base::default())
        .upstream(server(&call).unwrap())
        .unwrap();
    let files = Files::of(&call);
    let socket = w.path("forwarder.sock");
    let policy = policy(&call, &files, &upstream, &socket, &NO_PROGRAM, &[]);
    for dir in skills::skill_dirs(&call) {
        let covered = policy.read.iter().any(|read| dir.starts_with(read));
        assert!(covered, "{}", dir.display());
    }
    assert!(policy.read.contains(&steps) && policy.read.contains(&style));
}
