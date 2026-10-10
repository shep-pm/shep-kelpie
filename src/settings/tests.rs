use super::*;

const EXAMPLE: &str = include_str!("../../settings.example.toml");

// The example's first table, before which a test writes a key at the top level
const GIT: &str = "[app.dogs.kelpie.git]\n";

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
    assert_eq!(s.git.checkout, Path::new("/home/me/GitHub/shep"));
    assert_eq!(s.git.remote.unwrap().as_str(), "shep-pm/shep");
    assert_eq!(s.git.merging, Merging::Ask);
    assert_eq!(s.git.issues, Filing::Ask);
    assert!(s.ci.block);
    assert_eq!(s.ci.fix_attempts.cap(), None);
    assert_eq!(s.concurrency.active_items.get(), 1);
    assert_eq!(s.concurrency.pending_rulings, 2);
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
    let text = EXAMPLE.replace("merging = \"ask\"\n", "");
    let err = parse_err(&text);
    assert!(err.contains("missing field `merging`"), "{err}");
    assert!(err.contains("`git`"), "{err}");
}

#[test]
fn the_remote_may_be_left_to_the_checkout_s_origin() {
    let text = EXAMPLE.replace("remote = \"shep-pm/shep\"\n", "");
    assert_eq!(parse(&text).unwrap().git.remote, None);
    let err = parse_err(&EXAMPLE.replace("\"shep-pm/shep\"", "\"shep\""));
    assert!(err.contains("`git.remote = \"shep\"`"), "{err}");
    assert!(err.contains("must be `owner/name`"), "{err}");
}

#[test]
fn the_maintainer_is_a_github_login_and_may_be_left_out() {
    assert_eq!(parse(EXAMPLE).unwrap().git.maintainer, None);
    for (written, login) in [("octocat", "octocat"), ("@Octo-Cat", "Octo-Cat")] {
        let text = EXAMPLE.replace(
            "issues = \"ask\"\n",
            &format!("issues = \"ask\"\nmaintainer = \"{written}\"\n"),
        );
        let maintainer = parse(&text).unwrap().git.maintainer;
        assert_eq!(maintainer.unwrap().as_str(), login);
    }
    for bad in ["", "-octo", "octo cat", "octo/cat"] {
        let text = EXAMPLE.replace(
            "issues = \"ask\"\n",
            &format!("issues = \"ask\"\nmaintainer = \"{bad}\"\n"),
        );
        let err = parse_err(&text);
        assert!(err.contains("must be a GitHub login"), "{bad:?}: {err}");
    }
}

#[test]
fn the_issues_setting_asks_files_or_skips_and_asks_when_absent() {
    for (value, filing) in [
        ("ask", Filing::Ask),
        ("file", Filing::File),
        ("skip", Filing::Skip),
    ] {
        let text = EXAMPLE.replace("issues = \"ask\"", &format!("issues = \"{value}\""));
        assert_eq!(parse(&text).unwrap().git.issues, filing, "{value}");
    }
    let absent = EXAMPLE.replace("issues = \"ask\"\n", "");
    assert_eq!(parse(&absent).unwrap().git.issues, Filing::Ask);
    let err = parse_err(&EXAMPLE.replace("issues = \"ask\"", "issues = \"auto\""));
    assert!(err.contains("unknown variant `auto`"), "{err}");
}

#[test]
fn fix_attempts_is_a_cap_from_0_or_minus_1_for_none_and_none_when_absent() {
    let with =
        |value: &str| EXAMPLE.replace("fix_attempts = -1", &format!("fix_attempts = {value}"));
    assert_eq!(parse(&with("0")).unwrap().ci.fix_attempts.cap(), Some(0));
    assert_eq!(parse(&with("3")).unwrap().ci.fix_attempts.cap(), Some(3));
    assert_eq!(parse(&with("-1")).unwrap().ci.fix_attempts.cap(), None);
    let absent = EXAMPLE.replace("fix_attempts = -1\n", "");
    assert_eq!(parse(&absent).unwrap().ci.fix_attempts.cap(), None);
    let err = parse_err(&with("-2"));
    assert!(err.contains("`ci.fix_attempts = -2`"), "{err}");
    assert!(
        err.contains("must be -1 for no cap, or a count from 0"),
        "{err}"
    );
}

