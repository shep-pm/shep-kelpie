use std::path::PathBuf;

use super::*;
use crate::hooks::Hooks;
use crate::ports::{Fence, Guard};
use crate::trim::deny_with_trim;

#[test]
fn a_compaction_hook_leaves_a_mark_in_the_scratch_folder_and_prints_the_prompt_back() {
    let dir = tempfile::tempdir().unwrap();
    let prompt = dir.path().join("settings.prompt.md");
    std::fs::write(&prompt, "do the thing\n").unwrap();
    let marks = compactions_file(&dir.path().join("settings.tmp"));
    std::fs::create_dir_all(marks.parent().unwrap()).unwrap();
    let mut value = json!({});
    compacting(
        &mut value,
        NonZeroU32::new(200_000).unwrap(),
        &prompt,
        &marks,
    );
    let hook = &value["hooks"]["SessionStart"][0];
    assert_eq!(hook["matcher"], "compact");
    let command = hook["hooks"][0]["command"].as_str().unwrap();
    for _ in 0..2 {
        let out = std::process::Command::new(crate::spawn::sh())
            .args(["-c", command])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "do the thing\n");
    }
    assert_eq!(std::fs::read_to_string(&marks).unwrap(), "..");
}

#[test]
fn a_denied_folder_holding_a_readable_one_is_denied_around_it() {
    let dir = tempfile::tempdir().unwrap();
    let shep = dir.path();
    let own = shep.join("kelpie/koji/worktrees/7");
    for folder in ["run", "kelpie/rotom", "kelpie/koji/worktrees/8"] {
        std::fs::create_dir_all(shep.join(folder)).unwrap();
    }
    std::fs::create_dir_all(&own).unwrap();
    std::fs::write(shep.join("dogs.toml"), "").unwrap();
    let no_read = [format!("{}/**", shep.display()), "~/.ssh/**".to_owned()];

    let rules = read_denials(&no_read, &[own]);

    let at = |p: &str| spellings(&shep.join(p));
    let folder = |p: &str| at(p).into_iter().map(|s| format!("{s}/**"));
    let expected: Vec<String> = at("dogs.toml")
        .into_iter()
        .chain(folder("kelpie/koji/worktrees/8"))
        .chain(folder("kelpie/rotom"))
        .chain(folder("run"))
        .chain(["~/.ssh/**".to_owned()])
        .collect();
    assert_eq!(rules, expected);
}

// `path` as given, then with its links resolved where that differs.
fn spellings(path: &Path) -> Vec<String> {
    let resolved = path.canonicalize().ok().filter(|c| c != path);
    std::iter::once(path.to_owned())
        .chain(resolved)
        .map(|p| p.display().to_string())
        .collect()
}

#[cfg(unix)]
#[test]
fn a_denied_home_reached_through_a_link_is_denied_in_both_spellings() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    for folder in ["toolchain/bin", "other"] {
        std::fs::create_dir_all(real.join(folder)).unwrap();
    }
    std::fs::write(real.join("credentials"), "").unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let no_read = [format!("{}/**", link.display())];

    let rules = read_denials(&no_read, &[link.join("toolchain")]);

    let folder = |p: &str| {
        spellings(&link.join(p))
            .into_iter()
            .map(|s| format!("{s}/**"))
    };
    let expected: Vec<String> = spellings(&link.join("credentials"))
        .into_iter()
        .chain(folder("other"))
        .collect();
    assert_eq!(rules, expected);
    assert_ne!(
        link.canonicalize().unwrap(),
        link,
        "the denied home is reached through a link"
    );
    assert!(rules.iter().all(|r| !r.contains("toolchain")));
}

#[cfg(unix)]
#[test]
fn a_readable_folder_spelled_through_the_link_or_not_is_denied_in_neither() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    for folder in ["a", "b", "c"] {
        std::fs::create_dir_all(real.join(folder)).unwrap();
    }
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let no_read = [format!("{}/**", link.display())];

    let rules = read_denials(&no_read, &[link.join("a"), real.join("b")]);

    let expected: Vec<String> = spellings(&link.join("c"))
        .into_iter()
        .map(|s| format!("{s}/**"))
        .collect();
    assert_eq!(rules, expected);
}

#[cfg(unix)]
#[test]
fn a_denied_folder_that_holds_nothing_readable_is_denied_in_both_spellings() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    let plain = dir.path().join("plain");
    for folder in [&real, &plain] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let no_read = [
        format!("{}/**", link.display()),
        format!("{}/**", plain.display()),
    ];

    let rules = read_denials(&no_read, &[]);

    let expected: Vec<String> = spellings(&link)
        .into_iter()
        .chain(spellings(&plain))
        .map(|s| format!("{s}/**"))
        .collect();
    assert_eq!(rules, expected);
    assert_eq!(
        rules.iter().filter(|r| r.contains("plain")).count(),
        1,
        "a folder that is no link is named once"
    );
}

