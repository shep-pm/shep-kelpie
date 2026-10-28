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

#[tokio::test]
async fn an_implementer_no_turn_can_run_on_is_named_unsure_and_left_out_of_the_picks() {
    let scene = Scene::new().await;
    let local = "---\nrole: implementer\nharness: pi\nmodel: m\neffort: low\n\
                 gateway: paddock\ncontext: 65536\ndescription: Builds on a local model.\n---\n";
    crate::test::write_in(&scene.kelpie_home.join("agents"), "local.md", local);
    scene.runs("golbat", |_| {});
    let report = scene.report().await;
    let Verdict::Unsure { what, .. } = verdict(&report, "golbat: implementer local") else {
        panic!("{:#?}", report.render());
    };
    assert!(
        what.starts_with("local is left out: gateway paddock is not in kelpie's settings"),
        "{what}"
    );
    assert_eq!(
        ok(&report, "golbat: implementers"),
        "the agent board picks among opus-high, sonnet-high"
    );
}