#[test]
fn a_missing_nested_setting_is_named_with_its_table() {
    let text = EXAMPLE.replace("kickoff_hours = 8\n", "");
    let err = parse_err(&text);
    assert!(err.contains("missing field `kickoff_hours`"), "{err}");
    assert!(err.contains("`pacing`"), "{err}");
}

#[test]
fn ci_block_must_be_said_either_way() {
    let err = parse_err(&EXAMPLE.replace("block = true\n", ""));
    assert!(err.contains("missing field `block`"), "{err}");
    let off = parse(&EXAMPLE.replace("block = true", "block = false")).unwrap();
    assert!(!off.ci.block);
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
    let before = EXAMPLE.replace("active_items = 1\n", "");
    assert!(!before.contains("active_items ="));
    assert_eq!(parse(&before).unwrap().concurrency.active_items.get(), 1);
    let text = EXAMPLE.replace("active_items = 1", "active_items = 3");
    assert_eq!(parse(&text).unwrap().concurrency.active_items.get(), 3);
    let err = parse_err(&EXAMPLE.replace("active_items = 1", "active_items = 0"));
    assert!(err.contains("`concurrency.active_items = 0`"), "{err}");
}

#[test]
fn a_table_with_no_concurrency_table_takes_its_defaults() {
    let start = EXAMPLE.find("[app.dogs.kelpie.concurrency]").unwrap();
    let end = EXAMPLE.find("# The agents that build").unwrap();
    let text = format!("{}{}", &EXAMPLE[..start], &EXAMPLE[end..]);
    assert!(!text.contains("pending_rulings"));
    let s = parse(&text).unwrap();
    assert_eq!(s.concurrency.active_items.get(), 1);
    assert_eq!(s.concurrency.pending_rulings, 2);
    let zero = EXAMPLE.replace("pending_rulings = 2", "pending_rulings = 0");
    assert_eq!(parse(&zero).unwrap().concurrency.pending_rulings, 0);
}

// Every top-level key from before the tables, each with its value as it was.
const OLD_KEYS: &str = "[app.dogs.kelpie]\nrepo = \"~/GitHub/shep\"\nforge = \"shep-pm/shep\"\n\
                        merge_authority = \"auto\"\nci = true\nmax_items = 2\nmax_parked = 1\n\
                        private_names = [\"Acme Corp\"]\n";

#[test]
fn every_old_top_level_key_is_refused_at_once_naming_what_replaces_it() {
    assert!(EXAMPLE.contains(GIT), "the example's git table moved");
    // The old `ci` and today's `[ci]` cannot both be in one table.
    let start = EXAMPLE.find("[app.dogs.kelpie.ci]").unwrap();
    let end = EXAMPLE.find("[app.dogs.kelpie.concurrency]").unwrap();
    let text = format!("{}{}", &EXAMPLE[..start], &EXAMPLE[end..]);
    let skills = "\n[app.dogs.kelpie.skills]\nci = { kind = \"none\" }\n";
    let text = text.replace(GIT, &format!("{OLD_KEYS}{GIT}")) + skills;
    let grouped = "because every project setting sits in a table now";
    assert_eq!(
        parse_err(&text),
        format!(
            "the [app.dogs.kelpie] table on shep: these are no longer settings:\n\
             - `repo`, {grouped}: move its value to `git.checkout`\n\
             - `forge`, {grouped}: move its value to `git.remote`, or delete it to read \
             the repo from the checkout's `origin`\n\
             - `merge_authority`, because it set both who merges and whether deferred \
             findings are asked about: move its value to `git.merging`, and set \
             `git.issues`: `ask` asks before filing deferred findings as `ask` did, and \
             `file` files them as `auto` did\n\
             - `ci`, {grouped}: move its value to `block` in a `[ci]` table\n\
             - `max_items`, {grouped}: move its value to `concurrency.active_items`\n\
             - `max_parked`, {grouped}: move its value to `concurrency.pending_rulings`\n\
             - `private_names`, because kelpie no longer checks text for the project's own \
             words: guard a worker's commits and posts with a hook in \
             `worker.guard_hooks`, and delete it\n\
             - `skills.ci`, because the step is named for what it does, apart from the \
             `[ci]` table: move its value to `skills.ci_fix`"
        )
    );
}

