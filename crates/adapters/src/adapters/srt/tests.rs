use super::*;

// What the `sandbox` job's adapters test binary ran from on cargo 1.99 (run
// 42943995947) and on cargo 1.100, which moved it (run 45832208016, #2008)
const TEST_BINARIES: [&str; 2] = [
    "debug/deps/shep_kelpie_adapters-9be137b26dd0620b",
    "debug/build/shep-kelpie-adapters/c16a292e05c1a8dc/out/shep_kelpie_adapters-c16a292e05c1a8dc",
];

#[test]
fn the_built_kelpie_is_read_off_a_test_binary_of_either_build_dir_layout() {
    let dir = tempfile::tempdir().unwrap();
    for path in TEST_BINARIES {
        let test = dir.path().join(path);
        std::fs::create_dir_all(test.parent().unwrap()).unwrap();
        std::fs::write(&test, "").unwrap();
        let kelpie = dir.path().join("debug").join(kelpie_name());
        std::fs::write(&kelpie, "").unwrap();
        assert_eq!(kelpie_above(&test).unwrap(), kelpie, "{path}");
    }
}

#[test]
fn no_built_kelpie_is_claimed_for_a_test_binary_built_without_one() {
    let dir = tempfile::tempdir().unwrap();
    let test = dir.path().join(TEST_BINARIES[1]);
    std::fs::create_dir_all(test.parent().unwrap()).unwrap();
    std::fs::write(&test, "").unwrap();
    assert_eq!(kelpie_above(&test), None);
}

#[test]
fn a_windows_machine_with_no_install_refuses_to_run_anything() {
    let dir = tempfile::tempdir().unwrap();
    let srt = SandboxRuntime::new()
        .on(Platform::Windows)
        .with_install(WindowsInstall {
            provisioned: false,
            setup_installed: None,
            ..WindowsInstall::provisioned(false)
        });
    let settings = dir.path().join("call.srt.json");
    let err = srt
        .wrap(
            &Policy::default(),
            &settings,
            &crate::spawn::command("claude"),
        )
        .unwrap_err();
    assert!(matches!(err, SandboxError::Install(_)), "{err:?}");
    assert!(err.to_string().contains("crabpot windows-install"), "{err}");
    assert!(!settings.exists());
}

#[test]
fn the_forwarded_traffic_goes_to_the_address_the_forwarder_listens_on() {
    let socket = PathBuf::from("/k/worker/model.sock");
    let policy = Policy {
        forward: Some(crate::ports::Forward {
            host: "model.kelpie.test".into(),
            socket: socket.clone(),
        }),
        ..Policy::default()
    };
    let settings = settings_on(&policy, Platform::here(), false);
    let dialed = settings["network"]["mitmProxy"]["socketPath"]
        .as_str()
        .unwrap();
    assert_eq!(Path::new(dialed), crate::ipc::address(&socket));
    if cfg!(windows) {
        assert!(dialed.starts_with(r"\\.\pipe\kelpie-"), "{dialed}");
    } else {
        assert_eq!(dialed, "/k/worker/model.sock");
    }
}

#[test]
fn a_forwarded_host_is_the_only_one_allowed_and_its_traffic_goes_to_the_socket() {
    let policy = Policy {
        forward: Some(crate::ports::Forward {
            host: "model.kelpie.test".into(),
            socket: PathBuf::from("/k/worker/model.sock"),
        }),
        ..Policy::default()
    };
    let network = &srt_settings(&policy)["network"];
    assert_eq!(network["allowedDomains"], json!(["model.kelpie.test"]));
    assert_eq!(network["deniedDomains"], json!([]));
    assert!(network.get("deniedResolvedAddresses").is_none());
    assert_eq!(
        network["mitmProxy"]["domains"],
        json!(["model.kelpie.test"])
    );
    assert!(network.get("allowUnixSockets").is_none());
    let denying = Policy {
        denied_hosts: vec!["192.0.2.9".into(), "localhost".into()],
        denied_addresses: vec!["192.0.2.9".into()],
        ..policy.clone()
    };
    let network = &srt_settings(&denying)["network"];
    assert_eq!(network["deniedDomains"], json!(["192.0.2.9", "localhost"]));
    assert_eq!(network["deniedResolvedAddresses"], json!(["192.0.2.9"]));
    let plain = &srt_settings(&Policy::default())["network"];
    assert!(plain.get("mitmProxy").is_none());
}

