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
    let version = crate::home::probe_version();
    Probe::new(env!("CARGO_PKG_NAME"), &version)
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

    // Names the keys only one side has, not either whole set.
    fn assert_same_keys(schema: &BTreeSet<String>, set: &BTreeSet<String>) {
        let unset: Vec<_> = schema.difference(set).collect();
        let unknown: Vec<_> = set.difference(schema).collect();
        assert!(
            unset.is_empty() && unknown.is_empty(),
            "in the schema but never set: {unset:?}; set but not in the schema: {unknown:?}"
        );
    }

    fn keys_of_table(root: &Value, schema: &Value, text: &str) -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        table_keys(root, schema, &toml::from_str(text).unwrap(), "", &mut keys);
        keys
    }

    // The example with every optional key set, once for each kind of skill,
    // so each key is counted.
    fn examples_with_every_key() -> Vec<String> {
        let text = include_str!("../settings.example.toml")
            .replace("build_env = {}", "build_env = { BUN = \"bun\" }")
            .replace(
                "# instructions_file = \"~/.shep/kelpie/shep/worker-instructions.md\"",
                "instructions_file = \"worker.md\"",
            )
            .replace(
                "# [[app.dogs.kelpie.worker.guard_hooks]]\n# event = \"PreToolUse\"\n# matcher = \"Bash\"\n# command =",
                "[[app.dogs.kelpie.worker.guard_hooks]]\nevent = \"PreToolUse\"\nmatcher = \"Bash\"\ncommand =",
            )
            .replace(
                "# [[app.dogs.kelpie.hooks]]\n# point = \"publish\"\n# command =",
                "[[app.dogs.kelpie.hooks]]\npoint = \"publish\"\ncommand =",
            )
            .replace(
                "# test_command = \"cargo test --test {name}\"",
                "test_command = \"cargo test --test {name}\"",
            )
            .replace("# configuration = \"web\"", "configuration = \"web\"")
            .replace("# routes = [\"/\"]", "routes = [\"/\"]")
            .replace("# pm = \"pm\"", "pm = \"pm\"")
            .replace(
                "# [app.dogs.kelpie.rulings.pm_decides]\n# question =",
                "[app.dogs.kelpie.rulings.pm_decides]\nquestion =",
            )
            .replace(
                "# [app.dogs.kelpie.contributions]\n# pick = \"off\"\n# reviewers = [\"defect-hunter-lite\"]\n# security_reviewer = \"security-reviewer\"\n# quiet_days = 7\n# nudge_days = 3\n",
                "[app.dogs.kelpie.contributions]\npick = \"off\"\nreviewers = [\"defect-hunter\"]\nsecurity_reviewer = \"security-reviewer\"\nquiet_days = 7\nnudge_days = 3\n",
            )
            .replace(
                "# [app.dogs.kelpie.triage]\n# pick = \"off\"\n# hand_ready = false\n# wait_days = 7\n# max_questions = 10\n# agent = \"triager\"\n# per_day = 10\n# per_author_per_day = 2\n# min_account_days = 30\n# max_body_kb = 20\n",
                "[app.dogs.kelpie.triage]\npick = \"off\"\nhand_ready = false\nwait_days = 7\nmax_questions = 10\nagent = \"triager\"\nper_day = 10\nper_author_per_day = 2\nmin_account_days = 30\nmax_body_kb = 20\n",
            )
            .replace(
                "# authors = { \"qc-tester\"",
                "authors = { \"qc-tester\"",
            )
            .replace(
                "# [app.dogs.kelpie.triage.spam]\n# gateway = \"paddock\"\n# path = \"/classify\"\n# threshold = 0.8\n",
                "[app.dogs.kelpie.triage.spam]\ngateway = \"paddock\"\npath = \"/classify\"\nthreshold = 0.8\n",
            )
            .replace(
                "# [app.dogs.kelpie.dependencies]\n# pick = \"off\"\n# bots = [\"dependabot\"]\n# on_red = \"close\"\n# web = [\"web/\", \"package.json\"]\n# wait_hours = 24\n",
                "[app.dogs.kelpie.dependencies]\npick = \"off\"\nbots = [\"dependabot\"]\non_red = \"close\"\nweb = [\"web/\", \"package.json\"]\nwait_hours = 24\n",
            )
            .replace("# allow =", "allow =")
            .replace("# representative =", "representative =")
            .replace("# slack =", "slack =")
            .replace("# implementers =", "implementers =")
            .replace("# implementer_choice =", "implementer_choice =")
            .replace("# fallback_after =", "fallback_after =")
            .replace(
                "# [app.dogs.kelpie.reviews]\n# need = 2",
                "[app.dogs.kelpie.reviews]\nneed = 2",
            )
            .replace("# maintainer =", "maintainer =")
            .replace("# base =", "base =")
            .replace(
                "# [app.dogs.kelpie.e2e]\n# host =",
                "[app.dogs.kelpie.e2e]\nhost =",
            )
            .replace(
                "# [app.dogs.kelpie.local_rounds]\n# keep = true\n# keep_days = 30\n# folder =",
                "[app.dogs.kelpie.local_rounds]\nkeep = true\nkeep_days = 30\nfolder =",
            )
            .replace(
                "# [app.dogs.kelpie.builds]\n# max_gb = 20\n# min_free_gb = 20",
                "[app.dogs.kelpie.builds]\nmax_gb = 20\nmin_free_gb = 20",
            )
            .replace(
                "# [app.dogs.kelpie.repo_setup]\n# plugins =",
                "[app.dogs.kelpie.repo_setup]\nplugins =",
            )
            .replace("# own =", "own =")
            .replace("# roles =", "roles =")
            .replace("# parts =", "parts =")
            .replace("# skip =", "skip =")
            .replace("# extra =", "extra =")
            .replace("# command = \"scripts/", "command = \"scripts/")
            .replace("# results =", "results =");
        assert!(
            text.contains("[app.dogs.kelpie.reviews]\nneed = 2"),
            "the example's review count moved"
        );
        // Each kind of skill once, every step naming it.
        let kinds = [
            "{ kind = \"path\", path = \"skills/mine\" }",
            "{ kind = \"plugin\", plugin = \"plugins/house\", skill = \"mine\" }",
            "{ kind = \"none\" }",
            "{ kind = \"kelpie\" }",
        ];
        kinds
            .into_iter()
            .map(|kind| {
                let mut text = text.clone();
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
        assert_same_keys(&sheep, &set);
        for key in ["agents.reviewers", "worker.build_env"] {
            assert!(
                sheep.contains(key),
                "{key} is not a key of the project schema"
            );
        }
    }

    // Lookout edits a boolean, a list of strings and a table of strings, and
    // no table of tables, so the repo setup uses nothing else.
    #[test]
    fn each_repo_setup_key_is_a_boolean_a_list_of_strings_or_a_table_of_strings() {
        let root = schema();
        let project = resolve(&root, &root[SHEEP_SCHEMA_KEY]);
        let setup = resolve(&root, &project["properties"]["repo_setup"]);
        let key = |name: &str| resolve(&root, &setup["properties"][name]);
        assert_eq!(key("plugins")["type"], "boolean");
        assert_eq!(key("own")["type"], "boolean");
        for list in ["roles", "parts", "skip"] {
            let items = resolve(&root, &key(list)["items"]);
            assert_eq!(key(list)["type"], "array", "{list}");
            assert_eq!(items["type"], "string", "{list}");
        }
        let extra = key("extra");
        assert_eq!(extra["type"], "object");
        let sources = extra["patternProperties"].as_object().unwrap();
        let source = resolve(&root, sources.values().next().unwrap());
        assert_eq!(source["type"], "string");
    }

    #[test]
    fn the_kelpie_schema_round_trips_every_key() {
        let mut root = schema();
        root.as_object_mut().unwrap().remove(SHEEP_SCHEMA_KEY);
        let example = include_str!("../kelpie-settings.example.toml")
            .replace("# codex_home =", "codex_home =")
            .replace("# gpu_metrics_url =", "gpu_metrics_url =")
            .replace("# ca_cert =", "ca_cert =")
            .replace("# token =", "token =")
            .replace("# trusted =", "trusted =")
            .replace("# [kelpie.gateways.paddock]", "[kelpie.gateways.paddock]")
            .replace(
                "# url = \"http://gpu-box:8700\"",
                "url = \"http://gpu-box:8700\"",
            )
            .replace("# kind = \"paddock\"", "kind = \"paddock\"")
            .replace("# key_env =", "key_env =")
            .replace(
                "# [kelpie.leases]\n# cargo-test",
                "[kelpie.leases]\ncargo-test",
            );
        let example: toml::Table = toml::from_str(&example).unwrap();
        let section = toml::to_string(&example["kelpie"]).unwrap();
        assert_same_keys(
            &keys_of_schema(&root),
            &keys_of_table(&root, &root, &section),
        );
    }

    #[test]
    fn a_checked_value_carries_its_bounds() {
        let defs = &schema()["$defs"];
        assert_eq!(defs["KickoffHours"]["minimum"], 1);
        assert_eq!(defs["KickoffHours"]["maximum"], 24);
        let active_items = &defs["Concurrency"]["properties"]["active_items"];
        assert_eq!(
            (&active_items["minimum"], &active_items["default"]),
            (&1.into(), &1.into())
        );
        let pending = &defs["Concurrency"]["properties"]["pending_rulings"];
        assert_eq!(pending["default"], 2);
        let fix_attempts = &defs["FixAttempts"];
        assert_eq!(
            (&fix_attempts["minimum"], &fix_attempts["default"]),
            (&(-1).into(), &(-1).into())
        );
        assert_eq!(fix_attempts["maximum"], u32::MAX);
        assert!(defs["EndpointUrl"]["pattern"].is_string());
    }

    // Lookout groups a sheep's rows by table, under each table's title.
    #[test]
    fn each_project_table_has_a_title_and_says_what_it_holds() {
        let defs = &schema()["$defs"];
        for (name, title) in [("Git", "Git"), ("Ci", "CI"), ("Concurrency", "Concurrency")] {
            assert_eq!(defs[name]["title"], title, "{name}");
            assert!(defs[name]["description"].is_string(), "{name}");
        }
        let pending = &defs["Concurrency"]["properties"]["pending_rulings"]["description"];
        let pending = pending.as_str().unwrap();
        assert!(
            pending.contains("stops new work items opening and is not a cap on rulings"),
            "{pending}"
        );
        let properties = &schema()[SHEEP_SCHEMA_KEY]["properties"];
        for old in [
            "repo",
            "forge",
            "merge_authority",
            "max_items",
            "max_parked",
            "private_names",
        ] {
            assert!(properties[old].is_null(), "{old}");
        }
    }

    // A table an older build wrote loads, so the schema takes it too: each key
    // the agent board replaced is there, deprecated, with no type, so any value fits.
    #[test]
    fn the_keys_the_agent_board_replaced_are_deprecated_and_take_any_value() {
        let root = schema();
        let sheep = &root[SHEEP_SCHEMA_KEY];
        let project = resolve(&root, sheep);
        let agents = resolve(&root, &project["properties"]["agents"]);
        assert_eq!(agents["additionalProperties"], false, "{agents}");
        for old in ["implementers", "implementer_choice", "fallback_after"] {
            let key = &agents["properties"][old];
            assert_eq!(key["deprecated"], true, "{old}: {key}");
            let resolved = resolve(&root, key);
            assert!(resolved.get("type").is_none(), "{old}: {resolved}");
            let said = key["description"].as_str().unwrap_or_default();
            assert!(
                said.contains("`shep kelpie upgrade` deletes it"),
                "{old}: {said}"
            );
        }
        let old = "[app.dogs.kelpie.agents]\nimplementers = [\"sonnet-high\"]\n\
                   implementer_choice = \"order\"\nfallback_after = 15\n";
        let text =
            include_str!("../settings.example.toml").replace("[app.dogs.kelpie.agents]\n", old);
        let table = toml::to_string(&crate::test::project_table(&text)).unwrap();
        let keys = keys_of_table(&root, sheep, &table);
        let mut known = BTreeSet::new();
        schema_keys(&root, sheep, "", &mut known);
        assert!(keys.is_subset(&known), "{:?}", keys.difference(&known));
    }

    #[test]
    fn only_the_webhooks_url_ca_cert_and_token_are_secrets() {
        let root = schema();
        let secrets = root.to_string().matches(SECRET_KEY).count();
        assert_eq!(secrets, 3, "the schema marks {secrets} keys secret");
        let webhook = resolve(&root, &root["properties"]["webhook"]);
        for key in ["url", "ca_cert", "token"] {
            assert_eq!(webhook["properties"][key][SECRET_KEY], true, "{key}");
        }
    }
}
