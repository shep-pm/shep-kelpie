use super::super::*;

// The board's finding reaches the state file as whole percents, so a saved
// ruling loads back the same whatever the harness reads it with.
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

// A ruling an older build raised on median hours: the variant stays so its file
// loads, and it writes back exactly as saved.
#[test]
fn an_agent_fit_ruling_saved_with_hours_still_loads() {
    let saved = serde_json::json!({
        "kind": "agent-fit",
        "work_kind": "fix",
        "worse": "slow",
        "better": "quick",
        "measure": { "hours": { "worse": 180, "better": 60 } },
        "suggested_kinds": ["docs"],
    });

    let kind: RulingKind = serde_json::from_value(saved.clone()).unwrap();

    assert_eq!(
        kind,
        RulingKind::AgentFit(AgentFit {
            work_kind: "fix".into(),
            worse: "slow".into(),
            better: "quick".into(),
            measure: AgentMeasure::Hours {
                worse: 180,
                better: 60,
            },
            suggested_kinds: vec!["docs".into()],
        }),
        "the saved line reads as the hours measure it was written with"
    );
    assert_eq!(serde_json::to_value(&kind).unwrap(), saved);
}
