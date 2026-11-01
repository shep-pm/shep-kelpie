use super::*;

const PARENT: &str = "---\nrole: implementer\nharness: claude-code\nmodel: claude-opus-5-5\n\
                      effort: high\n---\nParent body.\n";

fn load(files: &[(&str, &str)]) -> Agents {
    let dir = folder(files);
    Agents::load(dir.path()).unwrap()
}

#[test]
fn a_file_with_extends_and_no_body_takes_its_parents_body() {
    let agents = load(&[
        ("base.md", PARENT),
        ("light.md", "---\nextends: base\neffort: medium\n---\n"),
    ]);
    let light = agents.get(&name("light")).unwrap();
    assert_eq!(light.prompt.as_deref(), Some("Parent body."));
    assert_eq!(pair(light), ("claude-opus-5-5", Effort::Medium));
}

#[test]
fn one_with_a_body_uses_its_own() {
    let agents = load(&[
        ("base.md", PARENT),
        ("light.md", "---\nextends: base\n---\nMy own.\n"),
    ]);
    let light = agents.get(&name("light")).unwrap();
    assert_eq!(light.prompt.as_deref(), Some("My own."));
    assert_eq!(pair(light), ("claude-opus-5-5", Effort::High));
}

#[test]
fn keys_the_file_sets_replace_the_parents_and_the_rest_come_from_it() {
    let agents = load(&[
        (
            "base.md",
            "---\n# A comment.\nrole: implementer\nharness: pi\nmodel: m1\neffort: high\n\
             url: http://box:11434/v1\ncontext: 65536\n---\n",
        ),
        (
            "light.md",
            "---\nextends: base # the parent\n# Lighter.\neffort: low\nmodel: \"m2\"\n---\n",
        ),
    ]);
    let base = agents.get(&name("base")).unwrap();
    let light = agents.get(&name("light")).unwrap();
    assert_eq!(pair(light), ("m2", Effort::Low));
    assert_eq!(model(light).harness, model(base).harness);
    assert_eq!(limit(light), limit(base), "`url` comes from the parent");
}

#[test]
fn role_and_harness_can_be_overridden() {
    let agents = load(&[
        ("base.md", PARENT),
        (
            "other.md",
            "---\nextends: base\nrole: reviewer\nharness: codex\n---\n",
        ),
    ]);
    let other = agents.get(&name("other")).unwrap();
    assert_eq!(other.role, Role::Reviewer);
    assert_eq!(model(other).harness, AgentHarness::Codex);
    assert_eq!(other.prompt.as_deref(), Some("Parent body."));
}

#[test]
fn a_merged_file_that_is_invalid_is_refused_naming_the_child() {
    let endpoint = "---\nrole: reviewer\nharness: endpoint\nurl: http://box/v1\nmodel: m\n\
                    context: 32768\n---\n";
    let err = refused(&[
        ("box.md", endpoint),
        ("look.md", "---\nextends: box\nsecond_look: true\n---\n"),
    ]);
    assert!(err.contains("look.md"), "{err}");
    assert!(err.contains("second_look"), "{err}");

    let err = refused(&[(
        "cubic.md",
        "---\nextends: kelpie:coderabbit\nbot: cubic\n---\nA prompt.\n",
    )]);
    assert!(err.contains("cubic.md"), "{err}");
    assert!(err.contains("body"), "{err}");
}

#[test]
fn a_missing_parent_is_refused_naming_the_file() {
    let err = refused(&[("light.md", "---\nextends: nobody\neffort: low\n---\n")]);
    assert!(err.contains("light.md"), "{err}");
    assert!(err.contains("nobody"), "{err}");
}

#[test]
fn a_bare_name_with_no_file_is_refused_as_naming_no_file_though_kelpie_ships_it() {
    let err = refused(&[("light.md", "---\nextends: triager\n---\n")]);
    assert!(err.contains("light.md"), "{err}");
    assert!(err.contains("`extends: triager` names no file"), "{err}");
}

