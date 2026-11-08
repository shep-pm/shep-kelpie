//! `[app.dogs.kelpie.repo_setup]`, whose one key `extra` adds a plugin the repo
//! does not name

use super::{EXAMPLE, parse, parse_err};

fn with(keys: &str) -> String {
    format!("{EXAMPLE}\n[app.dogs.kelpie.repo_setup]\n{keys}\n")
}

#[test]
fn left_out_the_table_adds_nothing() {
    assert!(parse(EXAMPLE).unwrap().repo_setup.extra.is_empty());
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
fn the_narrowing_keys_are_refused_by_name_and_say_migrate_deletes_them() {
    let gone = [
        "plugins = false",
        "own = false",
        "roles = [\"worker\", \"issue-writer\"]",
        "parts = [\"skills\", \"themes\"]",
        "skip = [\"style@house\"]",
    ];
    for key in gone {
        let err = parse_err(&with(key));
        let name = key.split([' ', '=']).next().unwrap();
        assert!(
            err.contains(&format!("`repo_setup.{name}`")),
            "{key}: {err}"
        );
        assert!(
            err.contains("the repo's `.claude` files decide what loads"),
            "{key}: {err}"
        );
        assert!(err.contains("shep kelpie settings migrate"), "{key}: {err}");
    }
    let whole = parse_err(&with(&gone.join("\n")));
    let named: Vec<&str> = whole
        .lines()
        .skip(1)
        .map(|line| line.split('`').nth(1).unwrap())
        .collect();
    assert_eq!(
        named,
        [
            "repo_setup.plugins",
            "repo_setup.own",
            "repo_setup.roles",
            "repo_setup.parts",
            "repo_setup.skip",
        ],
        "a table that narrowed names every key it must drop at once"
    );
    assert!(parse(EXAMPLE).is_ok(), "the example sets none of them");
}

#[test]
fn a_plugin_is_written_name_at_marketplace() {
    for bad in ["style", "style@", "@house", "a@b@c", "../x@house"] {
        let err = parse_err(&with(&format!(
            "extra = {{ {bad:?} = \"github:acme/lab\" }}"
        )));
        assert!(err.contains("name@marketplace"), "{bad}: {err}");
    }
    assert!(parse(&with("extra = { \"style@house\" = \"github:acme/lab\" }")).is_ok());
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
