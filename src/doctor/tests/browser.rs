use super::*;

#[tokio::test(start_paused = true)]
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
    install_browser(&tools);
    ok(&scene.report().await, "golbat: browser");
}

#[tokio::test(start_paused = true)]
async fn missing_linux_browser_libraries_fail_the_report_with_install_deps() {
    let mut scene = Scene::new().await;
    scene.host.os = "linux";
    scene.host.expected_browsers = Some(Tools::under(&scene.kelpie_home).browsers());
    scene.host.browser_gaps = Some(Ok(vec!["libnss3.so".into(), "libatk-1.0.so.0".into()]));
    scene.runs("golbat", |t| t["browser"] = json!({ "enabled": true }));
    let tools = Tools::under(&scene.kelpie_home);
    install_browser(&tools);

    let report = scene.report().await;
    let (what, fix) = missing(&report, "golbat: browser");
    assert!(what.contains("libnss3.so and libatk-1.0.so.0"), "{what}");
    assert!(
        fix.contains("install-deps chromium-headless-shell"),
        "{fix}"
    );
    assert!(fix.contains("playwright-core/cli.js"), "{fix}");
    assert!(!report.passed());

    scene.host.browser_gaps = Some(Ok(Vec::new()));
    ok(&scene.report().await, "golbat: browser");
    scene.host.browser_gaps = Some(Err("no Playwright headless shell".into()));
    let report = scene.report().await;
    let Verdict::Unsure { what, next } = verdict(&report, "golbat: browser") else {
        panic!("a failed library probe must remain unsure");
    };
    assert_eq!(what, "no Playwright headless shell");
    assert_eq!(
        next,
        "check the headless shell installation and `ldd`, then rerun `shep kelpie doctor`"
    );
    assert!(report.passed());
    scene.host.os = "macos";
    scene.host.browser_gaps = Some(Err("ldd is Linux only".into()));
    ok(&scene.report().await, "golbat: browser");
}

// A browser presence check needs a server and any Chromium build, without installing real tools.
fn install_browser(tools: &Tools) {
    std::fs::create_dir_all(
        tools
            .playwright_mcp()
            .parent()
            .expect("MCP entry has a parent"),
    )
    .expect("make the browser server fixture folder");
    std::fs::write(tools.playwright_mcp(), "").expect("write the browser server fixture");
    std::fs::create_dir_all(tools.browsers().join("chromium-fixture"))
        .expect("make the Chromium presence fixture");
}
