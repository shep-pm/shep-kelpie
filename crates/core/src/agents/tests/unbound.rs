use super::*;
use crate::agents::baseline::Baseline;
use crate::agents::front;

// A baseline with no binding keys, so a stub that extends it binds nothing.
const SCRIBE: Baseline = Baseline {
    name: "scribe",
    text: "---\nrole: pm\n---\nYou keep the project's notes.\n",
};

// The keys a binding is made of, which a baseline sets none of.
const KEYS: [&str; 10] = [
    "harness",
    "model",
    "effort",
    "second_look",
    "usage",
    "url",
    "gateway",
    "context",
    "tools",
    "lease",
];

const STUB: &str = "---\nextends: kelpie:scribe\n---\n";

#[test]
fn a_stub_that_binds_nothing_is_unbound_and_leaves_the_other_files_loading() {
    let dir = folder(&[("notes.md", STUB), ("qwen.md", QWEN)]);
    let agents = Agents::load_with(dir.path(), &[SCRIBE]).unwrap();
    assert!(agents.get(&name("notes")).is_none());
    assert!(agents.get(&name("qwen")).is_some());
    let unbound = agents.unbound();
    assert_eq!(unbound.len(), 1);
    assert_eq!(unbound[0].name, name("notes"));
    assert_eq!(unbound[0].path, dir.path().join("notes.md"));
    assert_eq!(unbound[0].role, Role::Pm);
    assert_eq!(
        unbound[0].command(),
        "shep kelpie agents new pm --name notes"
    );
}

#[test]
fn a_stub_that_adds_harness_model_and_effort_loads_with_the_baselines_prompt() {
    let bound = "---\nextends: kelpie:scribe\nharness: claude-code\nmodel: claude-opus-5-5\n\
                 effort: medium\n---\n";
    let dir = folder(&[("notes.md", bound)]);
    let agents = Agents::load_with(dir.path(), &[SCRIBE]).unwrap();
    let notes = agents.get(&name("notes")).unwrap();
    assert_eq!(notes.role, Role::Pm);
    assert_eq!(pair(notes), ("claude-opus-5-5", Effort::Medium));
    assert_eq!(
        notes.prompt.as_deref(),
        Some("You keep the project's notes.")
    );
    assert!(agents.unbound().is_empty());
}

#[test]
fn the_review_bots_and_the_codex_agent_still_load_whole() {
    let agents = Agents::load(tempfile::tempdir().unwrap().path()).unwrap();
    for whole in ["coderabbit", "cubic", "codex"] {
        assert!(agents.get(&name(whole)).is_some(), "{whole}");
    }
    assert!(agents.unbound().is_empty());
}

#[test]
fn a_file_with_no_role_still_stops_the_folder() {
    let err = refused(&[("notes.md", "---\nmodel: x\n---\n"), ("qwen.md", QWEN)]);
    assert!(err.contains("notes.md: missing field `role`"), "{err}");
}

#[test]
fn extends_resolves_in_the_whole_agents_and_in_the_baselines() {
    let both = "---\nextends: kelpie:nope\n---\n";
    let err = refused(&[("mine.md", both)]);
    assert!(
        err.contains("defect-hunter") && err.contains("codex"),
        "{err}"
    );
}

#[test]
fn the_command_is_named_for_an_unbound_agent_and_for_a_bare_baseline_with_no_file() {
    let bare = Baseline {
        name: "pm",
        text: "---\nrole: pm\n---\nYou manage.\n",
    };
    let dir = folder(&[("boss.md", "---\nrole: pm\n---\nYou manage.\n")]);
    let agents = Agents::load_over(dir.path(), &[bare]).unwrap();
    assert_eq!(
        agents.unbound_command(&name("boss")).as_deref(),
        Some("shep kelpie agents new pm --name boss")
    );
    assert_eq!(
        agents.unbound_command(&name("pm")).as_deref(),
        Some("shep kelpie agents new pm")
    );
}

#[test]
fn a_baseline_is_a_role_and_a_prompt_and_no_binding_key() {
    for shipped in baseline::ALL {
        let name = shipped.name;
        assert!(shipped.role().is_some(), "{name} names a role");
        let (front, body) = front::split(shipped.text).unwrap();
        assert!(!body.trim().is_empty(), "{name} has a prompt");
        for key in KEYS {
            let set = front
                .lines()
                .any(|line| line.starts_with(&format!("{key}:")));
            assert!(!set, "{name} sets `{key}`");
        }
    }
}

#[test]
fn a_baseline_binds_nothing_so_the_book_names_it_and_no_agent_runs_without_a_file() {
    let agents = Agents::load(folder(&[]).path()).unwrap();
    assert_eq!(agents.bare_baselines().len(), baseline::ALL.len());
    for shipped in baseline::ALL {
        let name = name(shipped.name);
        assert!(agents.get(&name).is_none(), "{name}");
        assert!(agents.unbound_command(&name).is_some(), "{name}");
    }
    let bound = agents.bind_all(OPUS_HIGH);
    assert!(bound.get(&name("pm")).is_some());
}

#[test]
fn a_file_over_a_bare_baseline_binds_it_and_leaves_the_baseline_listed_only_while_unbound() {
    let bare = Baseline {
        name: "scribe",
        text: "---\nrole: pm\n---\nYou keep the notes.\n",
    };
    let loose = Agents::load_over(folder(&[]).path(), &[bare]).unwrap();
    assert_eq!(loose.bare_baselines(), [bare]);
    let bound = "---\nextends: kelpie:scribe\nharness: claude-code\nmodel: m\neffort: low\n---\n";
    let dir = folder(&[("scribe.md", bound)]);
    let agents = Agents::load_over(dir.path(), &[bare]).unwrap();
    assert!(agents.get(&name("scribe")).is_some());
    assert_eq!(agents.unbound_command(&name("scribe")), None);
}
