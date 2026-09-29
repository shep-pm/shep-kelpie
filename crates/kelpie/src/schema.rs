//! Kelpie's answer to shep's `--schema` probe
//!
//! Lookout builds its settings panes from this answer: one for kelpie's
//! `[kelpie]` section of `dogs.toml`, and one for each runner sheep's
//! `[app.dogs.kelpie]` table. The webhook's URL is marked a secret, so
//! lookout draws it `<set>`.

use crate::settings::Settings;
use crate::webhook::KelpieSettings;

/// Answers `--version` or `--schema` and exits, or returns when neither was asked
///
/// The first line of `main`, before anything opens a socket.
pub fn probe() {
    shep_client::dogs::probe_with_sheep::<KelpieSettings, Settings>(
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
    );
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::Value;
    use shep_client::dogs::{SECRET_KEY, SHEEP_SCHEMA_KEY, config_schema_with_sheep};

    use super::*;

    fn schema() -> Value {
        config_schema_with_sheep::<KelpieSettings, Settings>().to_value()
    }

    // Follows a `$ref` into the root's `$defs`, and an `Option`'s `anyOf`.
    fn resolve<'a>(root: &'a Value, schema: &'a Value) -> &'a Value {
        if let Some(name) = schema["$ref"].as_str() {
            let name = name.trim_start_matches("#/$defs/");
            return resolve(root, &root["$defs"][name]);
        }
        if let Some(options) = schema["anyOf"].as_array()
            && let Some(some) = options.iter().find(|o| o["type"] != "null")
        {
            return resolve(root, some);
        }
        schema
    }

    // Every leaf key the schema names, dotted, the way lookout lists its rows.
    fn schema_keys(root: &Value, schema: &Value, prefix: &str, keys: &mut BTreeSet<String>) {
        let schema = resolve(root, schema);
        match schema["properties"].as_object() {
            Some(properties) => {
                for (key, property) in properties {
                    schema_keys(root, property, &format!("{prefix}{key}."), keys);
                }
            }
            None => {
                keys.insert(prefix.trim_end_matches('.').to_owned());
            }
        }
    }

    // Every leaf key a TOML table sets, dotted, walked beside its schema: a
    // map or an array is one key, as it is one row in lookout.
    fn table_keys(
        root: &Value,
        schema: &Value,
        table: &toml::Table,
        prefix: &str,
        keys: &mut BTreeSet<String>,
    ) {
        let properties = &resolve(root, schema)["properties"];
        for (key, value) in table {
            let property = resolve(root, &properties[key]);
            match value {
                toml::Value::Table(inner) if property["properties"].is_object() => {
                    table_keys(root, property, inner, &format!("{prefix}{key}."), keys);
                }
                _ => {
                    keys.insert(format!("{prefix}{key}"));
                }
            }
        }
    }

    fn keys_of_schema(schema: &Value) -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        schema_keys(schema, schema, "", &mut keys);
        keys
    }

    fn keys_of_table(root: &Value, schema: &Value, text: &str) -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        table_keys(root, schema, &toml::from_str(text).unwrap(), "", &mut keys);
        keys
    }

    // The example with every optional key set, so each one is counted.
    fn example_with_every_key() -> String {
        let text = include_str!("../settings.example.toml")
            .replace("# ruling_channels =", "ruling_channels =")
            .replace("build_env = {}", "build_env = { BUN = \"bun\" }")
            .replace(
                "# instructions_file = \"~/.kelpie/projects/shep/worker-instructions.md\"",
                "instructions_file = \"worker.md\"",
            )
            .replace("# [app.dogs.kelpie.preview]", "[app.dogs.kelpie.preview]")
            .replace("# configuration =", "configuration =")
            .replace("# routes =", "routes =")
            .replace("# domains =", "domains =");
        toml::to_string(&crate::test::project_table(&text)).unwrap()
    }

    #[test]
    fn the_project_schema_round_trips_every_key() {
        let root = schema();
        let mut sheep = BTreeSet::new();
        schema_keys(&root, &root[SHEEP_SCHEMA_KEY], "", &mut sheep);
        let set = keys_of_table(&root, &root[SHEEP_SCHEMA_KEY], &example_with_every_key());
        assert_eq!(sheep, set);
        for key in ["models.judge.effort", "worker.build_env", "preview.routes"] {
            assert!(sheep.contains(key), "{key}: {sheep:?}");
        }
    }

    #[test]
    fn the_kelpie_schema_round_trips_every_key() {
        let mut root = schema();
        root.as_object_mut().unwrap().remove(SHEEP_SCHEMA_KEY);
        let example = include_str!("../kelpie-settings.example.toml")
            .replace("# ruling_channels =", "ruling_channels =");
        let example: toml::Table = toml::from_str(&example).unwrap();
        let section = toml::to_string(&example["kelpie"]).unwrap();
        assert_eq!(keys_of_schema(&root), keys_of_table(&root, &root, &section));
    }

    #[test]
    fn only_the_webhook_url_is_a_secret() {
        let root = schema();
        let text = root.to_string();
        assert_eq!(text.matches(SECRET_KEY).count(), 1, "{text}");
        let webhook = resolve(&root, &root["properties"]["webhook"]);
        assert_eq!(webhook["properties"]["url"][SECRET_KEY], true);
    }
}
