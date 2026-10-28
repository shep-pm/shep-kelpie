use super::*;

#[tokio::test]
async fn the_keys_the_agent_board_replaced_are_named_in_one_unsure_line_saying_upgrade_deletes_them()
 {
    let scene = Scene::new().await;
    scene.runs("golbat", |t| {
        t["agents"]["implementers"] = json!(["sonnet-high"]);
        t["agents"]["fallback_after"] = json!(15);
    });
    let report = scene.report().await;
    let Verdict::Unsure { what, next } = verdict(&report, "golbat: old keys") else {
        panic!("{:#?}", report.render());
    };
    assert_eq!(
        what,
        "`agents.implementers`, `agents.fallback_after` read by nothing now"
    );
    assert_eq!(
        next,
        "`shep kelpie upgrade` deletes them, as `shep kelpie settings migrate` does"
    );
    assert!(!subjects(&report).contains(&"koji: old keys"));
}
