use super::*;

#[tokio::test]
async fn an_installed_app_that_mints_a_token_is_ok() {
    let scene = Scene::new().await;

    let report = scene.report().await;

    assert_eq!(
        ok(&report, "koji: github app"),
        "kelpie's App is installed on shep-pm/koji and mints a token"
    );
}

#[tokio::test]
async fn an_owner_with_no_app_is_missing_and_names_setup() {
    let scene = Scene::new().await;
    scene.runs("golbat", |t| {
        t["git"]["remote"] = json!("someone-else/golbat");
    });

    let report = scene.report().await;

    let (what, fix) = missing(&report, "golbat: github app");
    assert_eq!(what, "kelpie has no GitHub App for someone-else");
    assert!(fix.contains("`shep kelpie github setup`"), "{fix}");
    assert!(fix.contains("`--org someone-else`"), "{fix}");
    assert!(!report.passed());
}

#[tokio::test]
async fn an_app_not_installed_on_the_repo_is_missing_and_names_its_install_page() {
    let scene = Scene::new().await;
    scene.runs("rotom", |_| {});

    let report = scene.report().await;

    let (what, fix) = missing(&report, "rotom: github app");
    assert_eq!(what, "kelpie-shep-pm is not installed on shep-pm/rotom");
    assert_eq!(
        fix,
        "install it there from https://github.com/apps/kelpie-shep-pm/installations/new"
    );
    ok(&report, "koji: github app");
    assert!(!report.passed());
}