#[test]
fn a_policy_that_listens_binds_a_local_port_and_looks_up_its_services() {
    let listening = Policy {
        listen: true,
        services: vec!["com.apple.FSEvents".into()],
        ..Policy::default()
    };
    let network = &settings_on(&listening, Platform::Macos, false)["network"];
    assert_eq!(network["allowLocalBinding"], json!(true));
    assert_eq!(network["allowMachLookup"], json!(["com.apple.FSEvents"]));
    let plain = &settings_on(&Policy::default(), Platform::Macos, false)["network"];
    assert!(plain.get("allowLocalBinding").is_none(), "{plain}");
    assert!(plain.get("allowMachLookup").is_none(), "{plain}");
}

// On Windows the loopback mode is the install's, not the policy's: every
// worker on an `--allow-loopback` install gets `allowLocalBinding`, and a
// closed install writes nothing, whether or not the policy listens.
#[test]
fn on_windows_the_settings_ask_for_the_installs_loopback_mode_not_the_policys() {
    let listening = Policy {
        listen: true,
        ..Policy::default()
    };
    let open = &settings_on(&Policy::default(), Platform::Windows, true)["network"];
    assert_eq!(open["allowLocalBinding"], json!(true));
    let open_listening = &settings_on(&listening, Platform::Windows, true)["network"];
    assert_eq!(open_listening["allowLocalBinding"], json!(true));
    for policy in [&Policy::default(), &listening] {
        let closed = &settings_on(policy, Platform::Windows, false)["network"];
        assert!(closed.get("allowLocalBinding").is_none(), "{closed}");
    }
}

// From 0.0.80 srt blocks the user namespaces a command makes itself unless
// this is set, and setting it gives up the write denies the fence rests on
// (`docs/facts/worker-profile-and-sandbox.md`).
#[test]
fn a_command_may_not_make_user_namespaces_of_its_own() {
    for platform in [Platform::Macos, Platform::Linux] {
        let settings = settings_on(&Policy::default(), platform, false);
        assert!(
            settings.get("allowNestedUserNamespaces").is_none(),
            "{settings}"
        );
    }
}

#[test]
fn on_macos_an_opened_folder_is_read_alone_and_a_scratch_glob_is_written_one_level_down() {
    let policy = Policy {
        read: vec![PathBuf::from("/k/wt/7")],
        write: vec![PathBuf::from("/k/wt/7")],
        open: vec![PathBuf::from("/k/graphs"), PathBuf::from("/k/odd[1]*?")],
        scratch: vec!["/private/tmp/cbm-search-*".into()],
        ..Policy::default()
    };
    let files = &settings_on(&policy, Platform::Macos, false)["filesystem"];
    assert_eq!(
        files["allowRead"],
        json!(["/k/wt/7", "/k/graph[s]", "/k/odd[[]1[]][*][?]"])
    );
    assert_eq!(
        files["allowWrite"],
        json!([
            "/k/wt/7",
            "/private/tmp/cbm-search-*",
            "/private/tmp/cbm-search-*/*"
        ])
    );
    let files = &settings_on(&policy, Platform::Linux, false)["filesystem"];
    assert_eq!(files["allowRead"], json!(["/k/wt/7"]));
    assert_eq!(files["allowWrite"], json!(["/k/wt/7"]));
}

#[test]
fn on_windows_the_read_list_holds_no_root_and_no_path_without_a_drive() {
    let read = [
        r"C:\Users\R\.kelpie\wt\7",
        r"C:\Users\R\.kelpie\build",
        r"\\?\C:\Users\R\AppData\Roaming\npm",
        "/",
        "/bin",
        "/bin/sh",
        r"C:\",
        r"\\?\C:\",
    ];
    let policy = Policy {
        read: read.iter().map(PathBuf::from).collect(),
        ..Policy::default()
    };
    let files = &settings_on(&policy, Platform::Windows, false)["filesystem"];
    assert_eq!(
        files["allowRead"],
        json!([
            r"C:\Users\R\.kelpie\wt\7",
            r"C:\Users\R\.kelpie\build",
            r"\\?\C:\Users\R\AppData\Roaming\npm",
        ])
    );
    for platform in [Platform::Macos, Platform::Linux] {
        let files = &settings_on(&policy, platform, false)["filesystem"];
        assert_eq!(files["allowRead"], json!(read), "{platform:?}");
    }
}

