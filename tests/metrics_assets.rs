//! The alert rules and the dashboard kelpie ships, read as the programs that
//! load them read them
//!
//! `assets/prometheus/kelpie-rules.yml` goes in Prometheus's `rule_files` and
//! `assets/grafana/kelpie.json` is imported into Grafana, so nothing compiles
//! them and nothing else says they hold an alert and a panel for the series
//! kelpie's runners send. These tests read both files, check the pair against
//! the series `docs/guide/metrics.md` lists, and check the guide names every
//! alert.

use std::path::Path;

use serde_json::Value;

const RULES: &str = "assets/prometheus/kelpie-rules.yml";
const DASHBOARD: &str = "assets/grafana/kelpie.json";
const GUIDE: &str = "docs/guide/metrics.md";

// The alert that fires when one kind of tool error takes over a role's calls
const TOOL_ERROR_ALERT: &str = "KelpieToolErrorTakesOver";

fn repo_file(path: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path))
        .unwrap_or_else(|error| panic!("{path}: {error}"))
}

fn guide() -> String {
    repo_file(GUIDE)
}

fn rules() -> Value {
    serde_saphyr::from_str::<Value>(&repo_file(RULES)).expect(RULES)
}

// Every rule of every group. A recording rule has no `alert`, and kelpie's
// file holds none.
fn alerts() -> Vec<Value> {
    rules()["groups"]
        .as_array()
        .expect("the rules file has no group array")
        .iter()
        .flat_map(|group| {
            group["rules"]
                .as_array()
                .expect("a group has no rules array")
                .iter()
                .cloned()
        })
        .collect()
}

fn dashboard() -> Value {
    serde_json::from_str::<Value>(&repo_file(DASHBOARD)).expect(DASHBOARD)
}

fn panels() -> Vec<Value> {
    dashboard()["panels"]
        .as_array()
        .expect("the dashboard has no panel array")
        .to_vec()
}

// Every query of a rule or a panel target. A panel's `expr` is one line; a
// rule's is a block, which the YAML reader hands back with its newlines.
fn queries(value: &Value) -> Vec<String> {
    let Some(list) = value["targets"].as_array() else {
        return vec![value["expr"].as_str().unwrap_or_default().to_owned()];
    };
    list.iter()
        .map(|target| target["expr"].as_str().unwrap_or_default().to_owned())
        .collect()
}

// Every `kelpie_` series name a query reads, in the order it first appears.
fn series_in(query: &str) -> Vec<String> {
    const PREFIX: &str = "kelpie_";
    let mut found: Vec<String> = Vec::new();
    let mut rest = query;
    while let Some(at) = rest.find(PREFIX) {
        // Every character taken is ASCII, so the count is the byte length too.
        let tail = &rest[at + PREFIX.len()..];
        let len = PREFIX.len()
            + tail
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .count();
        let name = &rest[at..at + len];
        if !found.iter().any(|seen: &String| seen == name) {
            found.push(name.to_owned());
        }
        rest = &rest[at + len..];
    }
    found
}

// Every series the file's queries read, with where each was read.
fn series_read(queries: &[String], where_read: &str) -> Vec<(String, String)> {
    queries
        .iter()
        .flat_map(|query| {
            series_in(query)
                .into_iter()
                .map(|name| (name, where_read.to_owned()))
        })
        .collect()
}

fn alert_queries() -> Vec<String> {
    alerts().iter().flat_map(queries).collect()
}

fn panel_queries() -> Vec<String> {
    panels().iter().flat_map(queries).collect()
}

fn assert_no_problems(problems: &[String]) {
    assert!(problems.is_empty(), "\n{}\n", problems.join("\n"));
}

// A series is listed when the guide's table names it in backticks.
fn listed_in_guide(guide: &str, name: &str) -> bool {
    guide.contains(&format!("`{name}`"))
}

