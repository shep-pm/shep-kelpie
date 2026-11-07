//! `[app.dogs.kelpie.code_graph]`: keys that load and nothing reads, since the installed server decides

use schemars::JsonSchema;
use serde::Deserialize;

/// Keys of a table nothing reads: the installed server gives reviewers a code graph
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(title = "Code graph")]
pub struct CodeGraph {
    /// Read by nothing. The graph follows `shep kelpie tools install`.
    #[serde(default)]
    pub enabled: bool,

    /// Read by nothing. A worker never gets the graph.
    #[serde(default)]
    pub worker: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_graph_is_off_unless_the_project_turns_it_on() {
        assert!(!CodeGraph::default().enabled);
        assert!(!CodeGraph::default().worker);
        let on: CodeGraph = serde_json::from_value(serde_json::json!({ "enabled": true })).unwrap();
        assert!(on.enabled);
        assert!(!on.worker, "a worker is named by a key of its own");
        let none: CodeGraph = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(!none.enabled);
        assert!(!none.worker);
    }

    #[test]
    fn a_project_can_name_the_worker_too() {
        let named: CodeGraph =
            serde_json::from_value(serde_json::json!({ "enabled": true, "worker": true })).unwrap();
        assert!(named.enabled);
        assert!(named.worker);
        let off: CodeGraph =
            serde_json::from_value(serde_json::json!({ "enabled": true, "worker": false }))
                .unwrap();
        assert!(off.enabled);
        assert!(!off.worker);
    }

    #[test]
    fn a_key_the_table_does_not_have_is_refused() {
        let wrong = serde_json::json!({ "enable": true });
        assert!(serde_json::from_value::<CodeGraph>(wrong).is_err());
        let wrong = serde_json::json!({ "enabled": true, "woker": true });
        assert!(serde_json::from_value::<CodeGraph>(wrong).is_err());
    }
}