#[test]
fn on_macos_a_socket_folder_is_listed_after_the_sockets_and_off_macos_it_is_not() {
    let socket = PathBuf::from("/k/dog/lease.sock");
    let folder = PathBuf::from("/private/k/call.tmp");
    let policy = |sockets: Vec<PathBuf>, socket_folders: Vec<PathBuf>| Policy {
        sockets,
        socket_folders,
        ..Policy::default()
    };
    let both = policy(vec![socket.clone()], vec![folder.clone()]);
    let folder_only = policy(vec![], vec![folder.clone()]);
    let socket_only = policy(vec![socket.clone()], vec![]);
    let neither = policy(vec![], vec![]);

    let listed = |policy: &Policy, platform: Platform| {
        settings_on(policy, platform, false)["network"]
            .get("allowUnixSockets")
            .cloned()
    };
    assert_eq!(
        listed(&both, Platform::Macos),
        Some(json!(["/k/dog/lease.sock", "/private/k/call.tmp"]))
    );
    assert_eq!(
        listed(&folder_only, Platform::Macos),
        Some(json!(["/private/k/call.tmp"]))
    );
    assert_eq!(
        listed(&socket_only, Platform::Macos),
        Some(json!(["/k/dog/lease.sock"]))
    );
    assert_eq!(listed(&neither, Platform::Macos), None);

    assert_eq!(
        listed(&both, Platform::Linux),
        Some(json!(["/k/dog/lease.sock"]))
    );
    assert_eq!(listed(&folder_only, Platform::Linux), None);
    assert_eq!(
        listed(&socket_only, Platform::Linux),
        Some(json!(["/k/dog/lease.sock"]))
    );
    assert_eq!(listed(&neither, Platform::Linux), None);
}

// The call every wrap test makes, and the settings file it writes
fn inner() -> Command {
    let mut inner = crate::spawn::command("claude");
    inner
        .args(["-p", "it's"])
        .current_dir("/k/wt/7")
        .env("CARGO_TARGET_DIR", "/k/targets/7")
        .env_remove("CLAUDECODE");
    inner
}

fn policy() -> Policy {
    Policy {
        write: vec![PathBuf::from("/k/wt/7")],
        hosts: vec!["api.anthropic.com".into()],
        ..Policy::default()
    }
}

fn written(settings: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(settings).unwrap()).unwrap()
}

fn assert_carries_the_call(wrapped: &Command) {
    assert_eq!(wrapped.get_current_dir(), Some(Path::new("/k/wt/7")));
    let envs: Vec<_> = wrapped.get_envs().collect();
    assert!(envs.contains(&("CARGO_TARGET_DIR".as_ref(), Some("/k/targets/7".as_ref()))));
    assert!(envs.contains(&("CLAUDECODE".as_ref(), None)));
}

#[test]
fn on_macos_the_wrapped_command_is_current_exe_crabpot_settings() {
    // Nothing is installed: crabpot is in kelpie, so no node `srt` is needed.
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("worker").join("call.srt.json");
    let wrapped = SandboxRuntime::new()
        .on(Platform::Macos)
        .wrap(&policy(), &settings, &inner())
        .unwrap();
    assert_eq!(
        wrapped.get_program(),
        std::env::current_exe().unwrap().as_os_str()
    );
    let args: Vec<_> = wrapped.get_args().collect();
    assert_eq!(
        args,
        [
            "--crabpot".as_ref(),
            "--settings".as_ref(),
            settings.as_os_str(),
            "--".as_ref(),
            "claude".as_ref(),
            "-p".as_ref(),
            "it's".as_ref(),
        ]
    );
    assert_carries_the_call(&wrapped);
    let written = written(&settings);
    assert_eq!(written, settings_on(&policy(), Platform::Macos, false));
    assert_eq!(written["network"]["strictAllowlist"], true);
    assert_eq!(written["filesystem"]["allowWrite"], json!(["/k/wt/7"]));
}