#[test]
fn the_rules_alert_on_a_tool_error_kind_taking_over_a_role() {
    let alert = alerts()
        .into_iter()
        .find(|alert| alert["alert"] == TOOL_ERROR_ALERT)
        .unwrap_or_else(|| panic!("{RULES} holds no alert named {TOOL_ERROR_ALERT}"));
    let expr = alert["expr"].as_str().expect("the alert has no expr");

    assert!(
        expr.contains("kelpie_tool_errors_total"),
        "{TOOL_ERROR_ALERT} does not read kelpie_tool_errors_total: {expr}"
    );
    // `other` is a command that ran and failed, which is the worker's own
    // doing and not friction in the harness.
    assert!(
        expr.contains(r#"kind!="other""#),
        "{TOOL_ERROR_ALERT} counts `other`, which a command that merely failed is: {expr}"
    );
    // The share is of the role's calls, so the denominator is the calls series.
    assert!(
        expr.contains("kelpie_calls_total"),
        "{TOOL_ERROR_ALERT} is not a share of a role's calls: {expr}"
    );
    // A floor on calls, so one call with one failed tool call cannot fire it.
    assert!(
        expr.contains(">= 4"),
        "{TOOL_ERROR_ALERT} has no floor on calls: {expr}"
    );
    assert!(
        alert["for"] == "30m",
        "{TOOL_ERROR_ALERT} fires without waiting 30m: {}",
        alert["for"]
    );
    assert!(
        alert["labels"]["severity"] == "warning",
        "{TOOL_ERROR_ALERT} is not a warning"
    );
    let summary = alert["annotations"]["summary"].as_str().unwrap_or_default();
    assert!(
        summary.contains("$value"),
        "{TOOL_ERROR_ALERT} shows no share in its summary: {summary}"
    );
    for label in ["sheep", "role", "kind"] {
        assert!(
            summary.contains(&format!("$labels.{label}")),
            "{TOOL_ERROR_ALERT} names no {label} in its summary: {summary}"
        );
    }
}

#[test]
fn the_dashboard_has_a_panel_for_the_tool_error_series() {
    let panel = panels()
        .into_iter()
        .find(|panel| {
            queries(panel)
                .iter()
                .any(|q| q.contains("kelpie_tool_errors_total"))
        })
        .expect("no panel of the dashboard reads kelpie_tool_errors_total");
    let exprs = queries(&panel);
    assert!(
        exprs
            .iter()
            .any(|expr| expr.contains("by (sheep, role, kind)")),
        "{} groups its tool errors by none of project, role and kind: {exprs:?}",
        panel["title"]
    );
    assert!(
        exprs.iter().any(|expr| expr.contains("[$__range]")),
        "{} does not count over the dashboard's range: {exprs:?}",
        panel["title"]
    );
    assert!(
        exprs.iter().any(|expr| expr.contains(r#"sheep=~"$sheep"#)),
        "{} does not hold to the dashboard's project filter: {exprs:?}",
        panel["title"]
    );
    assert!(
        !panel["description"].as_str().unwrap_or_default().is_empty(),
        "{} has no description",
        panel["title"]
    );
}

#[test]
fn every_alert_is_named_in_the_guide() {
    let guide = guide();
    let problems: Vec<String> = alerts()
        .iter()
        .filter_map(|alert| alert["alert"].as_str())
        .filter(|name| !guide.contains(&format!("**{name}**")))
        .map(|name| format!("{RULES} alerts on {name}, which {GUIDE} never names"))
        .collect();
    assert_no_problems(&problems);
}

#[test]
fn every_series_the_rules_read_is_a_series_the_guide_lists() {
    let guide = guide();
    let problems: Vec<String> = series_read(&alert_queries(), RULES)
        .into_iter()
        .filter(|(name, _)| !listed_in_guide(&guide, name))
        .map(|(name, where_read)| format!("{where_read} reads {name}, which {GUIDE} does not list"))
        .collect();
    assert_no_problems(&problems);
}

#[test]
fn every_series_the_dashboard_reads_is_a_series_the_guide_lists() {
    let guide = guide();
    let problems: Vec<String> = series_read(&panel_queries(), DASHBOARD)
        .into_iter()
        .filter(|(name, _)| !listed_in_guide(&guide, name))
        .map(|(name, where_read)| format!("{where_read} reads {name}, which {GUIDE} does not list"))
        .collect();
    assert_no_problems(&problems);
}

#[test]
fn every_panel_type_the_dashboard_uses_is_in_its_requires() {
    let dashboard = dashboard();
    let required: Vec<&str> = dashboard["__requires"]
        .as_array()
        .expect("the dashboard has no __requires array")
        .iter()
        .filter(|need| need["type"] == "panel")
        .filter_map(|need| need["id"].as_str())
        .collect();
    let problems: Vec<String> = panels()
        .iter()
        .filter_map(|panel| panel["type"].as_str())
        .filter(|kind| !required.contains(kind))
        .map(|kind| {
            format!("{DASHBOARD} uses the panel type {kind}, which __requires does not hold")
        })
        .collect();
    assert_no_problems(&problems);
}

// A panel's rectangle, from its `gridPos`.
fn rect(panel: &Value) -> (u32, u32, u32, u32) {
    let pos = &panel["gridPos"];
    (
        pos["x"].as_u64().unwrap_or_default() as u32,
        pos["y"].as_u64().unwrap_or_default() as u32,
        pos["w"].as_u64().unwrap_or_default() as u32,
        pos["h"].as_u64().unwrap_or_default() as u32,
    )
}

fn overlap(a: (u32, u32, u32, u32), b: (u32, u32, u32, u32)) -> bool {
    a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
}

#[test]
fn no_two_panels_sit_on_each_other() {
    let panels = panels();
    let mut problems: Vec<String> = Vec::new();
    for (index, panel) in panels.iter().enumerate() {
        for other in &panels[index + 1..] {
            if overlap(rect(panel), rect(other)) {
                problems.push(format!(
                    "{} and {} hold the same grid squares",
                    panel["title"], other["title"]
                ));
            }
        }
    }
    assert_no_problems(&problems);
}

#[test]
fn every_panel_has_its_own_id() {
    let panels = panels();
    let mut problems: Vec<String> = Vec::new();
    for (index, panel) in panels.iter().enumerate() {
        for other in &panels[index + 1..] {
            if panel["id"] == other["id"] {
                problems.push(format!(
                    "{} and {} share id {}",
                    panel["title"], other["title"], panel["id"]
                ));
            }
        }
    }
    assert_no_problems(&problems);
}

// The value a field override sets for `id` on what `matcher` names, which is a
// target's refId for `byFrameRefID` and a column's name for `byName`.
fn override_property(panel: &Value, matcher_id: &str, matcher: &str, id: &str) -> Option<Value> {
    panel["fieldConfig"]["overrides"]
        .as_array()?
        .iter()
        .filter(|over| over["matcher"]["id"] == matcher_id && over["matcher"]["options"] == matcher)
        .flat_map(|over| over["properties"].as_array().into_iter().flatten())
        .find(|property| property["id"] == id)
        .map(|property| property["value"].clone())
}

#[test]
fn the_spend_panel_draws_open_work_items_on_a_right_axis_apart_from_the_stack() {
    let panel = panels()
        .into_iter()
        .find(|panel| panel["title"] == "Spend by role")
        .expect("the dashboard has no Spend by role panel");
    let target = panel["targets"]
        .as_array()
        .expect("Spend by role has no targets")
        .iter()
        .find(|target| {
            target["expr"]
                .as_str()
                .is_some_and(|expr| expr.contains("kelpie_work_items"))
        })
        .expect("Spend by role has no target reading kelpie_work_items");
    let ref_id = target["refId"].as_str().expect("the target has no refId");

    assert_eq!(
        override_property(&panel, "byFrameRefID", ref_id, "custom.axisPlacement"),
        Some(Value::from("right")),
        "target {ref_id} is not on the right axis"
    );
    assert_eq!(
        override_property(&panel, "byFrameRefID", ref_id, "custom.stacking"),
        Some(serde_json::json!({ "mode": "none" })),
        "target {ref_id} is stacked with the roles"
    );
}

#[test]
fn the_agent_and_kind_table_draws_both_its_queries_as_columns_of_one_table() {
    let panel = panels()
        .into_iter()
        .find(|panel| panel["title"] == "Work items by agent and kind")
        .expect("the dashboard has no Work items by agent and kind panel");
    let targets = panel["targets"]
        .as_array()
        .expect("Work items by agent and kind has no targets");
    let refs: Vec<&str> = targets
        .iter()
        .map(|target| target["refId"].as_str().expect("a target has no refId"))
        .collect();

    // Two instant queries reach a table panel as two frames, and the panel draws
    // one at a time until a transformation joins them.
    let joined = panel["transformations"]
        .as_array()
        .unwrap_or_else(|| panic!("{} holds no transformations", panel["title"]))
        .iter()
        .any(|transformation| transformation["id"] == "merge");
    assert!(
        joined,
        "{} draws its two queries as two frames, with a frame picker",
        panel["title"]
    );

    // A rename matched to a frame heads every column of that frame, the agent
    // and kind columns included.
    let whole_frames: Vec<String> = panel["fieldConfig"]["overrides"]
        .as_array()
        .expect("Work items by agent and kind has no overrides")
        .iter()
        .filter(|over| over["matcher"]["id"] == "byFrameRefID")
        .filter(|over| {
            over["properties"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|property| property["id"] == "displayName")
        })
        .map(|over| {
            format!(
                "{} renames every column of target {}",
                panel["title"], over["matcher"]["options"]
            )
        })
        .collect();
    assert_no_problems(&whole_frames);

    // A Prometheus table query names its value column after its target.
    let mut headed: Vec<String> = Vec::new();
    for ref_id in &refs {
        let column = format!("Value #{ref_id}");
        let name = override_property(&panel, "byName", &column, "displayName")
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| panic!("{} names no column for target {ref_id}", panel["title"]));
        headed.push(name);
    }

    let problems: Vec<String> = panel["options"]["sortBy"]
        .as_array()
        .expect("Work items by agent and kind has no sortBy")
        .iter()
        .map(|entry| entry["displayName"].as_str().unwrap_or_default().to_owned())
        .map(|wanted| {
            let columns = headed
                .iter()
                .filter(|name| name.as_str() == wanted.as_str())
                .count();
            (wanted, columns)
        })
        .filter(|(_, columns)| *columns != 1)
        .map(|(wanted, columns)| {
            format!(
                "{} sorts on {wanted}, which heads {columns} of its columns",
                panel["title"]
            )
        })
        .collect();
    assert_no_problems(&problems);
}