#[test]
fn the_ci_table_is_not_taken_for_the_old_ci_key() {
    assert!(parse(EXAMPLE).is_ok());
    let text = EXAMPLE.replace("block = true\n", "block = true\nci = true\n");
    assert!(
        parse_err(&text).contains("`ci.ci = true`: unknown field `ci`"),
        "{}",
        parse_err(&text)
    );
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
    let table = "[app.dogs.kelpie.git]\n";
    let relay = "[app.dogs.kelpie.models.relay]\nmodel = \"m\"\neffort = \"low\"\n";
    let with_model = format!("{EXAMPLE}\n{relay}");
    let err = parse_err(&with_model);
    assert!(
        err.contains("`models.relay` is no longer a setting"),
        "{err}"
    );
    assert!(err.contains("delete it"), "{err}");

    let with_channels = EXAMPLE.replace(
        table,
        &format!("[app.dogs.kelpie]\nruling_channels = [\"webhook\"]\n{table}"),
    );
    let err = parse_err(&with_channels);
    assert!(
        err.contains("`ruling_channels` is no longer a setting"),
        "{err}"
    );
}

#[test]
fn the_planning_calls_settings_are_refused_by_name() {
    let table = "[app.dogs.kelpie.git]\n";
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
    let at_top = EXAMPLE.replace(
        table,
        &format!("[app.dogs.kelpie]\nplanning = {{ enabled = true }}\n{table}"),
    );
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
fn the_review_skill_is_refused_by_name_saying_what_replaces_it() {
    let text = format!("{EXAMPLE}\n[app.dogs.kelpie.skills]\nreview = {{ kind = \"none\" }}\n");
    let err = parse_err(&text);
    assert!(
        err.contains("`skills.review` is no longer a setting, because no step drives"),
        "{err}"
    );
    assert!(
        err.contains("the body of a reviewer's agent file, listed in `agents.reviewers`"),
        "{err}"
    );
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
fn merging_auto_is_accepted() {
    let text = EXAMPLE.replace("merging = \"ask\"", "merging = \"auto\"");
    assert_eq!(parse(&text).unwrap().git.merging, Merging::Auto);
}

#[test]
fn merging_ask_surface_is_refused() {
    let text = EXAMPLE.replace("merging = \"ask\"", "merging = \"ask-surface\"");
    let err = parse_err(&text);
    assert!(err.contains("`git.merging = \"ask-surface\"`"), "{err}");
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
        "[app.dogs.kelpie.git]\n",
        "[app.dogs.kelpie]\npull_request_reviewers = [\"cubic\"]\n[app.dogs.kelpie.git]\n",
    );
    assert_eq!(
        refused(&listed),
        format!(
            "`pull_request_reviewers` is no longer a setting, because {BOT_FILES}: list each \
             bot's file, `coderabbit`, `cubic` or `codex`, in `agents.reviewers` where its \
             read should come, which `shep kelpie add` writes out"
        )
    );
    let generated = EXAMPLE.replace(GIT, &format!("[app.dogs.kelpie]\ngenerated = []\n{GIT}"));
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
            "[app.dogs.kelpie.git]\n",
            "[app.dogs.kelpie]\ngenerated = [\"Cargo.lock\"]\n[app.dogs.kelpie.git]\n",
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
fn a_checkout_path_without_a_tilde_is_kept() {
    let text = EXAMPLE.replace("\"~/GitHub/shep\"", "\"/srv/shep\"");
    assert_eq!(parse(&text).unwrap().git.checkout, Path::new("/srv/shep"));
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
fn a_forge_slug_s_name_is_the_repo_without_its_owner() {
    let slug = ForgeSlug::try_from("shep-pm/koji-website".to_owned()).unwrap();
    assert_eq!(slug.name(), "koji-website");
}
