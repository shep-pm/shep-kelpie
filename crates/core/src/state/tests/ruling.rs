use super::super::*;

// The board's finding reaches the state file as whole percents and whole
// minutes, so a saved ruling loads back the same whatever the harness reads it with.
#[test]
fn an_agent_fit_ruling_saves_both_records_and_loads_back() {
    let kind = RulingKind::AgentFit(AgentFit {
        work_kind: "fix".into(),
        worse: "weak".into(),
        better: "strong".into(),
        measure: AgentMeasure::MergeRate {
            worse: 50,
            better: 100,
        },
        suggested_kinds: vec!["docs".into()],
    });
    let value = serde_json::to_value(&kind).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "kind": "agent-fit",
            "work_kind": "fix",
            "worse": "weak",
            "better": "strong",
            "measure": { "merge-rate": { "worse": 50, "better": 100 } },
            "suggested_kinds": ["docs"],
        })
    );
    assert_eq!(serde_json::from_value::<RulingKind>(value).unwrap(), kind);
}