#[cfg(unix)]
#[test]
fn a_credential_named_by_path_under_a_folder_behind_a_link_is_denied_in_both_spellings() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::write(real.join("credentials"), "").unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let no_read = [
        link.join("credentials").display().to_string(),
        link.join("credentials.toml").display().to_string(),
    ];

    let rules = read_denials(&no_read, &[]);

    let expected: Vec<String> = spellings(&link.join("credentials"))
        .into_iter()
        .chain([link.join("credentials.toml").display().to_string()])
        .collect();
    assert_eq!(rules, expected, "a file not on disk is named as given");
    assert_eq!(rules.len(), 3);
}

#[cfg(unix)]
#[test]
fn the_settings_deny_a_credential_behind_a_link_in_both_spellings() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::write(real.join("credentials"), "").unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let given = link.join("credentials");
    let reach = Reach {
        fence: Some(Box::new(Fence {
            write: Vec::new(),
            no_write: Vec::new(),
            no_read: vec![given.display().to_string()],
            read: Vec::new(),
            hosts: Vec::new(),
            no_commands: Vec::new(),
            env: Default::default(),
            sockets: Vec::new(),
            guard: Guard {
                kelpie: "/k/kelpie".into(),
                worktree: "/k/wt".into(),
                build: "/k/build".into(),
                git_common_dir: "/k/git".into(),
                base: "main".into(),
                kelpie_pushes: false,
                folders: Vec::new(),
                hooks: Hooks::default(),
            },
            hooks: Vec::new(),
        })),
        ..Reach::default()
    };

    let s = settings(Tools::Work, &reach);

    let deny = s["permissions"]["deny"].as_array().unwrap();
    for spelled in spellings(&given) {
        assert!(
            deny.contains(&json!(format!("Read(/{spelled})"))),
            "{spelled} is denied"
        );
    }
    assert_eq!(spellings(&given).len(), 2);
}

#[cfg(unix)]
#[test]
fn a_readable_folder_spelled_only_as_the_real_one_carves_a_folder_denied_through_the_link() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    for folder in ["a", "b"] {
        std::fs::create_dir_all(real.join(folder)).unwrap();
    }
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let no_read = [format!("{}/**", link.display())];

    let rules = read_denials(&no_read, &[real.join("a")]);

    let expected: Vec<String> = spellings(&link.join("b"))
        .into_iter()
        .map(|s| format!("{s}/**"))
        .collect();
    assert_eq!(rules, expected);
}

#[test]
fn a_review_round_keeps_its_read_tools_and_reads_its_folders() {
    let bare = settings(Tools::Review, &Reach::default());
    assert_eq!(bare["permissions"]["deny"], deny_with_trim(&REVIEW_DENY));
    let extra = Reach {
        read: vec![PathBuf::from("/k/extra/7")],
        fence: None,
        ..Reach::default()
    };
    assert_eq!(
        settings(Tools::Review, &extra)["permissions"]["additionalDirectories"],
        json!(["/k/extra/7"])
    );
}

#[test]
fn a_retro_may_call_a_skill_with_or_without_a_fence() {
    let fenced = Reach {
        read: Vec::new(),
        fence: Some(Box::new(Fence {
            write: Vec::new(),
            no_write: Vec::new(),
            no_read: Vec::new(),
            read: Vec::new(),
            hosts: Vec::new(),
            no_commands: Vec::new(),
            env: Default::default(),
            sockets: Vec::new(),
            guard: Guard {
                kelpie: "/k/kelpie".into(),
                worktree: "/k/wt".into(),
                build: "/k/build".into(),
                git_common_dir: "/k/git".into(),
                base: "main".into(),
                kelpie_pushes: false,
                folders: Vec::new(),
                hooks: Hooks::default(),
            },
            hooks: Vec::new(),
        })),
        ..Reach::default()
    };
    for reach in [Reach::default(), fenced] {
        let s = settings(Tools::Retro, &reach);
        assert_eq!(s["permissions"]["allow"], json!(["Skill"]));
    }
    let review = settings(Tools::Review, &Reach::default());
    assert!(review["permissions"].get("allow").is_none());
}

// Claude Code lets a call through when its guard outlasts the hook's
// timeout, so with the project's hooks the guard's entry waits longer than
// the guard spends on them, and without them it keeps the default.
#[test]
fn the_guard_waits_longer_than_it_spends_on_the_projects_hooks() {
    use crate::settings::HookPoint;
    let reach = reach_with;
    let guard_entry = |reach: &Reach| {
        let pre = settings(Tools::Work, reach)["hooks"]["PreToolUse"].clone();
        let bash = pre
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["matcher"] == GUARDED_TOOLS);
        bash.unwrap()["hooks"][0].clone()
    };
    let with = guard_entry(&reach(hooks_at(&[HookPoint::Commit])));
    assert_eq!(with["timeout"], GUARD_BUDGET.as_secs() + GUARD_MARGIN);
    assert!(
        guard_entry(&reach(Hooks::default()))
            .get("timeout")
            .is_none()
    );
}