#[test]
fn an_unknown_shipped_name_is_refused_naming_the_shipped_agents() {
    let err = refused(&[("light.md", "---\nextends: kelpie:nope\n---\n")]);
    assert!(err.contains("light.md"), "{err}");
    assert!(
        err.contains("`extends: kelpie:nope` names no shipped agent"),
        "{err}"
    );
    assert!(
        err.contains("coderabbit, cubic, codex, defect-hunter"),
        "{err}"
    );
    assert!(err.ends_with("representative, triager"), "{err}");
}

#[test]
fn a_stub_that_sets_a_key_keeps_the_shipped_body_and_takes_its_own_value() {
    let agents = load(&[(
        "triager.md",
        "---\nextends: kelpie:triager\nharness: claude-code\nmodel: mine\neffort: low\n---\n",
    )]);
    let shipped = Agents::embedded().bind("triager", OPUS_HIGH);
    let shipped = shipped.get(&name("triager")).unwrap();
    let stub = agents.get(&name("triager")).unwrap();
    assert_eq!(pair(stub).0, "mine");
    assert_ne!(pair(shipped).0, "mine");
    assert_eq!(stub.prompt, shipped.prompt);
    assert_eq!(stub.role, Role::Triager);
}

#[test]
fn a_cycle_is_refused_naming_the_file() {
    let err = refused(&[
        ("a.md", "---\nextends: b\n---\n"),
        ("b.md", "---\nextends: a\n---\n"),
    ]);
    assert!(err.contains("a.md"), "{err}");
    assert!(err.contains("circle"), "{err}");

    let err = refused(&[("me.md", "---\nextends: me\n---\n")]);
    assert!(err.contains("me.md"), "{err}");
    assert!(err.contains("circle"), "{err}");
}

#[test]
fn a_parent_that_extends_another_resolves() {
    let agents = load(&[
        ("base.md", PARENT),
        ("mid.md", "---\nextends: base\neffort: medium\n---\n"),
        ("leaf.md", "---\nextends: mid\nmodel: m3\n---\n"),
    ]);
    let leaf = agents.get(&name("leaf")).unwrap();
    assert_eq!(pair(leaf), ("m3", Effort::Medium));
    assert_eq!(leaf.prompt.as_deref(), Some("Parent body."));
}

#[test]
fn a_child_that_sorts_before_its_parent_resolves() {
    let agents = load(&[
        ("a-child.md", "---\nextends: z-parent\neffort: low\n---\n"),
        ("z-parent.md", PARENT),
    ]);
    let child = agents.get(&name("a-child")).unwrap();
    assert_eq!(pair(child), ("claude-opus-5-5", Effort::Low));
    assert_eq!(child.prompt.as_deref(), Some("Parent body."));
}

#[test]
fn a_shipped_agent_can_be_extended_by_a_file_of_another_name() {
    let agents = load(&[(
        "hunter-light.md",
        "---\nextends: kelpie:defect-hunter\nharness: claude-code\nmodel: claude-opus-5-5\n\
         effort: medium\nsecond_look: false\n---\n",
    )]);
    let default =
        Agents::embedded().bind("defect-hunter", &format!("{OPUS_HIGH}second_look: true\n"));
    let hunter = default.get(&name("defect-hunter")).unwrap();
    let light = agents.get(&name("hunter-light")).unwrap();
    assert_eq!(pair(light), ("claude-opus-5-5", Effort::Medium));
    assert!(!light.second_look);
    assert!(hunter.second_look);
    assert_eq!(light.prompt, hunter.prompt);
    assert_eq!(light.role, Role::Reviewer);
}

#[test]
fn a_bare_name_extends_the_file_of_that_name_and_never_the_shipped_agent() {
    let agents = load(&[
        (
            "defect-hunter.md",
            "---\nrole: reviewer\nharness: claude-code\nmodel: mine\neffort: low\n---\nMine.\n",
        ),
        (
            "light.md",
            "---\nextends: defect-hunter\neffort: medium\n---\n",
        ),
    ]);
    let light = agents.get(&name("light")).unwrap();
    assert_eq!(pair(light), ("mine", Effort::Medium));
    assert_eq!(light.prompt.as_deref(), Some("Mine."));
}