// No platform has a node `srt` to install. Windows needs a crabpot
// install instead, and `check` is the gate that names it.
#[test]
fn no_platform_needs_a_node_runtime_and_windows_needs_its_install() {
    let dir = tempfile::tempdir().unwrap();
    let srt = SandboxRuntime::new();
    srt.clone().on(Platform::Macos).check().unwrap();
    srt.clone().on(Platform::Linux).check().unwrap();
    srt.clone()
        .on(Platform::Windows)
        .with_install(WindowsInstall::provisioned(false))
        .check()
        .unwrap();
    let outdated = srt
        .clone()
        .on(Platform::Windows)
        .with_install(WindowsInstall {
            provisioned: false,
            setup_installed: Some(3),
            setup_expected: 4,
            ..WindowsInstall::provisioned(false)
        });
    let err = outdated.clone().check().unwrap_err();
    assert!(matches!(err, SandboxError::Install(_)), "{err:?}");
    assert!(
        err.to_string().contains("setup version 3")
            && err.to_string().contains("wants 4")
            && err.to_string().contains("crabpot windows-install"),
        "{err}"
    );
    outdated
        .wrap(&policy(), &dir.path().join("s.json"), &inner())
        .unwrap_err();
    let off = srt
        .clone()
        .on(Platform::Windows)
        .with_install(WindowsInstall {
            seclogon_runs: false,
            ..WindowsInstall::provisioned(false)
        });
    let err = off.check().unwrap_err();
    assert!(err.to_string().contains("Secondary Logon"), "{err}");
    let zero = srt.on(Platform::Windows).with_install(WindowsInstall {
        desktop_session: false,
        ..WindowsInstall::provisioned(false)
    });
    let err = zero.check().unwrap_err();
    assert!(err.to_string().contains("session 0"), "{err}");
}

// Nothing is installed: crabpot is in kelpie on Linux too, so no node `srt`
// and no `socat` is on that path.
#[test]
fn on_linux_the_wrapped_command_is_current_exe_crabpot_too() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("worker").join("call.srt.json");
    let wrapped = SandboxRuntime::new()
        .on(Platform::Linux)
        .wrap(&policy(), &settings, &inner())
        .unwrap();
    assert_eq!(
        wrapped.get_program(),
        std::env::current_exe().unwrap().as_os_str()
    );
    let args: Vec<_> = wrapped.get_args().collect();
    assert_eq!(
        args,
        [
            "--crabpot".as_ref(),
            "--settings".as_ref(),
            settings.as_os_str(),
            "--".as_ref(),
            "claude".as_ref(),
            "-p".as_ref(),
            "it's".as_ref(),
        ]
    );
    assert_carries_the_call(&wrapped);
    let written = written(&settings);
    assert_eq!(written, settings_on(&policy(), Platform::Linux, false));
    assert_eq!(written["network"]["strictAllowlist"], true);
    assert_eq!(written["filesystem"]["allowWrite"], json!(["/k/wt/7"]));
}

// A Windows worker runs through the same binary, and its settings carry
// the install's loopback mode: every worker on an `--allow-loopback`
// install gets `allowLocalBinding`, whether or not its policy listens.
#[test]
fn on_windows_the_wrapped_command_is_current_exe_crabpot_too() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("worker").join("call.srt.json");
    let srt = SandboxRuntime::new()
        .on(Platform::Windows)
        .with_install(WindowsInstall::provisioned(true));
    let wrapped = srt.wrap(&policy(), &settings, &inner()).unwrap();
    assert_eq!(
        wrapped.get_program(),
        std::env::current_exe().unwrap().as_os_str()
    );
    let args: Vec<_> = wrapped.get_args().collect();
    assert_eq!(
        args,
        [
            "--crabpot".as_ref(),
            "--settings".as_ref(),
            settings.as_os_str(),
            "--".as_ref(),
            "claude".as_ref(),
            "-p".as_ref(),
            "it's".as_ref(),
        ]
    );
    assert_carries_the_call(&wrapped);
    let written = written(&settings);
    assert_eq!(written, settings_on(&policy(), Platform::Windows, true));
    assert_eq!(written["network"]["allowLocalBinding"], json!(true));
}

