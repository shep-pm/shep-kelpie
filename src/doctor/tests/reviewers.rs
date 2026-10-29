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
async fn a_candidate_the_project_named_no_that_cannot_run_is_no_line_s_to_refuse() {
    // `qwen` is a shipped reviewer file, so it is a candidate of every project
    // that names no agent, and its round is passed over when its script is not
    // there. Only a reviewer the project named is held to what a round cannot
    // work around, as the runner holds it.
    let scene = Scene::new().await;
    scene.runs("golbat", |t| {
        t["agents"]["allow"] = json!([]);
    });
    let report = scene.report().await;
    let found = ok(&report, "golbat: reviewers");
    assert!(
        found.contains("qwen"),
        "{found}: the candidate is still a candidate, and no line refuses it"
    );
}

#[tokio::test]
async fn a_review_bot_the_project_named_no_gets_no_line() {
    // `coderabbit`, `cubic` and `codex` are shipped reviewer files, so each is a
    // candidate of a project that names no agent, and a round passes it over: on
    // a repo that is not public, or as one never installed there.
    let scene = Scene::new().await;
    scene.forge.set_visibility(Visibility::Private);
    scene.runs("golbat", |t| {
        t["agents"]["allow"] = json!([]);
    });
    let report = scene.report().await;
    let subjects = subjects(&report);
    for bot in ["golbat: coderabbit", "golbat: cubic", "golbat: codex"] {
        assert!(
            !subjects.contains(&bot),
            "{subjects:?}: a bot the project named \
                 no is passed over at its round, so it is no line's to refuse"
        );
    }
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