#[test]
fn an_edit_to_the_parent_reaches_the_child_on_reload() {
    let dir = folder(&[
        ("base.md", PARENT),
        ("light.md", "---\nextends: base\neffort: medium\n---\n"),
    ]);
    let before = snapshot(dir.path());
    fs::write(
        dir.path().join("base.md"),
        PARENT
            .replace("claude-opus-5-5", "m9")
            .replace("Parent body.", "New body."),
    )
    .unwrap();
    assert_ne!(snapshot(dir.path()), before, "the edit shows on the reread");
    let agents = Agents::load(dir.path()).unwrap();
    let light = agents.get(&name("light")).unwrap();
    assert_eq!(pair(light), ("m9", Effort::Medium));
    assert_eq!(light.prompt.as_deref(), Some("New body."));
}

#[test]
fn a_refusal_of_a_key_the_file_sets_names_the_files_own_line() {
    let err = refused(&[
        ("base.md", PARENT),
        (
            "light.md",
            "---\nextends: base\neffort: bogus\n---\nA body.\n",
        ),
    ]);
    assert!(err.contains("light.md"), "{err}");
    assert!(err.contains("`effort`"), "{err}");
    assert!(err.ends_with("at line 3, column 9"), "{err}");
}

#[test]
fn a_refusal_of_a_key_it_inherits_names_the_parent_and_no_line() {
    let err = refused(&[
        ("a-light.md", "---\nextends: z-base\nmodel: m\n---\n"),
        (
            "z-base.md",
            "---\nrole: implementer\nharness: claude-code\nmodel: m\neffort: bogus\n---\n",
        ),
    ]);
    assert!(err.contains("a-light.md"), "{err}");
    assert!(err.contains("`effort`"), "{err}");
    assert!(err.ends_with("inherited through `z-base`"), "{err}");
    assert!(!err.contains("line"), "{err}");
}

#[test]
fn a_quoted_parent_name_with_a_comment_after_it_resolves() {
    let agents = load(&[
        ("base.md", PARENT),
        (
            "light.md",
            "---\nextends: \"base\" # the parent\neffort: low\n---\n",
        ),
    ]);
    assert_eq!(pair(agents.get(&name("light")).unwrap()).1, Effort::Low);
}

#[test]
fn a_quoted_parent_name_keeps_a_hash_inside_it() {
    let err = refused(&[("light.md", "---\nextends: \"a #b\"\n---\n")]);
    assert!(err.contains("`extends: a #b`"), "{err}");
}

#[test]
fn an_embedded_default_that_extends_is_refused() {
    let default = "---\nextends: other\n---\n";
    let parent = |_: &str| Ok(crate::agents::extends::ParentText::Default(default));
    let err =
        crate::agents::extends::merged("light", "---\nextends: odd\n---\n", &parent).unwrap_err();
    assert!(err.contains("not supported"), "{err}");
}

#[test]
fn a_stub_extending_a_shipped_agent_loads_its_frontmatter_and_body() {
    let stub = format!("---\nextends: kelpie:triager\n{OPUS_HIGH}---\n");
    let agents = load(&[("triager.md", &stub)]);
    assert_eq!(
        agents.get(&name("triager")),
        Agents::embedded()
            .bind("triager", OPUS_HIGH)
            .get(&name("triager"))
    );
}

#[test]
fn body_append_puts_the_files_body_after_the_parents_with_a_blank_line() {
    let agents = load(&[
        ("base.md", PARENT),
        (
            "light.md",
            "---\nextends: base\nbody: append\n---\nMy addition.\n",
        ),
    ]);
    let light = agents.get(&name("light")).unwrap();
    assert_eq!(
        light.prompt.as_deref(),
        Some("Parent body.\n\nMy addition.")
    );
}

#[test]
fn body_append_with_no_body_of_its_own_takes_the_parents() {
    let agents = load(&[
        ("base.md", PARENT),
        ("light.md", "---\nextends: base\nbody: append\n---\n"),
    ]);
    let light = agents.get(&name("light")).unwrap();
    assert_eq!(light.prompt.as_deref(), Some("Parent body."));
}

