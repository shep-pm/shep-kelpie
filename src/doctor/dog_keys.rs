//! Whether the dog will see each paddock gateway's key
//!
//! The dog reads a paddock model's load with its gateway's key, from the
//! environment the shepherd gives it: the `env` of its entry in
//! `shep.toml`'s `[daemon.adopted_dogs]`. Doctor's shell is not the dog's,
//! so it reads that entry instead. It names keys, never their values.

use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::path::Path;

use shep_client::shep_core::config::DaemonConfig;
use shep_client::shep_core::paths::ShepPaths;

use super::Line;
use crate::agents::Agents;
use crate::settings::{Gateway, GatewayKind, GatewayName, Gateways};
use crate::shepherd::DOG;

/// A line for each paddock gateway an agent names, on whether the dog's entry sets its key
pub(super) fn checks(gateways: &Gateways, agents: &Agents, shep_home: &Path) -> Vec<Line> {
    let named: BTreeSet<&GatewayName> = (agents.iter())
        .filter_map(|(_, agent)| Some(agent.runs.gateway()?.0))
        .collect();
    let shep_toml = ShepPaths::at(shep_home.to_path_buf()).daemon_config;
    let keys = dog_keys(&shep_toml);
    let table = format!("`[daemon.adopted_dogs.{DOG}]`");
    // parity: gateway.lease-availability
    let paddocks = (gateways.iter())
        .filter(|(name, gateway)| gateway.kind == GatewayKind::Paddock && named.contains(name));
    let line = |(name, gateway): (&GatewayName, &Gateway)| {
        let subject = format!("gateway {name}: the dog's key");
        let var = gateway.key_env.as_str();
        let what = match &keys {
            Err(why) => {
                return Line::unsure(subject, why, "put shep.toml right, then run doctor again");
            }
            Ok(Some(keys)) if keys.contains(var) => {
                return Line::ok(subject, format!("{table} in shep.toml sets {var}"));
            }
            Ok(Some(_)) => format!("{table} in shep.toml sets no {var}"),
            Ok(None) => format!("shep.toml has no {table}, so the dog has no {var}"),
        };
        let next = format!(
            "the dog cannot read this gateway's load without it. Set {var} in the `env` \
             of {table} in {}, as a `{{{{secret:...}}}}` if you like, then run `shep daemon reload`",
            shep_toml.display()
        );
        Line::unsure(subject, what, next)
    };
    paddocks.map(line).collect()
}