// A session may not ask for the other loopback mode than the install
// fixed (srt-win refuses it as exit 19), so a policy that listens on a
// closed install is refused here, with the doctor's hint.
#[test]
fn a_listening_policy_on_a_closed_install_is_refused_with_the_doctors_hint() {
    let dir = tempfile::tempdir().unwrap();
    let srt = SandboxRuntime::new()
        .on(Platform::Windows)
        .with_install(WindowsInstall::provisioned(false));
    let listening = Policy {
        listen: true,
        ..policy()
    };
    let settings = dir.path().join("call.srt.json");
    let err = srt.wrap(&listening, &settings, &inner()).unwrap_err();
    assert!(matches!(err, SandboxError::Loopback), "{err:?}");
    let said = err.to_string();
    assert!(
        said.contains("crabpot windows-install --allow-loopback"),
        "{said}"
    );
    assert!(said.contains("loopback address"), "{said}");
    assert!(!settings.exists());
    // A policy that does not listen runs, and asks for nothing local.
    let wrapped = srt.wrap(&policy(), &settings, &inner()).unwrap();
    assert_eq!(
        written(&settings),
        settings_on(&policy(), Platform::Windows, false)
    );
    assert!(
        written(&settings)["network"]
            .get("allowLocalBinding")
            .is_none(),
        "{wrapped:?}"
    );
}

#[test]
fn no_keychain_adds_deny_mach_lookup_for_both_services() {
    let covered = Policy {
        no_keychain: true,
        services: vec!["com.apple.FSEvents".into()],
        ..Policy::default()
    };
    let network = &settings_on(&covered, Platform::Macos, false)["network"];
    assert_eq!(
        network["denyMachLookup"],
        json!(["com.apple.securityd.xpc", "com.apple.SecurityServer"])
    );
    // The allow is kept: crabpot denies last, over every allow.
    assert_eq!(network["allowMachLookup"], json!(["com.apple.FSEvents"]));
    // A call that leaves the keychain reachable denies nothing.
    let open = &settings_on(&Policy::default(), Platform::Macos, false)["network"];
    assert!(open.get("denyMachLookup").is_none(), "{open}");
    // There is no keychain to ask off macOS.
    let linux = &settings_on(&covered, Platform::Linux, false)["network"];
    assert!(linux.get("denyMachLookup").is_none(), "{linux}");
}

// The settings are read by crabpot's own parser, which refuses a key it
// cannot apply yet rather than drop it.
#[test]
fn crabpot_reads_every_setting_a_policy_can_write_on_macos() {
    let policy = Policy {
        read: vec![PathBuf::from("/k/wt/7")],
        write: vec![PathBuf::from("/k/wt/7")],
        no_read: vec!["/Users/me/**".into()],
        no_write: vec![PathBuf::from("/k/wt/7/.git/hooks")],
        open: vec![PathBuf::from("/k/graphs")],
        scratch: vec!["/private/tmp/cbm-search-*".into()],
        hosts: vec!["github.com".into()],
        denied_hosts: vec!["localhost".into()],
        denied_addresses: vec!["192.0.2.9".into()],
        forward: Some(crate::ports::Forward {
            host: "model.kelpie.test".into(),
            socket: PathBuf::from("/k/worker/model.sock"),
        }),
        sockets: vec![PathBuf::from("/k/dog/lease.sock")],
        socket_folders: vec![PathBuf::from("/private/k/call.tmp")],
        listen: true,
        services: vec!["com.apple.FSEvents".into()],
        verify_tls: true,
        no_keychain: true,
    };
    let text = settings_on(&policy, Platform::Macos, false).to_string();
    let config = crabpot::Config::from_json(&text).unwrap_or_else(|e| panic!("{e}: {text}"));
    assert_eq!(
        config.network.deny_mach_lookup,
        Some(vec![
            "com.apple.securityd.xpc".to_owned(),
            "com.apple.SecurityServer".to_owned()
        ])
    );
}