#[test]
fn body_append_applies_at_each_step_of_a_chain_down_to_a_shipped_agent() {
    let agents = load(&[
        (
            "mid.md",
            "---\nextends: kelpie:triager\nbody: append\nharness: claude-code\nmodel: m2\neffort: low\n---\nMiddle.\n",
        ),
        (
            "leaf.md",
            "---\nextends: mid\nbody: append\nharness: claude-code\nmodel: m3\neffort: low\n---\nLeaf.\n",
        ),
    ]);
    let shipped = Agents::embedded().bind("triager", OPUS_HIGH);
    let shipped = shipped
        .get(&name("triager"))
        .unwrap()
        .prompt
        .clone()
        .unwrap();
    let mid = agents.get(&name("mid")).unwrap();
    assert_eq!(mid.prompt, Some(format!("{shipped}\n\nMiddle.")));
    let leaf = agents.get(&name("leaf")).unwrap();
    assert_eq!(leaf.prompt, Some(format!("{shipped}\n\nMiddle.\n\nLeaf.")));
    assert_eq!(pair(leaf).0, "m3");
}

#[test]
fn a_body_key_with_any_value_but_append_is_refused_with_its_line() {
    for value in ["replace", "\"\"", "true"] {
        let text = format!("---\nextends: base\neffort: low\nbody: {value}\n---\nA body.\n");
        let err = refused(&[("base.md", PARENT), ("light.md", &text)]);
        assert!(err.contains("light.md"), "{err}");
        assert!(err.contains("`body`"), "{err}");
        assert!(err.contains("`append`"), "{err}");
        assert!(err.ends_with("at line 4"), "{err}");
    }
}

#[test]
fn a_body_key_on_a_file_that_extends_nothing_is_refused() {
    let err = refused(&[("light.md", &PARENT.replace("effort: high", "body: append"))]);
    assert!(err.contains("light.md"), "{err}");
    assert!(err.contains("body"), "{err}");
}

#[test]
fn the_line_of_a_later_refusal_holds_with_body_in_the_frontmatter() {
    let err = refused(&[
        ("base.md", PARENT),
        (
            "light.md",
            "---\nextends: base\nbody: append\neffort: bogus\n---\nA body.\n",
        ),
    ]);
    assert!(err.ends_with("at line 4, column 9"), "{err}");
}

#[test]
fn a_body_key_set_twice_is_refused_with_the_line_of_the_second() {
    for second in ["append", "replace"] {
        let text = format!("---\nextends: base\nbody: append\nbody: {second}\n---\nA body.\n");
        let err = refused(&[("base.md", PARENT), ("light.md", &text)]);
        assert!(err.contains("light.md"), "{err}");
        assert!(err.contains("`body`"), "{err}");
        assert!(err.ends_with("at line 4"), "{err}");
    }
}

#[test]
fn a_refusal_from_inside_a_parent_file_names_that_file_and_not_the_child() {
    let parent = "---\nextends: kelpie:triager\nbody: bogus\n---\n";
    let err = refused(&[("a.md", "---\nextends: z\n---\n"), ("z.md", parent)]);
    assert!(err.contains("a.md: in `z.md`: "), "{err}");
    assert!(err.contains("`body: bogus`"), "{err}");
    let twice = "---\nextends: kelpie:triager\nbody: append\nbody: append\n---\n";
    let err = refused(&[("a.md", "---\nextends: z\n---\n"), ("z.md", twice)]);
    assert!(err.contains("z.md"), "{err}");
    assert!(err.contains("set twice"), "{err}");
}

#[test]
fn a_refusal_deep_in_a_chain_names_the_file_it_came_from_once() {
    let err = refused(&[
        ("a.md", "---\nextends: y\n---\n"),
        ("y.md", "---\nextends: z\n---\n"),
        ("z.md", "---\nextends: kelpie:triager\nbody: bogus\n---\n"),
    ]);
    assert!(err.contains("z.md"), "{err}");
    assert!(!err.contains("y.md"), "{err}");
}