#[test]
fn the_guard_is_told_the_base_branch_only_when_it_is_not_the_default() {
    let guard_command = |base: &str| {
        let mut reach = reach_with(Hooks::default());
        reach.fence.as_mut().unwrap().guard.base = base.to_owned();
        let pre = settings(Tools::Work, &reach)["hooks"]["PreToolUse"].clone();
        let pre = pre.as_array().unwrap();
        let bash = pre.iter().find(|e| e["matcher"] == GUARDED_TOOLS).unwrap();
        bash["hooks"][0]["command"].as_str().unwrap().to_owned()
    };
    assert!(guard_command("v2").ends_with(" '--base=v2'"));
    assert!(!guard_command("main").contains("--base"));
}

// A fenced worker's reach with `hooks` as the project's own.
fn reach_with(hooks: Hooks) -> Reach {
    Reach {
        read: Vec::new(),
        fence: Some(Box::new(Fence {
            write: Vec::new(),
            no_write: Vec::new(),
            no_read: Vec::new(),
            read: Vec::new(),
            hosts: Vec::new(),
            no_commands: Vec::new(),
            env: Default::default(),
            sockets: Vec::new(),
            guard: Guard {
                kelpie: "/k/kelpie".into(),
                worktree: "/k/wt".into(),
                build: "/k/build".into(),
                git_common_dir: "/k/git".into(),
                base: "main".into(),
                kelpie_pushes: false,
                folders: Vec::new(),
                hooks,
            },
            hooks: Vec::new(),
        })),
        ..Reach::default()
    }
}

fn hooks_at(points: &[crate::settings::HookPoint]) -> Hooks {
    use crate::settings::{NonBlank, ProjectHook};
    let list: Vec<ProjectHook> = points
        .iter()
        .map(|&point| ProjectHook {
            point,
            command: NonBlank::try_from("exit 0".to_owned()).unwrap(),
        })
        .collect();
    Hooks::new("koji", "acme/koji", Path::new("/"), &list)
}

fn commands_of(entries: &Value, matcher: &str) -> Vec<String> {
    let entries = entries.as_array().cloned().unwrap_or_default();
    (entries.iter())
        .filter(|e| e["matcher"] == matcher)
        .map(|e| e["hooks"][0]["command"].as_str().unwrap().to_owned())
        .collect()
}

// Claude Code runs the project's `write` hooks through `confine`, and its
// `after_command` ones through a second guard entry on each command's result,
// a success and a failure alike, since it fires a different event for each.
#[test]
fn the_projects_write_and_after_command_hooks_ride_on_confine_and_the_guard() {
    use crate::settings::HookPoint;
    let hooks = hooks_at(&[
        HookPoint::Write,
        HookPoint::AfterCommand,
        HookPoint::Command,
    ]);
    let flag = shell_quote(&hooks.flag().unwrap());

    let s = settings(Tools::Work, &reach_with(hooks));

    let confine = commands_of(&s["hooks"]["PreToolUse"], FILE_TOOLS);
    assert_eq!(confine.len(), 1);
    assert!(confine[0].ends_with(&flag), "{}", confine[0]);
    for event in ["PostToolUse", "PostToolUseFailure"] {
        let after = commands_of(&s["hooks"][event], "Bash");
        assert_eq!(after.len(), 1, "{event}");
        assert!(after[0].contains("guard"), "{event}: {}", after[0]);
        assert!(after[0].contains(&flag), "{event}: {}", after[0]);
        assert!(after[0].ends_with(" --after"), "{event}: {}", after[0]);
        let entry = &s["hooks"][event][0]["hooks"][0];
        assert_eq!(entry["timeout"], GUARD_BUDGET.as_secs() + GUARD_MARGIN);
    }
    let pre = &s["hooks"]["PreToolUse"][0]["hooks"][0];
    assert_eq!(pre["timeout"], GUARD_BUDGET.as_secs() + GUARD_MARGIN);
}

#[test]
fn no_write_or_after_command_hook_adds_no_flag_to_confine_and_no_post_entry() {
    use crate::settings::HookPoint;
    let s = settings(Tools::Work, &reach_with(hooks_at(&[HookPoint::Commit])));

    let confine = commands_of(&s["hooks"]["PreToolUse"], FILE_TOOLS);
    assert!(!confine[0].contains("--hooks="), "{}", confine[0]);
    // The only post entry is the pause hook, which has no matcher.
    for event in ["PostToolUse", "PostToolUseFailure"] {
        assert!(
            commands_of(&s["hooks"][event], "Bash").is_empty(),
            "{event}"
        );
        assert_eq!(s["hooks"][event].as_array().unwrap().len(), 1, "{event}");
    }
}

