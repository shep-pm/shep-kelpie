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

    assert_eq!(
        ok(&report, "golbat: package registries"),
        "bun (BUN_INSTALL_CACHE_DIR), cargo"
    );
    assert!(report.passed(), "{:#?}", report.render());
}

#[tokio::test]
async fn each_manager_names_its_caches() {
    let scene = Scene::new().await;
    scene.runs("golbat", |_| {});
    keeps(&scene, &["go.mod", "uv.lock", "Cargo.lock"]);

    let report = scene.report().await;

    assert_eq!(
        ok(&report, "golbat: package registries"),
        "cargo, go (GOMODCACHE, GOCACHE), uv (UV_CACHE_DIR)"
    );
}

#[tokio::test]
async fn a_build_env_entry_a_manager_covers_is_named_as_not_needed() {
    let scene = Scene::new().await;
    scene.runs("golbat", |table| {
        table["worker"]["build_env"] = json!({"BUN_INSTALL_CACHE_DIR": "bun-cache"});
    });
    keeps(&scene, &["bun.lock"]);

    let report = scene.report().await;

    let found = ok(&report, "golbat: build_env BUN_INSTALL_CACHE_DIR");
    assert!(found.contains("no longer needed"), "{found}");
    assert!(found.contains("bun"), "{found}");
    assert!(report.passed(), "{:#?}", report.render());
}

#[tokio::test]
async fn a_build_env_entry_in_another_folder_is_an_override_and_stays() {
    let scene = Scene::new().await;
    scene.runs("golbat", |table| {
        table["worker"]["build_env"] = json!({
            "BUN_INSTALL_CACHE_DIR": "my-bun",
            "PLAYWRIGHT_BROWSERS_PATH": "browsers",
        });
    });
    keeps(&scene, &["bun.lock"]);

    let report = scene.report().await;

    assert!(
        !subjects(&report)
            .iter()
            .any(|subject| subject.starts_with("golbat: build_env")),
        "{:#?}",
        report.render()
    );
}

#[tokio::test]
async fn nothing_is_named_where_the_checkout_keeps_no_lockfile_or_manifest() {
    let scene = Scene::new().await;
    scene.runs("golbat", |_| {});

    let report = scene.report().await;

    assert!(!subjects(&report).contains(&"golbat: package registries"));
}

#[tokio::test]
async fn a_build_env_entry_of_a_cache_every_worker_gets_is_named_with_no_lockfile() {
    let scene = Scene::new().await;
    scene.runs("golbat", |table| {
        table["worker"]["build_env"] = json!({
            "GOCACHE": "go-build",
            "GOMODCACHE": "go-mod",
        });
    });

    let report = scene.report().await;

    let found = ok(&report, "golbat: build_env GOCACHE");
    assert!(found.contains("no longer needed"), "{found}");
    // `GOMODCACHE` follows detection, and this checkout keeps no go files.
    assert!(
        !subjects(&report).contains(&"golbat: build_env GOMODCACHE"),
        "{:#?}",
        report.render()
    );
}
