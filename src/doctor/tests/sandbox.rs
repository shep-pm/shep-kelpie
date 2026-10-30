use super::*;

#[tokio::test(start_paused = true)]
async fn missing_sandbox_programs_each_name_their_apt_package() {
    let mut scene = Scene::new().await;
    scene.host.os = "linux";
    for (gaps, packages) in [
        (vec!["bwrap", "socat"], "bubblewrap socat"),
        (vec!["socat"], "socat"),
        (vec!["bwrap"], "bubblewrap"),
    ] {
        scene.host.gaps = gaps;
        let report = scene.report().await;
        let (_, fix) = missing(&report, "sandbox");
        assert_eq!(
            fix,
            format!(
                "install {} with your package manager, such as `sudo apt-get install {packages}` on Debian and Ubuntu",
                scene.host.gaps.join(" and ")
            )
        );
        assert!(!report.passed());
    }
}

#[tokio::test]
async fn doctor_names_the_linux_sandbox_limits() {
    let mut scene = Scene::new().await;
    scene.host.os = "linux";
    let report = scene.report().await;
    let found = ok(&report, "sandbox: limits");
    assert!(
        found.contains("nested harness folders made during a call are caught after it"),
        "{found}"
    );
    assert!(
        found.contains("cannot reach the lease door, so `cargo-test` runs unleased"),
        "{found}"
    );
    assert!(report.passed());
    let order = subjects(&report);
    assert_eq!(order[2..4], ["sandbox", "sandbox: limits"]);

    for os in ["macos", ""] {
        scene.host.os = os;
        let report = scene.report().await;
        assert!(!subjects(&report).contains(&"sandbox: limits"), "{os}");
    }
}

// crabpot is in kelpie on every platform, so no platform has a node srt to
// miss. Windows fences with an install, and its line names each state of it.
#[tokio::test]
async fn macos_and_linux_need_nothing_installed_and_windows_names_its_install() {
    let mut scene = Scene::new().await;

    for os in ["macos", "linux"] {
        scene.host.os = os;
        scene.host.keychain = os == "macos";
        let report = scene.report().await;
        assert_eq!(
            ok(&report, "sandbox"),
            "the sandbox runtime can run",
            "{os}: {:#?}",
            report.render()
        );
    }

    scene.host.os = "windows";
    scene.host.keychain = false;
    let report = scene.report().await;
    assert_eq!(
        ok(&report, "sandbox"),
        "the crabpot install can run, loopback closed",
        "{:#?}",
        report.render()
    );

    scene.host.windows = Some(Ok(WindowsInstall::provisioned(true)));
    let report = scene.report().await;
    assert_eq!(
        ok(&report, "sandbox"),
        "the crabpot install can run, loopback open"
    );
}

#[tokio::test]
async fn a_windows_machine_with_no_install_is_told_to_make_one() {
    let mut scene = Scene::new().await;
    scene.host.os = "windows";
    scene.host.windows = Some(Ok(WindowsInstall {
        provisioned: false,
        setup_installed: None,
        ..WindowsInstall::provisioned(false)
    }));
    let report = scene.report().await;
    let (what, fix) = missing(&report, "sandbox");
    assert!(what.contains("no crabpot install"), "{what}");
    assert_eq!(fix, "run `crabpot windows-install`");
    assert!(!report.passed());
}

#[tokio::test]
async fn a_windows_install_of_another_setup_version_names_both_numbers() {
    let mut scene = Scene::new().await;
    scene.host.os = "windows";
    scene.host.windows = Some(Ok(WindowsInstall {
        provisioned: false,
        setup_installed: Some(3),
        setup_expected: 4,
        ..WindowsInstall::provisioned(false)
    }));
    let (what, fix) = missing(&scene.report().await, "sandbox");
    assert!(
        what.contains("setup version 3") && what.contains("wants 4"),
        "{what}"
    );
    assert_eq!(fix, "run `crabpot windows-install`");
}

#[tokio::test]
async fn a_windows_machine_with_seclogon_off_or_in_session_0_is_told_its_fix() {
    let mut scene = Scene::new().await;
    scene.host.os = "windows";
    scene.host.windows = Some(Ok(WindowsInstall {
        seclogon_runs: false,
        ..WindowsInstall::provisioned(false)
    }));
    let (what, fix) = missing(&scene.report().await, "sandbox");
    assert!(what.contains("Secondary Logon"), "{what}");
    assert!(fix.contains("seclogon"), "{fix}");

    scene.host.windows = Some(Ok(WindowsInstall {
        desktop_session: false,
        ..WindowsInstall::provisioned(false)
    }));
    let (what, fix) = missing(&scene.report().await, "sandbox");
    assert!(what.contains("session 0"), "{what}");
    assert!(fix.contains("desktop session"), "{fix}");

    scene.host.windows = Some(Err("srt-win user status exited 1"));
    let (what, fix) = missing(&scene.report().await, "sandbox");
    assert!(what.contains("cannot be read"), "{what}");
    assert_eq!(fix, "run `crabpot windows-install`");
}

