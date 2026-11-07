//! `[app.dogs.kelpie.repo_setup]`, whose `extra` adds a plugin the repo does not name and whose
//! other keys still load, read by nothing

use super::{EXAMPLE, parse, parse_err};

fn with(keys: &str) -> String {
    format!("{EXAMPLE}\n[app.dogs.kelpie.repo_setup]\n{keys}\n")
}

#[test]
fn left_out_the_table_narrows_nothing_and_adds_nothing() {
    let setup = parse(EXAMPLE).unwrap().repo_setup;
    assert!(setup.plugins && setup.own);
    assert!(setup.skip.is_empty());
    assert!(setup.extra.is_empty());
}

#[test]
fn an_unknown_key_is_refused_by_name() {
    let err = parse_err(&with("plugin = false"));
    assert!(
        err.contains("`repo_setup.plugin = false`: unknown field `plugin`"),
        "{err}"
    );
}

#[test]
fn the_keys_that_used_to_narrow_still_load() {
    let keys = "plugins = false\nown = false\nroles = [\"worker\", \"pm\"]\n\
                parts = [\"skills\", \"agents\"]\nskip = [\"style@house\"]";
    let setup = parse(&with(keys)).unwrap().repo_setup;
    assert!(!setup.plugins && !setup.own);
    assert_eq!(
        (setup.roles.len(), setup.parts.len(), setup.skip.len()),
        (2, 2, 1)
    );
}

#[test]
fn a_role_kelpie_has_no_call_for_is_refused() {
    let err = parse_err(&with("roles = [\"implementer\"]"));
    assert!(err.contains("repo_setup.roles"), "{err}");
    assert!(err.contains("worker, reviewer"), "{err}");
}

#[test]
fn a_role_that_went_with_the_panel_is_refused_naming_it() {
    for gone in ["advisor", "researcher"] {
        let err = parse_err(&with(&format!("roles = [\"worker\", \"{gone}\"]")));
        assert!(err.contains("repo_setup.roles"), "{gone}: {err}");
        assert!(err.contains("`advisor` and `researcher`"), "{gone}: {err}");
        assert!(err.contains("ruling panel"), "{gone}: {err}");
    }
}

#[test]
fn the_issue_writers_role_is_refused_naming_it_and_the_migration() {
    let err = parse_err(&with("roles = [\"worker\", \"issue-writer\"]"));
    assert!(err.contains("repo_setup.roles"), "{err}");
    assert!(
        err.contains("`issue-writer` went with the issue writer"),
        "{err}"
    );
    assert!(err.contains("shep kelpie settings migrate"), "{err}");
}

#[test]
fn a_plugin_is_written_name_at_marketplace() {
    for bad in ["style", "style@", "@house", "a@b@c", "../x@house"] {
        let err = parse_err(&with(&format!("skip = [{bad:?}]")));
        assert!(err.contains("name@marketplace"), "{bad}: {err}");
    }
    assert!(parse(&with("skip = [\"style@house\"]")).is_ok());
}

#[test]
fn extra_gives_each_plugin_its_marketplace() {
    let keys = "extra = { \"quiet@lab\" = \"github:acme/lab#v2\" }";
    let setup = parse(&with(keys)).unwrap().repo_setup;
    let entries: Vec<_> = setup
        .extra
        .iter()
        .map(|(id, source)| (id.as_str().to_owned(), source.entry()))
        .collect();
    assert_eq!(
        entries,
        [(
            "quiet@lab".to_owned(),
            serde_json::json!({
                "source": { "source": "github", "repo": "acme/lab", "ref": "v2" }
            })
        )]
    );
}

#[test]
fn a_marketplace_is_written_kind_colon_address() {
    for bad in ["acme/lab", "svn:acme/lab", "github:", "github:#v2"] {
        let err = parse_err(&with(&format!("extra = {{ \"quiet@lab\" = {bad:?} }}")));
        assert!(err.contains("github:owner/name"), "{bad}: {err}");
    }
    let keys = "extra = { \"a@x\" = \"git:https://h/r.git\", \"b@y\" = \"directory:plugins/y\" }";
    let setup = parse(&with(keys)).unwrap().repo_setup;
    let entries: Vec<_> = setup.extra.values().map(|s| s.entry()).collect();
    assert_eq!(
        entries,
        [
            serde_json::json!({ "source": { "source": "git", "url": "https://h/r.git" } }),
            serde_json::json!({ "source": { "source": "directory", "path": "plugins/y" } }),
        ]
    );
}

#[test]
fn a_part_a_plugin_lacks_is_refused() {
    let err = parse_err(&with("parts = [\"themes\"]"));
    assert!(err.contains("repo_setup.parts"), "{err}");
    assert!(
        err.contains("skills, agents, commands, hooks or mcp"),
        "{err}"
    );
}
