use super::*;

const EXAMPLE: &str = include_str!("../../settings.example.toml");

const HOME: &str = "/home/me";
const FOLDER: &str = "/home/me/.kelpie/projects/shep";

// A runner's Flockfile entry, read the way its runner reads the table.
fn parse(entry: &str) -> Result<Settings, String> {
    let table = crate::test::project_table(entry);
    Settings::from_table(&table, "shep", Path::new(HOME), Path::new(FOLDER))
        .map_err(|e| e.to_string())
}

fn parse_err(text: &str) -> String {
    parse(text).expect_err("the settings should be refused")
}

// The example with its commented project hook switched on, on `event`.
fn with_hook(event: &str) -> String {
    let hook = "# [[app.dogs.kelpie.worker.guard_hooks]]\n# event = \"PreToolUse\"\n# matcher = \"Bash\"\n# command = \"~/";
    assert!(EXAMPLE.contains(hook), "the example's hook moved");
    let on = format!(
        "[[app.dogs.kelpie.worker.guard_hooks]]\nevent = \"{event}\"\nmatcher = \"Bash\"\ncommand = \"~/"
    );
    EXAMPLE.replace(hook, &on)
}

#[test]
fn the_example_holds_the_first_build_defaults() {
    let s = parse(EXAMPLE).unwrap();
    assert_eq!(s.repo, Path::new("/home/me/.kelpie/repos/shep"));
    assert_eq!(s.forge.as_str(), "shep-pm/shep");
    assert_eq!(s.merge_authority, MergeAuthority::Ask);
    assert!(s.ci);
    assert_eq!(s.max_items.get(), 1);
    let role = |r: &RoleModel| (r.model.as_str().to_owned(), r.effort);
    assert_eq!(
        role(&s.models.worker),
        ("claude-sonnet-5".into(), Effort::Medium)
    );
    assert_eq!(
        role(&s.models.reviewer),
        ("claude-sonnet-5".into(), Effort::Medium)
    );
    assert_eq!(
        role(&s.models.judge),
        ("claude-opus-5-5".into(), Effort::Low)
    );
    assert_eq!(
        role(&s.models.relay),
        ("claude-haiku-4-5-20251001".into(), Effort::Low)
    );
    assert_eq!(s.review.loop_guard.get(), 8);
    assert!(s.coderabbit.enabled);
    assert_eq!(s.coderabbit.divisor.get(), 1000);
    assert!(s.pacing.enabled);
    assert_eq!(s.pacing.kickoff_hours.get(), 8);
    assert_eq!(s.worker.turn_timeout.get(), 60);
    assert!(s.generated.iter().any(|g| g == "Cargo.lock"));
    assert!(
        s.worker.guard_hooks.is_empty(),
        "kelpie's own guard needs none"
    );
    let domains: Vec<&str> = s
        .worker
        .allowed_domains
        .iter()
        .map(NonBlank::as_str)
        .collect();
    assert_eq!(
        domains,
        ["crates.io", "index.crates.io", "static.crates.io"]
    );
}

#[test]
fn a_table_with_no_preview_table_captures_the_root() {
    assert!(
        !EXAMPLE.contains("\n[app.dogs.kelpie.preview]"),
        "the example sets no preview"
    );
    let s = parse(EXAMPLE).unwrap();
    assert_eq!(s.preview.routes.len(), 1);
    assert_eq!(s.preview.routes[0].as_str(), "/");
}

#[test]
fn the_instructions_file_expands_the_home_folder_and_defaults_to_none() {
    assert_eq!(parse(EXAMPLE).unwrap().worker.instructions_file, None);
    let text = EXAMPLE.replace("build_env = {}\n", "instructions_file = \"~/w.md\"\n");
    let file = parse(&text).unwrap().worker.instructions_file;
    assert_eq!(file.as_deref(), Some(Path::new("/home/me/w.md")));
}