// The keys of the dog's entry's `env`, or None when shep.toml has no entry for it.
fn dog_keys(shep_toml: &Path) -> Result<Option<BTreeSet<String>>, String> {
    let text = match std::fs::read_to_string(shep_toml) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot read {}: {e}", shep_toml.display())),
    };
    // No `SHEP_*` variable reaches an adopted dog's entry. A parse error can quote
    // a line of the file, and so a value, so it is not shown.
    let config = DaemonConfig::load(text.as_deref(), &|_| None)
        .map_err(|_| format!("{} is not a shep.toml shep reads", shep_toml.display()))?;
    let entry = config.daemon.adopted_dogs.get(DOG);
    Ok(entry.map(|dog| dog.env.keys().cloned().collect()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::doctor::Verdict;

    const VALUE: &str = "pk-never-shown";

    fn gateways() -> Gateways {
        let table: BTreeMap<GatewayName, Gateway> = toml::from_str(
            "[paddock]\nurl = \"http://gpu:8700\"\nkey_env = \"PADDOCK_KEY\"\n\
             [unnamed]\nurl = \"http://gpu:8701\"\nkey_env = \"UNNAMED_KEY\"\n\
             [lite]\nkind = \"openai\"\nurl = \"http://lite:4000\"\nkey_env = \"LITE_KEY\"\n",
        )
        .unwrap();
        Gateways::reading(table, |_| None)
    }

    // An agent on `paddock` and one on `lite`; no agent names `unnamed`.
    fn agents() -> Agents {
        let dir = tempfile::tempdir().unwrap();
        for (file, gateway) in [("local.md", "paddock"), ("lite.md", "lite")] {
            let text = format!(
                "---\nrole: implementer\nharness: pi\nmodel: coder\neffort: low\n\
                 gateway: {gateway}\ncontext: 65536\n---\n"
            );
            std::fs::write(dir.path().join(file), text).unwrap();
        }
        Agents::load(dir.path()).unwrap()
    }

    fn checked(shep_toml: Option<&str>) -> Vec<Line> {
        let home = tempfile::tempdir().unwrap();
        if let Some(text) = shep_toml {
            std::fs::write(home.path().join("shep.toml"), text).unwrap();
        }
        checks(&gateways(), &agents(), home.path())
    }

    fn only(lines: &[Line]) -> &Line {
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0].subject, "gateway paddock: the dog's key");
        assert!(!lines[0].to_string().contains(VALUE), "{}", lines[0]);
        &lines[0]
    }

    fn unsure(line: &Line) -> (&str, &str) {
        match &line.verdict {
            Verdict::Unsure { what, next } => (what, next),
            other => panic!("not unsure: {other:?}"),
        }
    }

    #[test]
    fn a_key_the_dog_s_entry_sets_passes_without_its_value() {
        let lines = checked(Some(&format!(
            "[daemon.adopted_dogs.kelpie]\npath = \"/bin/shep-kelpie\"\n\
             env = {{ PADDOCK_KEY = \"{VALUE}\" }}\n"
        )));
        let line = only(&lines);
        assert_eq!(
            line.verdict,
            Verdict::Ok("`[daemon.adopted_dogs.kelpie]` in shep.toml sets PADDOCK_KEY".to_owned())
        );
    }

    #[test]
    fn a_key_the_dog_s_entry_lacks_is_unsure_naming_the_key_the_table_and_the_reload() {
        let lines = checked(Some(&format!(
            "[daemon.adopted_dogs.kelpie]\npath = \"/bin/shep-kelpie\"\n\
             env = {{ OTHER = \"{VALUE}\" }}\n"
        )));
        let (what, next) = unsure(only(&lines));
        assert_eq!(
            what,
            "`[daemon.adopted_dogs.kelpie]` in shep.toml sets no PADDOCK_KEY"
        );
        assert!(
            next.contains("Set PADDOCK_KEY in the `env` of `[daemon.adopted_dogs.kelpie]`"),
            "{next}"
        );
        assert!(next.contains("shep.toml"), "{next}");
        assert!(next.ends_with("then run `shep daemon reload`"), "{next}");
    }

    #[test]
    fn an_entry_written_as_a_bare_path_sets_no_key() {
        let lines = checked(Some(
            "[daemon.adopted_dogs]\nkelpie = \"/bin/shep-kelpie\"\n",
        ));
        let (what, _) = unsure(only(&lines));
        assert!(what.ends_with("sets no PADDOCK_KEY"), "{what}");
    }

    #[test]
    fn no_entry_for_the_dog_or_no_shep_toml_is_unsure() {
        for shep_toml in [None, Some("[daemon]\nlog_json = true\n")] {
            let lines = checked(shep_toml);
            let (what, _) = unsure(only(&lines));
            assert_eq!(
                what,
                "shep.toml has no `[daemon.adopted_dogs.kelpie]`, so the dog has no PADDOCK_KEY"
            );
        }
    }

    #[test]
    fn a_shep_toml_shep_cannot_read_is_unsure_and_quotes_none_of_it() {
        let lines = checked(Some(&format!(
            "[daemon.adopted_dogs.kelpie]\npath = \"/bin/shep-kelpie\"\n\
             env = {{ PADDOCK_KEY = \"{VALUE}\" \n"
        )));
        let (what, _) = unsure(only(&lines));
        assert!(what.ends_with("is not a shep.toml shep reads"), "{what}");
    }
}