// A `cargo install` moves the new exe in from `%TEMP%`, so it keeps that
// folder's access list and inherits nothing from the folder it lands in, where
// the install's grant sits. A re-grant writes nothing to it, and only a reset
// re-applies what the folder passes down.
#[tokio::test]
async fn a_windows_runner_the_sandbox_cannot_read_names_the_exe_and_its_reset() {
    let mut scene = Scene::new().await;
    scene.host.os = "windows";
    scene.host.windows = Some(Ok(WindowsInstall {
        runner_readable: Some(false),
        ..WindowsInstall::provisioned(false)
    }));
    let report = scene.report().await;
    let (what, fix) = missing(&report, "sandbox");
    let exe = std::env::current_exe().unwrap().display().to_string();
    assert!(what.contains("cannot read kelpie"), "{what}");
    assert!(what.contains(&exe), "{what}");
    assert!(
        what.contains("cargo install") && what.contains("%TEMP%"),
        "{what}"
    );
    assert!(fix.contains(&format!("`icacls \"{exe}\" /reset`")), "{fix}");
    assert!(!report.passed());
}

// A runner the check read as readable is no gap, and one the machine could not
// check refuses nothing, which is how crabpot's own start leaves it.
#[tokio::test]
async fn a_windows_runner_that_is_readable_or_unchecked_leaves_the_sandbox_line_ok() {
    let mut scene = Scene::new().await;
    scene.host.os = "windows";
    for readable in [Some(true), None] {
        scene.host.windows = Some(Ok(WindowsInstall {
            runner_readable: readable,
            ..WindowsInstall::provisioned(false)
        }));
        let report = scene.report().await;
        assert_eq!(
            ok(&report, "sandbox"),
            "the crabpot install can run, loopback closed",
            "{readable:?}: {:#?}",
            report.render()
        );
    }
}

// A project with the browser on and a launch file on `main` gets a dev server
// that binds a local port, and only an `--allow-loopback` install can hold it.
// The fix says what opening loopback allows.
#[tokio::test]
async fn a_listening_project_on_a_closed_windows_install_is_told_the_widening() {
    let mut scene = Scene::new().await;
    scene.host.os = "windows";
    scene.runs("golbat", |t| t["browser"] = json!({ "enabled": true }));
    scene.launches("golbat");
    let report = scene.report().await;
    let (what, fix) = missing(&report, "sandbox");
    assert!(what.contains("dev server"), "{what}");
    assert!(what.contains("closes loopback"), "{what}");
    assert!(
        fix.contains("crabpot windows-install --allow-loopback"),
        "{fix}"
    );
    assert!(
        fix.contains("every service on its own loopback address"),
        "{fix}"
    );
    assert!(!report.passed());

    // With the install open, the same project runs, and the line says so.
    scene.host.windows = Some(Ok(WindowsInstall::provisioned(true)));
    let report = scene.report().await;
    assert_eq!(
        ok(&report, "sandbox"),
        "the crabpot install can run, loopback open"
    );

    // A missing install on such a project is fixed with the open form too.
    scene.host.windows = Some(Ok(WindowsInstall {
        provisioned: false,
        setup_installed: None,
        ..WindowsInstall::provisioned(false)
    }));
    let (_, fix) = missing(&scene.report().await, "sandbox");
    assert_eq!(fix, "run `crabpot windows-install --allow-loopback`");
}

// The runner plans a dev server only when the base branch has a launch file
// with a configuration, so a browser project without one listens on nothing,
// and a closed install is not a gap for it.
#[tokio::test]
async fn a_browser_project_with_no_launch_file_does_not_ask_to_open_loopback() {
    let mut scene = Scene::new().await;
    scene.host.os = "windows";
    scene.runs("golbat", |t| t["browser"] = json!({ "enabled": true }));
    let report = scene.report().await;
    assert_eq!(
        ok(&report, "sandbox"),
        "the crabpot install can run, loopback closed",
        "{:#?}",
        report.render()
    );

    // A launch file on `main` with no configuration starts no server either.
    scene.launches("golbat");
    std::fs::write(
        scene.home.join("golbat/.claude/launch.json"),
        r#"{"configurations": []}"#,
    )
    .unwrap();
    git(&scene.home.join("golbat"), &["add", "--all"]);
    git(&scene.home.join("golbat"), &["commit", "-q", "-m", "empty"]);
    git(
        &scene.home.join("golbat"),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    let report = scene.report().await;
    assert_eq!(
        ok(&report, "sandbox"),
        "the crabpot install can run, loopback closed",
        "{:#?}",
        report.render()
    );
}

// Asked about one project, the sandbox line is fitted to that project's plan,
// not to any other project's.
#[tokio::test]
async fn the_loopback_advice_follows_the_project_doctor_was_asked_about() {
    let mut scene = Scene::new().await;
    scene.host.os = "windows";
    scene.runs("golbat", |t| t["browser"] = json!({ "enabled": true }));
    scene.launches("golbat");

    let koji = ProjectName::try_from("koji").unwrap();
    let report = scene
        .check(Ask {
            project: Some(&koji),
            test_alert: false,
        })
        .await;
    assert_eq!(
        ok(&report, "sandbox"),
        "the crabpot install can run, loopback closed",
        "{:#?}",
        report.render()
    );

    let golbat = ProjectName::try_from("golbat").unwrap();
    let report = scene
        .check(Ask {
            project: Some(&golbat),
            test_alert: false,
        })
        .await;
    let (_, fix) = missing(&report, "sandbox");
    assert!(
        fix.contains("crabpot windows-install --allow-loopback"),
        "{fix}"
    );
}