#[test]
fn a_missing_setting_is_named() {
    let text = EXAMPLE.replace("forge = \"shep-pm/shep\"\n", "");
    assert!(parse_err(&text).contains("missing field `forge`"));
}

#[test]
fn a_missing_nested_setting_is_named_with_its_table() {
    let text = EXAMPLE.replace("divisor = 1000\n", "");
    let err = parse_err(&text);
    assert!(err.contains("missing field `divisor`"), "{err}");
    assert!(err.contains("`coderabbit`"), "{err}");
}

#[test]
fn ci_must_be_said_either_way() {
    let err = parse_err(&EXAMPLE.replace("ci = true\n", ""));
    assert!(err.contains("missing field `ci`"), "{err}");
    assert!(
        !parse(&EXAMPLE.replace("ci = true", "ci = false"))
            .unwrap()
            .ci
    );
}

#[test]
fn a_table_written_before_the_worker_keys_loads_with_their_defaults() {
    let before = EXAMPLE
        .replace(
            "allowed_domains = [\"crates.io\", \"index.crates.io\", \"static.crates.io\"]\n",
            "",
        )
        .replace("build_env = {}\n", "")
        .replace("turn_timeout = 60\n", "");
    for key in ["turn_timeout =", "allowed_domains =", "build_env ="] {
        assert!(!before.contains(key), "{key}");
    }
    let s = parse(&before).unwrap();
    assert_eq!(s.worker.turn_timeout.get(), 60);
    assert!(s.worker.allowed_domains.is_empty());
    assert!(s.worker.build_env.is_empty());
}

#[test]
fn one_work_item_is_open_at_a_time_when_the_table_does_not_say() {
    let before = EXAMPLE.replace("max_items = 1\n", "");
    assert!(!before.contains("max_items ="));
    assert_eq!(parse(&before).unwrap().max_items.get(), 1);
    let text = EXAMPLE.replace("max_items = 1", "max_items = 3");
    assert_eq!(parse(&text).unwrap().max_items.get(), 3);
    let err = parse_err(&EXAMPLE.replace("max_items = 1", "max_items = 0"));
    assert!(err.contains("`max_items = 0`"), "{err}");
}

#[test]
fn pacing_is_on_when_the_table_does_not_say() {
    let before = EXAMPLE.replace("enabled = true\nkickoff_hours", "kickoff_hours");
    assert!(!before.contains("[app.dogs.kelpie.pacing]\nenabled"));
    assert!(parse(&before).unwrap().pacing.enabled);
}

#[test]
fn pacing_can_be_turned_off() {
    let off = EXAMPLE.replace(
        "[app.dogs.kelpie.pacing]\nenabled = true",
        "[app.dogs.kelpie.pacing]\nenabled = false",
    );
    assert!(!parse(&off).unwrap().pacing.enabled);
}

#[test]
fn a_zero_turn_timeout_is_still_refused() {
    let err = parse_err(&EXAMPLE.replace("turn_timeout = 60", "turn_timeout = 0"));
    assert!(err.contains("`worker.turn_timeout = 0`"), "{err}");
}

#[test]
fn a_misspelt_setting_is_named() {
    let text = EXAMPLE.replace("loop_guard", "loop_gaurd");
    let err = parse_err(&text);
    assert!(
        err.contains("`review.loop_gaurd = 8`: unknown field `loop_gaurd`"),
        "{err}"
    );
}

#[test]
fn merge_authority_auto_is_accepted() {
    let text = EXAMPLE.replace("merge_authority = \"ask\"", "merge_authority = \"auto\"");
    assert_eq!(parse(&text).unwrap().merge_authority, MergeAuthority::Auto);
}

#[test]
fn merge_authority_ask_surface_is_refused() {
    let text = EXAMPLE.replace(
        "merge_authority = \"ask\"",
        "merge_authority = \"ask-surface\"",
    );
    let err = parse_err(&text);
    assert!(err.contains("merge_authority"), "{err}");
    assert!(err.contains("unknown variant `ask-surface`"), "{err}");
}

