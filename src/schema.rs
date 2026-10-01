//! Kelpie's answer to shep's `--schema` probe
//!
//! Lookout builds its settings panes from this answer: one for kelpie's
//! `[kelpie]` section of `dogs.toml`, and one for each runner sheep's
//! `[app.dogs.kelpie]` table. The webhook's URL is marked a secret, so
//! lookout draws it `<set>`.

use shep_client::dogs::Probe;

use crate::settings::Settings;
use crate::webhook::KelpieSettings;

/// Answers `--version` or `--schema` and exits, or returns when neither was asked
///
/// The first line of `main`, before anything opens a socket. The version
/// answer asks for the shepherd channel, which the lease dog serves.
pub fn probe() {
    Probe::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
        .ask_for_channel()
        .answer_with_sheep::<KelpieSettings, Settings>();
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
    // A tagged enum's variants each add their own keys.
    fn schema_keys(root: &Value, schema: &Value, prefix: &str, keys: &mut BTreeSet<String>) {
        let schema = resolve(root, schema);
        if let Some(variants) = schema["oneOf"].as_array() {
            for variant in variants {
                schema_keys(root, variant, prefix, keys);
            }
            return;
        }
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

    // The schema a table is read by: for a tagged enum, the variant its `kind` names.
    fn variant<'a>(root: &'a Value, schema: &'a Value, table: &toml::Table) -> &'a Value {
        let schema = resolve(root, schema);
        let kind = table.get("kind").and_then(toml::Value::as_str);
        schema["oneOf"]
            .as_array()
            .and_then(|variants| {
                variants
                    .iter()
                    .find(|v| kind.is_some_and(|k| v["properties"]["kind"]["const"] == k))
            })
            .unwrap_or(schema)
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
        let properties = &variant(root, schema, table)["properties"];
        for (key, value) in table {
            match value {
                toml::Value::Table(inner)
                    if variant(root, &properties[key], inner)["properties"].is_object() =>
                {
                    table_keys(
                        root,
                        &properties[key],
                        inner,
                        &format!("{prefix}{key}."),
                        keys,
                    );
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

    // The example with every optional key set, once for each kind of local
    // round, so each key is counted.
    fn examples_with_every_key() -> Vec<String> {
        let text = include_str!("../settings.example.toml")
            .replace("# ruling_channels =", "ruling_channels =")
            .replace("# pull_request_reviewers =", "pull_request_reviewers =")
            .replace("build_env = {}", "build_env = { BUN = \"bun\" }")
            .replace(
                "# instructions_file = \"~/.kelpie/projects/shep/worker-instructions.md\"",
                "instructions_file = \"worker.md\"",
            )
            .replace(
                "# [[app.dogs.kelpie.worker.guard_hooks]]\n# event = \"PreToolUse\"\n# matcher = \"Bash\"\n# command =",
                "[[app.dogs.kelpie.worker.guard_hooks]]\nevent = \"PreToolUse\"\nmatcher = \"Bash\"\ncommand =",
            )
            .replace("# [app.dogs.kelpie.preview]", "[app.dogs.kelpie.preview]")
            .replace("# enabled = true", "enabled = true")
            .replace("# configuration =", "configuration =")
            .replace("# routes =", "routes =")
            .replace("# domains =", "domains =")
            .replace("# private_names =", "private_names =")
            .replace("# local_rounds =", "local_rounds =")
            .replace("# rounds =", "rounds =")
            .replace("# reviewers = [\"qwen\", \"claude\", \"opus\"]", "reviewers = [\"qwen\"]")
            .replace("# [app.dogs.kelpie.agents]", "[app.dogs.kelpie.agents]")
            .replace("# worker = \"opus-high\"", "worker = \"opus-high\"")
            .replace("# reviewer = \"opus-high\"", "reviewer = \"opus-high\"")
            .replace("# judge = \"opus-high\"", "judge = \"opus-high\"")
            .replace("# planner = \"opus-high\"", "planner = \"opus-high\"");
        assert!(
            text.contains("reviewers = [\"qwen\"]"),
            "the example's list moved"
        );
        let command = "kind = \"command\"\ncommand = \"~/.claude/scripts/qwen-review.sh\"\n\
                       gpu_lease = true\nollama = \"http://localhost:11434\"\n\
                       ollama_model = \"m\"\npaths = [\"src/**\"]\n";
        let endpoint = "kind = \"endpoint\"\nurl = \"http://localhost:11434/v1\"\n\
                        model = \"m\"\ncontext = 32768\nlease = \"gpu\"\n";
        // Each kind of skill once, every step naming it.
        let kinds = [
            "{ kind = \"path\", path = \"skills/mine\" }",
            "{ kind = \"plugin\", plugin = \"plugins/house\", skill = \"mine\" }",
            "{ kind = \"none\" }",
        ];
        [command, endpoint, "kind = \"off\"\n"]
            .into_iter()
            .zip(kinds)
            .map(|(local, kind)| {
                let table = format!("[app.dogs.kelpie.review.local]\n{local}");
                let mut text = crate::test::with_tables(&text, &table);
                text.push_str("\n[app.dogs.kelpie.skills]\n");
                for step in crate::skills::Step::ALL {
                    text.push_str(&format!("{step} = {kind}\n"));
                }
                toml::to_string(&crate::test::project_table(&text)).unwrap()
            })
            .collect()
    }

    #[test]
    fn the_project_schema_round_trips_every_key() {
        let root = schema();
        let sheep_schema = &root[SHEEP_SCHEMA_KEY];
        let mut sheep = BTreeSet::new();
        schema_keys(&root, sheep_schema, "", &mut sheep);
        let set: BTreeSet<String> = examples_with_every_key()
            .iter()
            .flat_map(|text| keys_of_table(&root, sheep_schema, text))
            .collect();
        assert_eq!(sheep, set);
        for key in [
            "models.judge.effort",
            "worker.build_env",
            "preview.enabled",
            "preview.routes",
            "review.local.context",
            "review.local.command",
        ] {
            assert!(sheep.contains(key), "{key}: {sheep:?}");
        }
    }

    #[test]
    fn the_kelpie_schema_round_trips_every_key() {
        let mut root = schema();
        root.as_object_mut().unwrap().remove(SHEEP_SCHEMA_KEY);
        let example = include_str!("../kelpie-settings.example.toml")
            .replace("# ruling_channels =", "ruling_channels =")
            .replace("# [kelpie.reviewers.", "[kelpie.reviewers.")
            .replace("# reviews =", "reviews =")
            .replace("# hours =", "hours =")
            .replace(
                "# [kelpie.leases]\n# cargo-test",
                "[kelpie.leases]\ncargo-test",
            )
            .replace(
                "# [kelpie.local_reviewers.qwen]\n# kind = \"command\"\n# command =",
                "[kelpie.local_reviewers.qwen]\nkind = \"command\"\ncommand =",
            )
            .replace(
                "# [kelpie.agents.opus-high]\n# harness = \"claude-code\"\n\
                 # model = \"claude-opus-5-5\"\n# effort = \"high\"",
                "[kelpie.agents.opus-high]\nharness = \"claude-code\"\n\
                 model = \"claude-opus-5-5\"\neffort = \"high\"",
            );
        assert!(
            example.contains("\n[kelpie.local_reviewers.qwen]"),
            "the example moved"
        );
        let example: toml::Table = toml::from_str(&example).unwrap();
        let section = toml::to_string(&example["kelpie"]).unwrap();
        assert_eq!(keys_of_schema(&root), keys_of_table(&root, &root, &section));
    }

    #[test]
    fn a_checked_value_carries_its_bounds() {
        let defs = &schema()["$defs"];
        assert_eq!(defs["KickoffHours"]["minimum"], 1);
        assert_eq!(defs["KickoffHours"]["maximum"], 24);
        assert_eq!(defs["Channels"]["minItems"], 1);
        assert_eq!(defs["Route"]["pattern"], "^/");
        assert_eq!(defs["ContextSize"]["minimum"], 4096);
        let max_items = &defs["Settings"]["properties"]["max_items"];
        assert_eq!(
            (&max_items["minimum"], &max_items["default"]),
            (&1.into(), &1.into())
        );
        assert!(defs["EndpointUrl"]["pattern"].is_string());
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
