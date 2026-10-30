use super::*;

#[tokio::test]
async fn a_project_with_the_browser_on_is_missing_tools_that_are_not_installed() {
    let scene = Scene::new().await;
    scene.runs("golbat", |t| t["browser"] = json!({ "enabled": true }));
    let report = scene.report().await;
    let (what, fix) = missing(&report, "golbat: browser");
    assert!(what.contains("Playwright"), "{what}");
    assert_eq!(
        fix,
        "run `shep kelpie tools install`, or turn `browser.enabled` off"
    );
    assert!(
        !subjects(&report).contains(&"koji: browser"),
        "off is not checked"
    );

    let tools = Tools::under(&scene.kelpie_home);
    std::fs::create_dir_all(tools.playwright_mcp().parent().unwrap()).unwrap();
    std::fs::write(tools.playwright_mcp(), "").unwrap();
    std::fs::create_dir_all(tools.browsers().join("chromium-1200")).unwrap();
    ok(&scene.report().await, "golbat: browser");
}

#[tokio::test(start_paused = true)]
async fn missing_linux_browser_libraries_fail_the_report_with_install_deps() {
    let mut scene = Scene::new().await;
    scene.host.os = "linux";
    scene.host.browser = Some(Ok(vec!["libnss3.so".into(), "libatk-1.0.so.0".into()]));
    scene.runs("golbat", |t| t["browser"] = json!({ "enabled": true }));
    let tools = Tools::under(&scene.kelpie_home);
    std::fs::create_dir_all(tools.playwright_mcp().parent().unwrap()).unwrap();
    std::fs::write(tools.playwright_mcp(), "").unwrap();
    std::fs::create_dir_all(tools.browsers().join("chromium-1200")).unwrap();

    let report = scene.report().await;
    let (what, fix) = missing(&report, "golbat: browser");
    assert!(what.contains("libnss3.so and libatk-1.0.so.0"), "{what}");
    assert!(
        fix.contains("install-deps chromium-headless-shell"),
        "{fix}"
    );
    assert!(fix.contains("playwright-core/cli.js"), "{fix}");
    assert!(!report.passed());

    scene.host.browser = Some(Ok(Vec::new()));
    ok(&scene.report().await, "golbat: browser");
    scene.host.os = "macos";
    scene.host.browser = Some(Err("ldd is Linux only".into()));
    ok(&scene.report().await, "golbat: browser");
}
