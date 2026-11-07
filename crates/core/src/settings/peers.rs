//! What of a project's settings another project's project manager may read
//!
//! A project's table holds no secret, so this is the belt to that: a key
//! named for a secret is dropped, and an `env` table keeps its names alone.

use serde_json::{Map, Value};

// What an `env` table's value is shown as, so a name stays and its value is gone.
const HIDDEN: &str = "(hidden)";

// Words that make a key a secret's, whatever its case.
const SECRET_WORDS: [&str; 4] = ["token", "secret", "password", "credential"];

/// `table` with nothing in it that could be a secret
///
/// A key whose name holds `token`, `secret`, `password` or `credential` is
/// dropped at any depth. An `env` table, or a `*_env` one, keeps its names and
/// loses its values. A `*_env` or `*_var` key holding a string names a variable
/// and stays, whatever its words.
pub fn shown_table(table: &Map<String, Value>) -> Map<String, Value> {
    table
        .iter()
        .filter_map(|(key, value)| Some((key.clone(), shown(key, value)?)))
        .collect()
}

fn shown(key: &str, value: &Value) -> Option<Value> {
    let key = key.to_ascii_lowercase();
    let is_env = key == "env" || key.ends_with("_env");
    match value {
        Value::Object(names) if is_env => Some(Value::Object(
            names
                .keys()
                .map(|name| (name.clone(), Value::String(HIDDEN.to_owned())))
                .collect(),
        )),
        Value::String(_) if is_env || key.ends_with("_var") => Some(value.clone()),
        _ if SECRET_WORDS.iter().any(|word| key.contains(word)) => None,
        Value::Object(inner) => Some(Value::Object(shown_table(inner))),
        Value::Array(items) => Some(Value::Array(items.iter().map(shown_item).collect())),
        _ => Some(value.clone()),
    }
}

// An array's own items, which have no key: a table in one, at any depth, is shown as any other.
fn shown_item(item: &Value) -> Value {
    match item {
        Value::Object(inner) => Value::Object(shown_table(inner)),
        Value::Array(items) => Value::Array(items.iter().map(shown_item).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test::project_table;

    const EXAMPLE: &str = include_str!("../../../../settings.example.toml");

    fn table(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            other => panic!("not a table: {other}"),
        }
    }

    // Every key of `value` as a dotted path, arrays of tables flattened.
    fn paths(value: &Map<String, Value>, prefix: &str, into: &mut Vec<String>) {
        for (key, value) in value {
            let path = format!("{prefix}{key}");
            into.push(path.clone());
            match value {
                Value::Object(inner) => paths(inner, &format!("{path}."), into),
                Value::Array(items) => {
                    for inner in items.iter().filter_map(Value::as_object) {
                        paths(inner, &format!("{path}[]."), into);
                    }
                }
                _ => {}
            }
        }
    }

    #[test]
    fn a_key_naming_a_secret_is_dropped_at_any_depth() {
        let shown = shown_table(&table(json!({
            "git": { "merging": "auto", "api_TOKEN": "t0" },
            "webhook": { "inner": { "client_secret": "s0", "url": "u" } },
            "list": [{ "password": "p0", "name": "n" }],
            "credentials_file": "/x",
            "ci": { "block": true },
        })));
        assert_eq!(
            Value::Object(shown),
            json!({
                "git": { "merging": "auto" },
                "webhook": { "inner": { "url": "u" } },
                "list": [{ "name": "n" }],
                "ci": { "block": true },
            })
        );
    }

    #[test]
    fn a_table_inside_an_array_of_arrays_is_shown_like_any_other() {
        let shown = shown_table(&table(json!({
            "x": [[{ "api_token": "t0", "name": "n", "env": { "K": "v" } }]],
        })));
        assert_eq!(
            Value::Object(shown),
            json!({ "x": [[{ "name": "n", "env": { "K": "(hidden)" } }]] })
        );
    }

    #[test]
    fn an_env_tables_names_stay_and_its_values_go() {
        let shown = shown_table(&table(json!({
            "env": { "SHEP_HOME": "/home/me/.shep", "GH_TOKEN": "ghp_x" },
            "worker": { "build_env": { "CARGO_HOME": "/home/me/.cache/cargo" } },
        })));
        let text = Value::Object(shown.clone()).to_string();
        for gone in ["/home/me", "ghp_x"] {
            assert!(!text.contains(gone), "{text}");
        }
        let names = |value: &Value| {
            value
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&shown["env"]), ["GH_TOKEN", "SHEP_HOME"]);
        assert_eq!(names(&shown["worker"]["build_env"]), ["CARGO_HOME"]);
    }

    #[test]
    fn a_key_naming_the_variable_that_holds_a_secret_stays() {
        let shown = shown_table(&table(json!({
            "gateway": { "key_env": "PADDOCK_KEY", "token_env": "NTFY_TOKEN", "token": "t0" },
        })));
        assert_eq!(
            Value::Object(shown),
            json!({ "gateway": { "key_env": "PADDOCK_KEY", "token_env": "NTFY_TOKEN" } })
        );
    }

    #[test]
    fn the_example_settings_keep_every_key_but_those() {
        let example = project_table(EXAMPLE);
        let (mut before, mut after) = (Vec::new(), Vec::new());
        paths(&example, "", &mut before);
        paths(&shown_table(&example), "", &mut after);
        assert!(before.len() > 20, "the example's table moved: {before:?}");
        assert_eq!(after, before);
    }
}