#[test]
fn a_zero_loop_guard_is_refused() {
    let text = EXAMPLE.replace("loop_guard = 8", "loop_guard = 0");
    let err = parse_err(&text);
    assert_eq!(
        err,
        "the [app.dogs.kelpie] table on shep: `review.loop_guard = 0`: \
         invalid value: integer `0`, expected a nonzero u32"
    );
}

#[test]
fn the_review_budget_is_unset_until_a_table_sets_it() {
    let s = parse(EXAMPLE).unwrap();
    assert_eq!((s.review.local_rounds, s.coderabbit.rounds), (None, None));
    let text = EXAMPLE
        .replace("loop_guard = 8\n", "loop_guard = 8\nlocal_rounds = 2\n")
        .replace("divisor = 1000\n", "divisor = 1000\nrounds = 1\n");
    let s = parse(&text).unwrap();
    assert_eq!(s.review.local_rounds.map(NonZeroU32::get), Some(2));
    assert_eq!(s.coderabbit.rounds.map(NonZeroU32::get), Some(1));
    let err = parse_err(&EXAMPLE.replace("divisor = 1000\n", "divisor = 1000\nrounds = 0\n"));
    assert!(err.contains("`coderabbit.rounds = 0`"), "{err}");
    let err = parse_err(&EXAMPLE.replace("loop_guard = 8\n", "loop_guard = 8\nlocal_rounds = 0\n"));
    assert!(err.contains("`review.local_rounds = 0`"), "{err}");
}

#[test]
fn kickoff_hours_outside_a_day_are_refused() {
    for hours in ["0", "25", "300", "-1"] {
        let line = format!("kickoff_hours = {hours}");
        let err = parse_err(&EXAMPLE.replace("kickoff_hours = 8", &line));
        assert!(err.contains(&format!("`pacing.{line}`")), "{err}");
        assert!(err.contains("must be from 1 to 24"), "{err}");
    }
}

#[test]
fn every_effort_parses_from_what_claude_takes() {
    for effort in [
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::Xhigh,
        Effort::Max,
    ] {
        assert_eq!(Effort::parse(effort.as_str()), Some(effort));
    }
    assert_eq!(Effort::parse("Medium"), None);
}

#[test]
fn an_unknown_effort_is_refused() {
    let text = EXAMPLE.replacen("effort = \"medium\"", "effort = \"huge\"", 1);
    assert!(parse_err(&text).contains("unknown variant `huge`"));
}

#[test]
fn a_forge_slug_needs_an_owner_and_a_name() {
    for slug in [
        "shep",
        "/shep",
        "shep-pm/",
        "shep-pm/shep/x",
        "shep pm/shep",
    ] {
        let text = EXAMPLE.replace("\"shep-pm/shep\"", &format!("{slug:?}"));
        assert!(parse_err(&text).contains("must be `owner/name`"), "{slug}");
    }
}

#[test]
fn a_blank_model_is_refused() {
    let text = EXAMPLE.replacen("model = \"claude-sonnet-5\"", "model = \" \"", 1);
    assert!(parse_err(&text).contains("must not be blank"));
}

#[test]
fn build_env_names_folders_inside_the_build_folder() {
    let text = EXAMPLE.replace(
        "build_env = {}",
        r#"build_env = { BUN_INSTALL_CACHE_DIR = "bun/cache" }"#,
    );
    let s = parse(&text).unwrap();
    let [(name, dir)] = s
        .worker
        .build_env
        .iter()
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    assert_eq!(
        (name.as_str(), dir.as_path()),
        ("BUN_INSTALL_CACHE_DIR", Path::new("bun/cache"))
    );
    for bad in [
        "\"\"",
        "\"/tmp/bun\"",
        "\"../bun\"",
        "\"bun/../..\"",
        "\"./bun\"",
    ] {
        let line = format!("build_env = {{ BUN = {bad} }}");
        let err = parse_err(&EXAMPLE.replace("build_env = {}", &line));
        assert!(err.contains("inside the build folder"), "{bad}: {err}");
    }
    for bad in ["bun", "1BUN", "BUN-DIR"] {
        let line = format!("build_env = {{ {bad} = \"bun\" }}");
        let err = parse_err(&EXAMPLE.replace("build_env = {}", &line));
        assert!(err.contains("environment variable name"), "{bad}: {err}");
    }
}

