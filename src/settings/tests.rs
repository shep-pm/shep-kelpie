use super::*;

const EXAMPLE: &str = include_str!("../../settings.example.toml");

const HOME: &str = "/home/me";
const FOLDER: &str = "/home/me/.shep/kelpie/shep";

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
    assert_eq!(s.repo, Path::new("/home/me/GitHub/shep"));
    assert_eq!(s.forge.as_str(), "shep-pm/shep");
    assert_eq!(s.merge_authority, MergeAuthority::Ask);
    assert!(s.ci);
    assert_eq!(s.max_items.get(), 1);
    let implementers: Vec<&str> = s.agents.implementers.iter().map(|n| n.as_str()).collect();
    assert_eq!(implementers, ["sonnet-high"]);
    assert_eq!(
        s.agents.reviewers, None,
        "kelpie's default list, with no bot"
    );
    assert!(s.pacing.enabled);
    assert_eq!(s.pacing.kickoff_hours.get(), 8);
    assert_eq!(s.worker.turn_timeout.get(), 60);
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
fn the_previews_settings_are_refused_by_name() {
    for block in [
        "[app.dogs.kelpie.preview]\nenabled = true\nroutes = [\"/\"]\n",
        "[app.dogs.kelpie.preview]\n",
    ] {
        let err = parse_err(&format!("{EXAMPLE}\n{block}"));
        assert!(
            err.contains(
                "`preview` is no longer a setting, because shots and the preview are parked: \
                 delete it"
            ),
            "{err}"
        );
    }
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
    let text = EXAMPLE.replace("kickoff_hours = 8\n", "");
    let err = parse_err(&text);
    assert!(err.contains("missing field `kickoff_hours`"), "{err}");
    assert!(err.contains("`pacing`"), "{err}");
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
fn the_relays_settings_are_refused_by_name() {
    let table = "[app.dogs.kelpie]\n";
    let relay = "[app.dogs.kelpie.models.relay]\nmodel = \"m\"\neffort = \"low\"\n";
    let with_model = format!("{EXAMPLE}\n{relay}");
    let err = parse_err(&with_model);
    assert!(
        err.contains("`models.relay` is no longer a setting"),
        "{err}"
    );
    assert!(err.contains("delete it"), "{err}");

    let with_channels =
        EXAMPLE.replace(table, &format!("{table}ruling_channels = [\"webhook\"]\n"));
    let err = parse_err(&with_channels);
    assert!(
        err.contains("`ruling_channels` is no longer a setting"),
        "{err}"
    );
}

#[test]
fn the_planning_calls_settings_are_refused_by_name() {
    let table = "[app.dogs.kelpie]\n";
    let removed = [
        ("planning", "[app.dogs.kelpie.planning]\nenabled = false\n"),
        (
            "models.planner",
            "[app.dogs.kelpie.models.planner]\nmodel = \"m\"\neffort = \"low\"\n",
        ),
        (
            "skills.planning",
            "[app.dogs.kelpie.skills]\nplanning = { kind = \"none\" }\n",
        ),
    ];
    let removed = removed
        .map(|(key, block)| (key, format!("{EXAMPLE}\n{block}")))
        .into_iter()
        .chain([("agents.planner", with_agents("planner = \"opus\"\n"))]);
    for (key, text) in removed {
        let err = parse_err(&text);
        assert!(
            err.contains(&format!(
                "`{key}` is no longer a setting, because the planning call is gone: delete it"
            )),
            "{key}: {err}"
        );
    }
    let at_top = EXAMPLE.replace(table, &format!("{table}planning = {{ enabled = true }}\n"));
    assert!(parse_err(&at_top).contains("`planning` is no longer"));
}

#[test]
fn the_review_loops_settings_are_refused_by_name() {
    let review = |key: &str| format!("{EXAMPLE}\n[app.dogs.kelpie.review]\n{key}\n");
    let removed = [
        (
            "models.judge",
            format!("{EXAMPLE}\n[app.dogs.kelpie.models.judge]\nmodel = \"m\"\neffort = \"low\"\n"),
        ),
        ("agents.judge", with_agents("judge = \"opus\"\n")),
        ("review.loop_guard", review("loop_guard = 8")),
        ("review.local_rounds", review("local_rounds = 1")),
    ];
    for (key, text) in removed {
        let err = parse_err(&text);
        assert!(
            err.contains(&format!(
                "`{key}` is no longer a setting, because the review loop and its judge \
                 are gone: delete it"
            )),
            "{key}: {err}"
        );
    }
}

#[test]
fn the_whole_issue_checks_settings_are_refused_by_name() {
    let removed = [
        (
            "models.auditor",
            format!(
                "{EXAMPLE}\n[app.dogs.kelpie.models.auditor]\nmodel = \"m\"\neffort = \"low\"\n"
            ),
        ),
        ("agents.auditor", with_agents("auditor = \"opus\"\n")),
    ];
    for (key, text) in removed {
        let err = parse_err(&text);
        assert!(
            err.contains(&format!(
                "`{key}` is no longer a setting, because the whole-issue check is gone: \
                 delete it"
            )),
            "{key}: {err}"
        );
    }
}

// The example with `keys` added to its `[agents]` table.
fn with_agents(keys: &str) -> String {
    let table = "[app.dogs.kelpie.agents]\n";
    assert!(EXAMPLE.contains(table), "the example's agents table moved");
    EXAMPLE.replace(table, &format!("{table}{keys}"))
}

#[test]
fn the_worker_settings_agent_files_replace_are_refused_saying_what_replaces_each() {
    let removed = [
        (
            "agents.worker",
            with_agents("worker = \"opus-high\"\n"),
            "list the agents that build in `agents.implementers`, the first being the default",
        ),
        (
            "models.worker",
            format!(
                "{EXAMPLE}\n[app.dogs.kelpie.models.worker]\nmodel = \"m\"\neffort = \"low\"\n"
            ),
            "name the agent that builds in `agents.implementers`, such as `sonnet-high`, \
             which is Sonnet 5.5 at high",
        ),
    ];
    for (key, text, fix) in removed {
        assert_eq!(
            parse_err(&text),
            format!(
                "the [app.dogs.kelpie] table on shep: `{key}` is no longer a setting, \
                 because agents are files in kelpie's home's `agents` folder: {fix}"
            )
        );
    }
    let labels = format!("{EXAMPLE}\n[app.dogs.kelpie.models.labels]\nopus = \"claude-opus-6\"\n");
    assert_eq!(
        parse_err(&labels),
        "the [app.dogs.kelpie] table on shep: `models.labels` is no longer a setting, \
         because an `agent:<name>` label names its agent file, which holds the model id: \
         list each agent a label may name in `agents.implementers`"
    );
}

#[test]
fn a_misspelt_setting_is_named() {
    let text = EXAMPLE.replace("turn_timeout = 60", "turn_timout = 60");
    let err = parse_err(&text);
    assert!(
        err.contains("`worker.turn_timout = 60`: unknown field `turn_timout`"),
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
fn a_zero_turn_timeout_is_refused() {
    let text = EXAMPLE.replace("turn_timeout = 60", "turn_timeout = 0");
    let err = parse_err(&text);
    assert_eq!(
        err,
        "the [app.dogs.kelpie] table on shep: `worker.turn_timeout = 0`: \
         invalid value: integer `0`, expected a nonzero u32"
    );
}

const BOT_FILES: &str = "review bots are reviewer agent files on the `bot` harness, listed \
                         in `agents.reviewers` and run once a pass in their place";

#[test]
fn the_review_bot_settings_bot_files_replace_are_refused_saying_what_replaces_each() {
    let gate = |keys: &str| format!("{EXAMPLE}\n[app.dogs.kelpie.coderabbit]\n{keys}\n");
    let refused = |text: &str| parse_err(text).replace("the [app.dogs.kelpie] table on shep: ", "");
    assert_eq!(
        refused(&gate("enabled = true")),
        format!(
            "`coderabbit.enabled` is no longer a setting, because {BOT_FILES}: for `true`, \
             list `coderabbit` where its read should come, as in \
             `agents.reviewers = [\"defect-hunter\", \"coderabbit\"]`, and for `false` leave \
             it out; then delete the key"
        )
    );
    assert_eq!(
        refused(&gate("rounds = 1")),
        format!(
            "`coderabbit.rounds` is no longer a setting, because {BOT_FILES}: set `rounds` in the \
             bot's file, such as `agents/coderabbit.md`, where it is the most reads that bot \
             makes of a work item's pull request"
        )
    );
    assert_eq!(
        refused(&gate("divisor = 1000")),
        "`coderabbit.divisor` is no longer a setting, because a review bot reads once a \
         pass, so no round cap is counted: delete it"
    );
    assert_eq!(
        refused(&gate("")),
        format!("`coderabbit` is no longer a setting, because {BOT_FILES}: delete it")
    );
    let listed = EXAMPLE.replace(
        "max_items = 1\n",
        "max_items = 1\npull_request_reviewers = [\"cubic\"]\n",
    );
    assert_eq!(
        refused(&listed),
        format!(
            "`pull_request_reviewers` is no longer a setting, because {BOT_FILES}: list each \
             bot's file, `coderabbit`, `cubic` or `codex`, in `agents.reviewers` where its \
             read should come, which `shep kelpie add` writes out"
        )
    );
    let generated = EXAMPLE.replace("max_items = 1\n", "max_items = 1\ngenerated = []\n");
    assert_eq!(
        refused(&generated),
        "`generated` is no longer a setting, because it only kept files out of the review \
         bot cap's changed-line count, and a review bot now reads once a pass: delete it"
    );
}

// A live table from before bot files, its own review list set: every key
// it must drop is named at once, and the fix lists its own reviewers.
#[test]
fn a_table_carrying_several_removed_keys_names_them_all_at_once() {
    let text = EXAMPLE
        .replace(
            "max_items = 1\n",
            "max_items = 1\ngenerated = [\"Cargo.lock\"]\n",
        )
        .replace(
            "# reviewers = [\"qwen\", \"defect-hunter\"]",
            "reviewers = [\"qwen\", \"defect-hunter\"]",
        );
    let text = format!(
        "{text}\n[app.dogs.kelpie.coderabbit]\nenabled = true\ndivisor = 1000\nrounds = 1\n"
    );
    let err = parse_err(&text);
    let lines: Vec<&str> = err.lines().collect();
    assert_eq!(
        lines[0],
        "the [app.dogs.kelpie] table on shep: these are no longer settings:"
    );
    let named: Vec<&str> = lines[1..]
        .iter()
        .map(|l| l.split('`').nth(1).unwrap())
        .collect();
    assert_eq!(
        named,
        [
            "coderabbit.enabled",
            "coderabbit.rounds",
            "coderabbit.divisor",
            "generated"
        ],
        "`[coderabbit]` is named by its keys"
    );
    assert!(
        err.contains("`agents.reviewers = [\"qwen\", \"defect-hunter\", \"coderabbit\"]`"),
        "{err}"
    );
}

#[test]
fn the_review_settings_reviewer_files_replace_are_refused_saying_what_replaces_each() {
    let table = |name: &str, keys: &str| format!("{EXAMPLE}\n[app.dogs.kelpie.{name}]\n{keys}\n");
    let model = "model = \"m\"\neffort = \"low\"";
    let removed = [
        (
            "models.reviewer",
            table("models.reviewer", model),
            "write the model and effort as a reviewer's agent file, or list kelpie's \
             `defect-hunter`, Opus 5.5 at high, in `agents.reviewers`",
        ),
        (
            "models.deep_reviewer",
            table("models.deep_reviewer", model),
            "list `defect-hunter` in `agents.reviewers`: it is the deep round, and its \
             file holds the model and effort",
        ),
        (
            "agents.reviewer",
            with_agents("reviewer = \"opus-high\"\n"),
            "list the reviewer's agent file in `agents.reviewers`",
        ),
        (
            "agents.deep_reviewer",
            with_agents("deep_reviewer = \"opus-high\"\n"),
            "list `defect-hunter`, or a reviewer file with `second_look: true`, in \
             `agents.reviewers`",
        ),
        (
            "review.reviewers",
            table("review", "reviewers = [\"deep\"]"),
            "list them in `agents.reviewers`: `deep` is now `defect-hunter`, and \
             `claude` and each of kelpie's `[local_reviewers]` an agent file of its own",
        ),
        (
            "review.local",
            table("review.local", "kind = \"command\"\ncommand = \"~/q.sh\""),
            "write the round as an agent file with `role: reviewer`, its `kind` as \
             `harness` and `gpu_lease = true` as `lease: gpu`, and list it in \
             `agents.reviewers`; `kind = \"off\"` is `agents.reviewers = [\"defect-hunter\"]`",
        ),
        ("review", table("review", ""), "delete it"),
    ];
    for (key, text, fix) in removed {
        assert_eq!(
            parse_err(&text),
            format!(
                "the [app.dogs.kelpie] table on shep: `{key}` is no longer a setting, \
                 because reviewers are agent files listed in `agents.reviewers`: {fix}"
            )
        );
    }
    assert_eq!(
        parse_err(&table("models", "")),
        "the [app.dogs.kelpie] table on shep: `models` is no longer a setting, because \
         agents are files in kelpie's home's `agents` folder, which hold each model and \
         effort: delete it"
    );
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
fn private_names_are_read_and_default_to_none() {
    assert_eq!(parse(EXAMPLE).unwrap().private_names, []);
    let text = EXAMPLE.replace(
        "max_items = 1\n",
        "max_items = 1\nprivate_names = [\"Acme Corp\", \"zeta\"]\n",
    );
    let names: Vec<_> = parse(&text).unwrap().private_names;
    let names: Vec<&str> = names.iter().map(NonBlank::as_str).collect();
    assert_eq!(names, ["Acme Corp", "zeta"]);
    let blank = EXAMPLE.replace(
        "max_items = 1\n",
        "max_items = 1\nprivate_names = [\" \"]\n",
    );
    assert!(parse_err(&blank).contains("blank"), "{}", parse_err(&blank));
}

#[test]
fn a_repo_path_without_a_tilde_is_kept() {
    let text = EXAMPLE.replace("\"~/GitHub/shep\"", "\"/srv/shep\"");
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
    assert_eq!(hook.command.as_str(), "~/bin/lint-guard");
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
fn a_forge_slug_s_name_is_the_repo_without_its_owner() {
    let slug = ForgeSlug::try_from("shep-pm/koji-website".to_owned()).unwrap();
    assert_eq!(slug.name(), "koji-website");
}
