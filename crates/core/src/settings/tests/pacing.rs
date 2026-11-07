use super::*;

// The example with a `[pacing]` table an older build wrote, its keys being
// `body`, put back before the `[worker]` table it followed
fn with_pacing(body: &str) -> String {
    const WORKER: &str = "[app.dogs.kelpie.worker]\n";
    assert!(EXAMPLE.contains(WORKER), "the example's worker table moved");
    EXAMPLE.replace(
        WORKER,
        &format!("[app.dogs.kelpie.pacing]\n{body}\n{WORKER}"),
    )
}

#[test]
fn a_table_holding_pacing_loads_and_is_read_by_nothing() {
    for body in [
        "enabled = false\n",
        "enabled = true\n",
        "kickoff_hours = 8\n",
    ] {
        let settings = parse(&with_pacing(body)).unwrap_or_else(|e| panic!("{body}: {e}"));
        assert!(settings.pacing.is_some(), "{body}");
    }
}

#[test]
fn a_table_with_no_pacing_loads() {
    assert!(
        !EXAMPLE.contains("[app.dogs.kelpie.pacing]"),
        "the example writes no pacing table"
    );
    assert_eq!(parse(EXAMPLE).unwrap().pacing, None);
}

#[test]
fn a_pacing_table_with_a_value_it_once_refused_loads() {
    parse(&with_pacing("kickoff_hours = 99\n")).unwrap();
}

#[test]
fn a_table_holding_pacing_names_it_among_the_gone_keys() {
    let settings = parse(&with_pacing("kickoff_hours = 8\n")).unwrap();
    assert_eq!(settings.gone_keys(), ["pacing"]);
    let old = parse(&with_pacing("kickoff_hours = 8\n").replace(
        "[app.dogs.kelpie.agents]\n",
        "[app.dogs.kelpie.agents]\nimplementers = [\"opus-high\"]\n",
    ))
    .unwrap();
    assert_eq!(old.gone_keys(), ["agents.implementers", "pacing"]);
}
