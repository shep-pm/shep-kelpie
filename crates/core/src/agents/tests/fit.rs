//! The keys an implementer or reviewer file is chosen by: `description`, `kinds`, `tier`

use super::{OPUS_HIGH, QWEN, folder, name, refused};
use crate::agents::baseline::Baseline;
use crate::agents::fit::Tier;
use crate::agents::{Agent, Agents, Role};
use crate::settings::NonBlank;

const KEYS: &str = "description: Writes the docs\nkinds: [docs, mechanical-2]\ntier: light\n";

fn implementer(keys: &str) -> String {
    format!("---\nrole: implementer\n{OPUS_HIGH}{keys}---\n")
}

fn reviewer(keys: &str) -> String {
    format!("---\nrole: reviewer\n{OPUS_HIGH}{keys}---\nRead it.\n")
}

fn bot(keys: &str) -> String {
    format!("---\nrole: reviewer\nharness: bot\nbot: cubic\nreviews: 5\nhours: 24\n{keys}---\n")
}

fn kinds(agent: &Agent) -> Vec<&str> {
    agent
        .fit
        .kinds
        .as_ref()
        .expect("it names kinds")
        .iter()
        .collect()
}

fn description(agent: &Agent) -> Option<&str> {
    agent.fit.description.as_ref().map(NonBlank::as_str)
}

#[test]
fn a_file_with_the_keys_loads_with_them_on_its_agent() {
    let dir = folder(&[
        ("builder.md", &implementer(KEYS)),
        ("looker.md", &reviewer(KEYS)),
        ("cubic.md", &bot(KEYS)),
    ]);
    let agents = Agents::load(dir.path()).unwrap();

    for file in ["builder", "looker", "cubic"] {
        let agent = agents.get(&name(file)).unwrap();
        assert_eq!(description(agent), Some("Writes the docs"), "{file}");
        assert_eq!(kinds(agent), ["docs", "mechanical-2"], "{file}");
        assert_eq!(agent.fit.tier(), Tier::Light, "{file}");
    }
    assert_eq!(agents.get(&name("looker")).unwrap().role, Role::Reviewer);
}

#[test]
fn a_file_without_the_keys_loads_with_no_description_or_kinds_and_tier_standard() {
    let dir = folder(&[
        ("builder.md", &implementer("")),
        ("looker.md", &reviewer("")),
    ]);
    let agents = Agents::load(dir.path()).unwrap();

    for file in ["builder", "looker"] {
        let agent = agents.get(&name(file)).unwrap();
        assert_eq!(description(agent), None, "{file}");
        assert_eq!(agent.fit.kinds, None, "{file}");
        assert_eq!(agent.fit.tier(), Tier::Standard, "{file}");
    }
    assert!(Tier::Light < Tier::Standard && Tier::Standard < Tier::Heavy);
}

#[test]
fn a_blank_description_is_refused_naming_its_line() {
    let err = refused(&[("mine.md", &implementer("description: \"  \"\n"))]);

    assert!(err.contains("`description`"), "{err}");
    assert!(err.contains("must not be blank"), "{err}");
    assert!(err.ends_with("at line 6, column 14"), "{err}");
}

#[test]
fn a_tier_other_than_light_standard_or_heavy_is_refused_naming_the_three() {
    let err = refused(&[("mine.md", &implementer("tier: huge\n"))]);

    assert!(err.contains("`tier`"), "{err}");
    for tier in ["light", "standard", "heavy"] {
        assert!(err.contains(tier), "{err}");
    }
    assert!(err.contains("at line 6"), "{err}");
}

#[test]
fn bad_kinds_are_refused_naming_the_key() {
    for bad in [
        "[]",
        "[Docs]",
        "\n  - Docs",
        "\n- Docs",
        "\n# the kinds\n- Docs",
        "[docs, docs]",
        "[\"two words\"]",
        "[\"\"]",
    ] {
        let err = refused(&[("mine.md", &implementer(&format!("kinds: {bad}\n")))]);
        assert!(err.contains("`kinds`"), "{bad}: {err}");
        assert!(err.contains("at line "), "{bad}: {err}");
    }
}

#[test]
fn the_other_roles_refuse_the_keys_by_name() {
    let session =
        |role: &str, key: &str| format!("---\nrole: {role}\n{OPUS_HIGH}{key}---\nDecide.\n");
    for role in ["pm", "representative", "triager"] {
        for key in ["description: x\n", "kinds: [docs]\n", "tier: light\n"] {
            let err = refused(&[("mine.md", &session(role, key))]);
            let name = key.split(':').next().unwrap();
            assert!(
                err.contains(&format!("unknown field `{name}`")),
                "{role}: {err}"
            );
        }
    }
}

#[test]
fn a_file_with_limits_is_refused_as_an_unknown_field() {
    let err = refused(&[("mine.md", &implementer("limits: 5\n"))]);

    assert!(err.contains("unknown field `limits`"), "{err}");
}

#[test]
fn a_file_inherits_the_keys_of_what_it_extends_and_its_own_replace_them() {
    let parent = implementer("description: Parent\nkinds: [docs]\ntier: heavy\n");
    let dir = folder(&[
        ("parent.md", &parent),
        ("same.md", "---\nextends: parent\n---\n"),
        (
            "own.md",
            "---\nextends: parent\nkinds: [ci, chore]\ntier: light\n---\n",
        ),
    ]);
    let agents = Agents::load(dir.path()).unwrap();

    let same = agents.get(&name("same")).unwrap();
    assert_eq!(description(same), Some("Parent"));
    assert_eq!(same.fit.tier(), Tier::Heavy);
    let own = agents.get(&name("own")).unwrap();
    assert_eq!(description(own), Some("Parent"));
    assert_eq!(kinds(own), ["ci", "chore"]);
    assert_eq!(own.fit.tier(), Tier::Light);
}

#[test]
fn a_file_with_a_role_and_a_description_but_no_harness_still_binds_nothing() {
    let scribe = Baseline {
        name: "scribe",
        text: "---\nrole: implementer\n---\nYou build.\n",
    };
    let stub = "---\nextends: kelpie:scribe\ndescription: Builds things\n---\n";
    let dir = folder(&[("notes.md", stub), ("qwen.md", QWEN)]);
    let agents = Agents::load_with(dir.path(), &[scribe]).unwrap();

    assert!(agents.get(&name("notes")).is_none());
    assert_eq!(agents.unbound().len(), 1);
    assert!(agents.get(&name("qwen")).is_some());
}
