use super::*;

// Puts `files` in golbat's checkout
fn keeps(scene: &Scene, files: &[&str]) {
    for file in files {
        std::fs::write(scene.home.join("golbat").join(file), "").unwrap();
    }
}

#[tokio::test]
async fn each_manager_found_is_named() {
    let scene = Scene::new().await;
    scene.runs("golbat", |table| {
        table["worker"]["allowed_domains"] = json!([]);
    });
    keeps(&scene, &["bun.lock", "Cargo.lock"]);

    let report = scene.report().await;

    assert_eq!(ok(&report, "golbat: package registries"), "bun, cargo");
    assert!(report.passed(), "{:#?}", report.render());
}

#[tokio::test]
async fn nothing_is_named_where_the_checkout_keeps_no_lockfile_or_manifest() {
    let scene = Scene::new().await;
    scene.runs("golbat", |_| {});

    let report = scene.report().await;

    assert!(!subjects(&report).contains(&"golbat: package registries"));
}
