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

// A gateway in kelpie's settings whose key this shell does not hold, which the runner reads
// from its sheep's `env`
const PADDOCK: &str = "[gateways.paddock]\nurl = \"http://127.0.0.1:1\"\n\
                       key_env = \"KELPIE_TEST_KEY_NEVER_SET\"\n";

const ON_PADDOCK: &str = "---\nrole: implementer\nharness: pi\nmodel: m\neffort: low\n\
                          gateway: paddock\ncontext: 65536\ndescription: Builds on a local model.\n---\n";

#[tokio::test]
async fn an_implementer_on_a_gateway_whose_key_only_the_runner_holds_is_one_the_board_picks_among()
{
    let scene = Scene::new().await;
    scene.shepherd.holds_section(&format!("{WEBHOOK}{PADDOCK}"));
    crate::test::write_in(&scene.kelpie_home.join("agents"), "local.md", ON_PADDOCK);
    scene.runs("golbat", |_| {});
    let report = scene.report().await;
    assert!(
        !subjects(&report).contains(&"golbat: implementer local"),
        "{:#?}",
        report.render()
    );
    assert_eq!(
        ok(&report, "golbat: implementers"),
        "the agent board picks among local, opus-high, sonnet-high"
    );
}

#[tokio::test]
async fn implementers_all_left_out_of_the_boards_ask_are_missing() {
    let scene = Scene::new().await;
    crate::test::write_in(&scene.kelpie_home.join("agents"), "local.md", ON_PADDOCK);
    scene.runs("golbat", |t| {
        t["agents"]["allow"] = json!(["local", "standards"]);
    });
    let report = scene.report().await;
    let Verdict::Missing { what, fix } = verdict(&report, "golbat: implementers") else {
        panic!("{:#?}", report.render());
    };
    assert_eq!(
        what,
        "every implementer `agents.allow` holds is left out of the agent board's ask"
    );
    assert_eq!(fix, "correct what each `implementer` line names");
}
