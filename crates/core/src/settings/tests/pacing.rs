use super::*;

// The example's `[pacing]` table with its body replaced by `body`.
fn with_pacing(body: &str) -> String {
    let at = EXAMPLE
        .find("[app.dogs.kelpie.pacing]\n")
        .expect("the example holds a pacing table");
    let end = EXAMPLE[at..]
        .find("\n\n")
        .map_or(EXAMPLE.len(), |n| at + n + 1);
    format!(
        "{}[app.dogs.kelpie.pacing]\n{body}{}",
        &EXAMPLE[..at],
        &EXAMPLE[end..]
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
    let text = EXAMPLE.replace(
        "[app.dogs.kelpie.pacing]\nenabled = true\nkickoff_hours = 8\n",
        "",
    );
    assert!(
        !text.contains("[app.dogs.kelpie.pacing]"),
        "the example's pacing table moved"
    );
    assert_eq!(parse(&text).unwrap().pacing, None);
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