#[test]
fn an_answer_reads_nothing_unless_its_sandbox_lists_a_folder() {
    let none = settings(Tools::Answer, &Reach::default());
    assert_eq!(none["permissions"]["deny"], deny_with_trim(&NO_TOOLS));
    let extra = Reach {
        read: vec![PathBuf::from("/k/extra/7")],
        fence: None,
        ..Reach::default()
    };
    let deny = settings(Tools::Answer, &extra)["permissions"]["deny"].clone();
    let without_read: Vec<&str> = NO_TOOLS.into_iter().filter(|t| *t != "Read").collect();
    assert_eq!(deny, deny_with_trim(&without_read));
}

// The file tools each role's settings may leave undenied, with the reason.
// A match with no catch-all, so a new role cannot join without a decision.
fn file_tools_left_open(tools: Tools) -> &'static [&'static str] {
    match tools {
        // It builds the change, so it writes files, held to its folders by `confine`.
        Tools::Work => &["Edit", "Write", "MultiEdit", "NotebookEdit"],
        // It may edit its notes file only: `Edit(<notes>)` allows it and
        // `confine --append` holds it to appending (`Write` rides the `Edit` rule).
        // `MultiEdit` has no deny rule: Claude Code warns it matches no known
        // tool, and the `--tools` allow-list already leaves it out.
        Tools::Pm => &["Edit", "Write", "MultiEdit"],
        // Known gap: a reviewer's session is read-only by design (Read, Grep and Glob),
        // but its own deny list holds `Agent`, `Task` and `Bash` only. Every settings
        // file denies `NotebookEdit` through `trimmed`, so that one is asserted. Kelpie's
        // sandbox holds the writes of the other three, so no deny is asserted for them yet.
        Tools::Review => &["Edit", "Write", "MultiEdit"],
        Tools::Answer | Tools::Retro => &[],
    }
}

// Every role, walked by a match with no catch-all: a new `Tools` variant fails to
// compile here until it is chained in, so the test below cannot skip it.
fn next_role(tools: Tools) -> Option<Tools> {
    match tools {
        Tools::Work => Some(Tools::Review),
        Tools::Review => Some(Tools::Answer),
        Tools::Answer => Some(Tools::Pm),
        Tools::Pm => Some(Tools::Retro),
        Tools::Retro => None,
    }
}

#[test]
fn every_role_denies_every_file_tool_it_does_not_leave_open() {
    let file_tools: Vec<&str> = FILE_TOOLS.split('|').collect();
    assert!(file_tools.contains(&"MultiEdit"), "{file_tools:?}");
    let mut role = Some(Tools::Work);
    while let Some(tools) = role {
        role = next_role(tools);
        let s = settings(tools, &Reach::default());
        let denied: Vec<&str> = s["permissions"]["deny"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let open = file_tools_left_open(tools);
        for tool in &file_tools {
            if !open.contains(tool) {
                assert!(denied.contains(tool), "{tools:?} does not deny {tool}");
            }
        }
    }
}

#[test]
fn every_role_drops_the_features_it_never_uses() {
    // A worker's fenced settings are pinned in `profile`'s tests.
    for tools in [
        Tools::Work,
        Tools::Review,
        Tools::Answer,
        Tools::Pm,
        Tools::Retro,
    ] {
        let s = settings(tools, &Reach::default());
        for key in [
            "disableBundledSkills",
            "disableWorkflows",
            "disableClaudeAiConnectors",
            "disableArtifact",
            "disableRemoteControl",
        ] {
            assert_eq!(s[key], true, "{tools:?} {key}");
        }
        assert_eq!(s["enableArtifact"], false, "{tools:?}");
        let own: &[&str] = match tools {
            Tools::Work => &WORK_DENY,
            Tools::Review => &REVIEW_DENY,
            Tools::Answer => &NO_TOOLS,
            Tools::Pm => &PM_DENY,
            Tools::Retro => &RETRO_DENY,
        };
        assert_eq!(s["permissions"]["deny"], deny_with_trim(own), "{tools:?}");
        // What kelpie's own calls use stays, unless the role denies it itself.
        let deny = s["permissions"]["deny"].to_string();
        for kept in ["SendMessage", "Agent", "Skill", "ToolSearch", "TaskCreate"] {
            if !own.contains(&kept) {
                assert!(!deny.contains(&format!("\"{kept}\"")), "{tools:?} {kept}");
            }
        }
    }
}
