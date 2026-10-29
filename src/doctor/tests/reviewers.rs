use super::*;

// A gateway in kelpie's settings whose key this shell does not hold
const PADDOCK: &str = "[gateways.paddock]\nurl = \"http://127.0.0.1:1\"\n\
                       key_env = \"KELPIE_TEST_KEY_NEVER_SET\"\n";

#[tokio::test]
async fn a_reviewer_on_a_gateway_kelpie_holds_is_a_candidate() {
    let scene = Scene::new().await;
    scene.shepherd.holds_section(&format!("{WEBHOOK}{PADDOCK}"));
    scene.runs("golbat", |t| {
        holds(t, &["iq3-xxs-review", "defect-hunter-lite"]);
    });
    let report = scene.report().await;
    assert_eq!(
        ok(&report, "golbat: reviewers"),
        "each pull request is read by defect-hunter, then defect-hunter-lite, then iq3-xxs-review"
    );
}

#[tokio::test]
async fn a_reviewer_the_project_named_on_a_gateway_kelpie_lacks_is_missing() {
    let scene = Scene::new().await;
    scene.runs("golbat", |t| {
        holds(t, &["iq3-xxs-review"]);
    });
    let report = scene.report().await;
    let (what, fix) = missing(&report, "golbat: reviewers");
    assert!(
        what.contains("iq3-xxs-review: gateway paddock is not in kelpie's settings"),
        "{what}"
    );
    assert!(fix.contains("`agents.allow`"), "{fix}");
}

#[tokio::test]
async fn a_reviewer_on_a_gateway_kelpie_lacks_is_no_candidate_when_the_project_named_none() {
    // `agents.allow` names no agent, so every reviewer file is a candidate and none
    // is held to what a round cannot work around: `iq3-xxs-review` is left out
    // rather than refused.
    let scene = Scene::new().await;
    let scripts = scene.home.join(".claude/scripts");
    std::fs::create_dir_all(&scripts).unwrap();
    crate::test::write_script(&scripts.join("qwen-review.sh"), "#!/bin/sh\nexit 0\n");
    scene.runs("golbat", |t| {
        t["agents"]["allow"] = json!([]);
    });
    let report = scene.report().await;
    let found = ok(&report, "golbat: reviewers");
    assert!(
        !found.contains("iq3-xxs-review"),
        "{found}: a candidate on a gateway kelpie's settings hold none of is left out, \
         not refused"
    );
}

#[tokio::test]
async fn an_old_reviewers_list_is_named_as_an_old_key() {
    let scene = Scene::new().await;
    scene.runs("golbat", |t| {
        t["agents"]["reviewers"] = json!(["opus-high", "fable"]);
    });
    let report = scene.report().await;

    let Verdict::Unsure { what, next } = verdict(&report, "golbat: old keys") else {
        panic!("{:#?}", report.render());
    };
    assert_eq!(what, "`agents.reviewers` read by nothing now");
    assert_eq!(
        next,
        "`shep kelpie upgrade` deletes them, as `shep kelpie settings migrate` does"
    );
    assert_eq!(
        ok(&report, "golbat: reviewers"),
        "each pull request is read by defect-hunter",
        "the old list names no reviewer"
    );
    assert!(!subjects(&report).contains(&"koji: old keys"));
}