#[test]
fn a_repo_path_without_a_tilde_is_kept() {
    let text = EXAMPLE.replace("\"~/.kelpie/repos/shep\"", "\"/srv/shep\"");
    assert_eq!(parse(&text).unwrap().repo, Path::new("/srv/shep"));
}

#[test]
fn an_unreadable_file_names_its_path() {
    let err = Settings::load(Path::new("/nonexistent/settings.toml"), Path::new("/"));
    let err = err.unwrap_err();
    assert_eq!(
        err,
        SettingsError::Read {
            path: "/nonexistent/settings.toml".into(),
            kind: io::ErrorKind::NotFound,
        }
    );
    assert!(err.to_string().contains("/nonexistent/settings.toml"));
}

#[test]
fn a_projects_own_guard_hooks_are_read() {
    let [hook] = parse(&with_hook("PostToolUse"))
        .unwrap()
        .worker
        .guard_hooks
        .try_into()
        .unwrap();
    assert_eq!(hook.event, HookEvent::PostToolUse);
    assert_eq!(
        hook.matcher.map(|m| m.as_str().to_owned()),
        Some("Bash".into())
    );
    assert_eq!(hook.command.as_str(), "~/.kelpie/hooks/lint-guard");
}

#[test]
fn a_table_names_a_malformed_key_in_a_list_of_tables() {
    let err = parse_err(&with_hook("Stop"));
    assert!(
        err.contains("`worker.guard_hooks.event = \"Stop\"`"),
        "{err}"
    );
}

#[test]
fn a_relative_instructions_file_is_taken_from_the_project_folder() {
    let text = EXAMPLE.replace("build_env = {}\n", "instructions_file = \"w.md\"\n");
    let file = parse(&text).unwrap().worker.instructions_file;
    assert_eq!(
        file.as_deref(),
        Some(Path::new(FOLDER).join("w.md").as_path())
    );
}

#[test]
fn an_old_settings_file_loads_the_same_settings_as_its_table() {
    let table = crate::test::project_table(EXAMPLE);
    let folder = tempfile::tempdir().unwrap();
    let file = folder.path().join("settings.toml");
    std::fs::write(&file, toml::to_string(&table).unwrap()).unwrap();
    let home = Path::new(HOME);
    let from_file = Settings::load(&file, home).unwrap();
    let from_table = Settings::from_table(&table, "shep", home, folder.path()).unwrap();
    assert_eq!(from_file, from_table);
}

#[test]
fn a_file_moves_into_an_equal_table() {
    let table = crate::test::project_table(EXAMPLE);
    let old_file = toml::to_string(&table).unwrap();
    assert_eq!(table_of(&old_file), Ok(table));
}

#[test]
fn a_table_s_local_command_expands_the_home_folder_and_takes_the_project_folder() {
    let command = |path: &str| {
        let line = format!("command = \"{path}\"");
        let text = EXAMPLE.replace("command = \"~/.claude/scripts/qwen-review.sh\"", &line);
        match parse(&text).unwrap().review.local {
            LocalRound::Command(local) => local.command,
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(
        command("~/.claude/scripts/qwen-review.sh"),
        Path::new("/home/me/.claude/scripts/qwen-review.sh")
    );
    assert_eq!(command("review.sh"), Path::new(FOLDER).join("review.sh"));
    assert_eq!(command("/opt/review.sh"), Path::new("/opt/review.sh"));
}
