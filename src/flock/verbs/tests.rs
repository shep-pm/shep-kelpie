use super::*;
use crate::ports::{ForgeError, Notification};
use crate::settings::ForgeSlug;
use crate::test::FakeClock;

fn words(line: &str) -> Vec<String> {
    line.split(' ').map(str::to_owned).collect()
}

#[test]
fn a_base_is_taken_from_anywhere_in_the_line() {
    for line in ["--base next koji", "koji --base next", "--base=next koji"] {
        let (base, rest) = split_base(&words(line)).unwrap();
        assert_eq!(base.as_deref(), Some("next"), "{line}");
        assert_eq!(rest, ["koji"], "{line}");
    }
    assert_eq!(split_base(&words("koji")).unwrap(), (None, words("koji")));
}

#[test]
fn a_base_after_two_dashes_is_a_word() {
    let (base, rest) = split_base(&words("koji -- --base next")).unwrap();
    assert_eq!(base, None);
    assert_eq!(rest, words("koji -- --base next"));
}

#[test]
fn a_base_named_twice_or_with_no_branch_is_refused() {
    let twice = split_base(&words("--base a --base=b")).unwrap_err();
    assert!(twice.contains("once"), "{twice}");
    let bare = split_base(&words("koji --base")).unwrap_err();
    assert!(bare.contains("takes a branch"), "{bare}");
}

#[test]
fn the_project_goes_anywhere_in_the_line_before_a_double_dash() {
    let koji = Named::parse("koji").ok();
    for line in [
        "-p koji 3 7",
        "3 7 -p koji",
        "3 --project koji 7",
        "3 7 --project=koji",
    ] {
        let (named, rest) = split_project(&words(line), None).unwrap();
        assert_eq!(named, koji, "{line}");
        assert_eq!(rest, words("3 7"), "{line}");
    }
    let (named, rest) = split_project(&words("14 no -- drop the -p flag"), None).unwrap();
    assert_eq!(named, None);
    assert_eq!(rest, words("14 no drop the -p flag"));
    for refused in ["-p", "-p koji -p rotom", "-p ../koji"] {
        assert!(split_project(&words(refused), None).is_err(), "{refused}");
    }
}

#[test]
fn a_project_with_a_glob_character_is_a_glob_wherever_the_project_goes() {
    for line in ["-p shep* 3", "3 --project=shep*", "3 -p [ks]*"] {
        let (named, rest) = split_project(&words(line), None).unwrap();
        assert!(matches!(named, Some(Named::Glob(_))), "{line}");
        assert_eq!(rest, words("3"), "{line}");
    }
}

#[test]
fn a_glob_is_refused_by_every_verb_but_set_and_get_naming_those_two() {
    let koji = Named::parse("k*").unwrap();
    for command in VERBS {
        let split = Named::split(Some(koji.clone()), command);
        match command {
            "set" | "get" => assert!(matches!(split, Ok((None, Some(_)))), "{command}"),
            _ => {
                let refused = split.unwrap_err();
                assert!(refused.contains("`set` and `get`"), "{command}: {refused}");
            }
        }
    }
}

#[test]
fn a_ruling_s_answer_keeps_a_p_as_its_own_words() {
    let koji = Named::parse("koji").ok();
    for line in ["-p koji 14 yes", "14 -p koji yes", "14 --project=koji yes"] {
        let (named, rest) = split_project(&words(line), Some(2)).unwrap();
        assert_eq!(named, koji, "{line}");
        assert_eq!(rest, words("14 yes"), "{line}");
    }
    let line = words("14 no pass -p koji through");
    let (named, rest) = split_project(&line, Some(2)).unwrap();
    assert_eq!(named, None);
    assert_eq!(rest, line);
}

#[test]
fn a_verb_after_the_project_goes_first() {
    let moved = |line| verb_first(words(line));
    assert_eq!(moved("-p koji rule 14 yes"), words("rule -p koji 14 yes"));
    assert_eq!(moved("--project=koji gate"), words("gate --project=koji"));
    assert_eq!(moved("-p koji timings 5"), words("timings -p koji 5"));
    assert_eq!(
        moved("-p koji issue check --draft a.md"),
        words("issue -p koji check --draft a.md")
    );
    assert_eq!(moved("-p koji attach 7"), words("attach -p koji 7"));
    assert_eq!(moved("-p koji tell hold #4"), words("tell -p koji hold #4"));
    assert_eq!(moved("rule -p koji 14 yes"), words("rule -p koji 14 yes"));
    assert_eq!(moved("-p koji runner x"), words("-p koji runner x"));
    assert_eq!(moved("-p"), words("-p"));
}

// `status` against a fake shepherd holding the runner `koji`.
async fn status(args: &str) -> Result<Vec<String>, String> {
    verb("status", args, None).await
}

// `command args` against a fake shepherd holding the runner `koji`, which
// answers every trigger with `says` when that is given.
async fn verb(command: &str, args: &str, says: Option<&str>) -> Result<Vec<String>, String> {
    // Only a bare `status` needs no project; the rest find theirs from the folder.
    let project = if command == "status" { "" } else { "-p koji " };
    verb_line(command, &format!("{project}{args}"), says).await
}

// `command line` as typed, against the same fake shepherd.
async fn verb_line(command: &str, line: &str, says: Option<&str>) -> Result<Vec<String>, String> {
    verb_line_on(&koji_shepherd(says), command, line).await
}

fn koji_shepherd(says: Option<&str>) -> crate::test::FakeShepherd {
    let shepherd = crate::test::FakeShepherd::new();
    let launch = Launch {
        kelpie: "/opt/kelpie".into(),
        shep_home: shepherd.home().to_owned(),
        kelpie_home: None,
    };
    let table = crate::test::project_table(include_str!("../../../settings.example.toml"));
    let name = ProjectName::try_from("koji").unwrap();
    shepherd.holds(launch.runner(&name, table), true);
    shepherd.holds_dog("kelpie", true);
    crate::agents::write_bound_stubs(
        &crate::home::kelpie_home_of(shepherd.home()).join("agents"),
        "harness: claude-code\nmodel: claude-opus-5-5\neffort: medium\n",
    );
    if let Some(says) = says {
        shepherd.says("koji", &[says]);
    }
    shepherd
}

async fn verb_line_on(
    shepherd: &crate::test::FakeShepherd,
    command: &str,
    line: &str,
) -> Result<Vec<String>, String> {
    let args: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
    let dial = shepherd.dial();
    let ask = run(&*dial, shepherd.home(), command, &args);
    tokio::time::timeout(std::time::Duration::from_secs(40), ask)
        .await
        .unwrap_or_else(|_| panic!("{command} neither ended nor failed in time"))
}

#[tokio::test]
async fn issue_check_here_is_the_usage_and_a_request_is_refused_naming_the_skill() {
    let said = verb_line("issue", "-p koji check 7", None).await;
    assert_eq!(said, Err(USAGE.to_owned()));

    for line in ["-p koji add a thing", "-p koji --interactive add a thing"] {
        let refused = verb_line("issue", line, None).await.unwrap_err();
        assert!(refused.contains("went with the issue writer"), "{refused}");
        assert!(refused.contains("`issue` skill"), "{refused}");
        assert!(refused.contains("shep kelpie issue check"), "{refused}");
    }
}

#[tokio::test]
async fn help_prints_the_usage_and_registers_nothing() {
    let shepherd = koji_shepherd(None);
    let usage: Vec<String> = USAGE.lines().map(str::to_owned).collect();
    for (command, line) in [
        ("add", "--help"),
        ("add", "-h koji"),
        ("status", "--help"),
        ("get", "--help"),
    ] {
        let printed = verb_line_on(&shepherd, command, line).await;
        assert_eq!(printed, Ok(usage.clone()), "{command} {line}");
    }
    assert!(shepherd.sheep("--help").is_none());
}

// What `command args` prints plain, then with `--json`, for a runner that
// answers `says`.
async fn both(command: &str, args: &str, says: &str) -> (Vec<String>, Vec<String>) {
    let plain = verb(command, args, Some(says)).await.unwrap();
    let json = verb(command, &format!("{args} --json"), Some(says)).await;
    (plain, json.unwrap())
}

#[tokio::test]
async fn timings_prints_the_table_plain_and_the_object_with_json() {
    let object = r#"{"seconds":{"worker":90},"table":"phase   seconds\nworker  90"}"#;
    let (plain, json) = both("timings", "20", object).await;
    assert_eq!(plain, ["phase   seconds", "worker  90"]);
    assert_eq!(json, [object]);
}

#[tokio::test]
async fn board_prints_the_numbered_list_with_titles_and_priorities() {
    let list = r#"{"project":"koji","board":[
        {"position":1,"issue":"284","title":"Add the list","priority":"P0","blocked":false,"held":false},
        {"position":2,"issue":"282","title":"Order it","priority":null,"blocked":true,"held":false},
        {"position":3,"issue":"k12","title":"Show it","priority":"P2","blocked":false,"held":true}]}"#;
    let (plain, json) = both("board", "", list).await;
    assert_eq!(
        plain,
        [
            "1. #284 Add the list (P0)",
            "2. #282 Order it (no priority, blocked)",
            "3. k12 Show it (P2, held)",
        ]
    );
    assert_eq!(json, [list]);
    let empty = r#"{"project":"koji","board":[]}"#;
    let (plain, _) = both("board", "", empty).await;
    assert_eq!(plain, ["koji: no issue is ready on the board"]);
}

#[tokio::test]
async fn board_top_and_board_move_print_the_list_as_it_now_stands() {
    let list = r#"{"project":"koji","board":[
        {"position":1,"issue":"282","title":"Order it","priority":null,"blocked":false,"held":false},
        {"position":2,"issue":"284","title":"Add the list","priority":"P0","blocked":false,"held":false}]}"#;
    for args in ["top 282", "move 282 1"] {
        let (plain, _) = both("board", args, list).await;
        assert_eq!(
            plain,
            [
                "1. #282 Order it (no priority)",
                "2. #284 Add the list (P0)"
            ],
            "board {args}"
        );
    }
    for args in ["top", "move 282", "top x", "move 282 x", "up 282"] {
        let err = verb("board", args, Some(list)).await.unwrap_err();
        assert!(err.starts_with("usage: shep kelpie add"), "{args}: {err}");
    }
}

#[tokio::test]
async fn drain_names_the_calls_still_running_and_undrain_says_calls_start() {
    let running =
        r#"{"draining":{"calls":[{"issue":"7","role":"worker"},{"role":"pm"}],"ceiling":3600}}"#;
    let (plain, json) = both("drain", "", running).await;
    assert_eq!(
        plain,
        ["koji is draining: no new call starts; still running: worker #7, pm"]
    );
    assert_eq!(json, [running]);
    let idle = r#"{"draining":{"calls":[],"ceiling":3600}}"#;
    let (plain, _) = both("drain", "", idle).await;
    assert_eq!(
        plain,
        ["koji is draining: no new call starts, and none is running"]
    );
    let status = r#"{"project":"koji","draining":null}"#;
    let (plain, json) = both("undrain", "", status).await;
    assert_eq!(plain, ["koji is not draining: calls start again"]);
    assert_eq!(json, [status]);
}

#[tokio::test]
async fn drain_and_undrain_print_an_answer_that_contradicts_them_as_it_came() {
    let not_draining = r#"{"project":"koji","draining":null}"#;
    let (plain, _) = both("drain", "", not_draining).await;
    assert_eq!(plain, [not_draining]);
    let draining = r#"{"draining":{"calls":[],"ceiling":3600}}"#;
    let (plain, _) = both("undrain", "", draining).await;
    assert_eq!(plain, [draining]);
}

#[tokio::test]
async fn gate_drop_and_add_say_what_they_did_to_the_work_item() {
    let status = r#"{"project":"koji","work_item":null}"#;
    for (command, args, line) in [
        (
            "gate",
            "",
            "koji: sent the open work item to the review gate",
        ),
        ("gate", "7", "koji: sent #7 to the review gate"),
        ("drop", "", "koji: dropped the open work item"),
        ("drop", "7", "koji: dropped #7"),
        ("add", "7", "koji: queued a work item for #7"),
        ("gate", "k12", "koji: sent k12 to the review gate"),
        ("drop", "k12", "koji: dropped k12"),
        ("add", "k12", "koji: queued a work item for k12"),
    ] {
        let (plain, json) = both(command, args, status).await;
        assert_eq!(plain, [line], "{command} {args}");
        assert_eq!(json, [status], "{command} {args}");
    }
}

#[tokio::test]
async fn tell_and_finish_print_their_line_plain_and_the_answer_with_json() {
    let told = r#"{"project":"koji"}"#;
    let (plain, json) = both("tell", "hold #4", told).await;
    assert_eq!(
        plain,
        ["told koji's project manager, which reads it on its next wake"]
    );
    assert_eq!(json, [told]);
    let finishing = r#"{"project":"koji","run":"finishing","work_items":[]}"#;
    let (plain, json) = both("finish", "", finishing).await;
    assert_eq!(
        plain,
        ["koji has no work item open, so its runner stops now"]
    );
    assert_eq!(json, [finishing]);
}

#[tokio::test]
async fn finish_all_is_refused_with_a_project() {
    for line in [
        "--all -p koji",
        "--all --project=koji",
        "--all koji",
        "koji --all",
    ] {
        let err = verb_line("finish", line, None).await.unwrap_err();
        assert!(err.starts_with("name the project once"), "{line}: {err}");
    }
}

#[tokio::test]
async fn finish_all_finishes_every_project_and_prints_its_answer_with_json() {
    let finishing = r#"{"project":"koji","run":"finishing","work_items":[]}"#;
    let plain = verb_line("finish", "--all", Some(finishing)).await;
    assert_eq!(
        plain.unwrap(),
        ["koji has no work item open, so its runner stops now"]
    );
    let json = verb_line("finish", "--json --all", Some(finishing)).await;
    assert_eq!(json.unwrap(), [format!("koji (finishing): {finishing}")]);
}

#[tokio::test]
async fn finish_all_fails_when_a_runner_refuses() {
    let refused = r#"{"error":"no work item is open"}"#;
    let err = verb_line("finish", "--all", Some(refused)).await;
    assert_eq!(err.unwrap_err(), "koji: no work item is open");
}

#[tokio::test]
async fn finish_all_as_a_word_names_the_flag_when_no_project_is_called_all() {
    let err = verb_line("finish", "all", None).await.unwrap_err();
    assert!(err.contains("no kelpie runner named all"), "{err}");
    assert!(err.contains("finish --all finishes every project"), "{err}");
}

#[tokio::test]
async fn a_refused_answer_prints_its_reason_and_fails_in_both_forms() {
    let refused = r#"{"error":"no work item for #8 is open"}"#;
    for (command, args) in [
        ("timings", "20"),
        ("board", ""),
        ("board", "top 8"),
        ("drain", ""),
        ("undrain", ""),
        ("gate", "8"),
        ("drop", "8"),
        ("tell", "hold #4"),
        ("add", "8"),
        ("finish", ""),
    ] {
        for flag in ["", " --json"] {
            let err = verb(command, &format!("{args}{flag}"), Some(refused)).await;
            assert_eq!(
                err.unwrap_err(),
                "no work item for #8 is open",
                "{command} {args}{flag}"
            );
        }
    }
}

#[tokio::test]
async fn json_is_refused_where_there_is_nothing_to_print_as_json() {
    for (command, args) in [("rework", "7 --json"), ("add", "--json"), ("pm", "--json")] {
        let err = verb(command, args, None).await.unwrap_err();
        assert!(
            err.starts_with("usage: shep kelpie add"),
            "{command}: {err}"
        );
    }
}

#[tokio::test]
async fn status_prints_json_for_the_flag_wherever_it_stands() {
    let object = r#"{"action":"status","sheep":"koji"}"#;
    assert_eq!(status("--json").await.unwrap(), [format!("koji: {object}")]);
    for line in ["--json -p koji", "-p koji --json", "--project=koji --json"] {
        assert_eq!(status(line).await.unwrap(), [object], "{line}");
    }
}

#[tokio::test]
async fn status_prints_a_plain_block_without_the_flag() {
    let block = [
        "koji",
        "  run: running",
        "  build: not recorded",
        "  work items: none",
        "  rulings waiting: none",
        "  pacer: on",
    ];
    assert_eq!(status("").await.unwrap(), block);
    assert_eq!(status("-p koji").await.unwrap(), block);
}

#[tokio::test]
async fn status_refuses_a_flag_it_does_not_know() {
    for line in ["--yaml", "--json --yaml", "-p koji --yaml", "--jsn"] {
        let err = status(line).await.unwrap_err();
        assert!(err.starts_with("usage: shep kelpie add"), "{line}: {err}");
    }
}

#[test]
fn a_project_named_like_a_ticket_cannot_be_registered() {
    for name in ["7", "k12", "k0"] {
        let name = ProjectName::try_from(name).unwrap();
        let refused = not_a_ticket(&name).unwrap_err();
        assert!(
            refused.contains("reads as a ticket id"),
            "{name}: {refused}"
        );
    }
    for name in ["koji", "k", "k12x", "kk12", "k-12"] {
        let name = ProjectName::try_from(name).unwrap();
        assert_eq!(not_a_ticket(&name), Ok(()), "{name}");
    }
}

#[tokio::test]
async fn attach_refuses_what_is_no_ticket_id() {
    for bad in ["#7", "0", "k0", "x"] {
        let refused = verb("attach", bad, None).await.unwrap_err();
        assert_eq!(refused, format!("{bad:?} is not a ticket id"));
    }
}

#[tokio::test]
async fn a_glob_in_p_on_start_is_refused_and_starts_nothing() {
    let shepherd = koji_shepherd(None);
    for line in ["-p k*", "-p ?oji", "--project=[k]*"] {
        let refused = verb_line_on(&shepherd, "start", line).await.unwrap_err();
        assert!(refused.contains("`set` and `get`"), "{line}: {refused}");
    }
}

#[tokio::test]
async fn get_with_a_glob_prints_a_line_per_matching_project() {
    let shepherd = koji_shepherd(None);
    let said = verb_line_on(&shepherd, "get", "-p * ci.block")
        .await
        .unwrap();
    assert_eq!(said, ["project koji: true"]);
    let said = verb_line_on(&shepherd, "get", "-p k?ji git.nothing")
        .await
        .unwrap();
    assert_eq!(said, ["project koji: not set"]);
}

#[tokio::test]
async fn get_with_a_glob_that_matches_nothing_or_names_no_key_is_refused() {
    let shepherd = koji_shepherd(None);
    let none = verb_line_on(&shepherd, "get", "-p z* ci.block")
        .await
        .unwrap_err();
    assert!(none.contains("z*"), "{none}");
    let keyless = verb_line_on(&shepherd, "get", "-p *").await.unwrap_err();
    assert!(keyless.contains("needs a key"), "{keyless}");
}

#[tokio::test]
async fn set_with_a_glob_writes_the_matching_projects_and_a_plain_name_still_writes_one() {
    let mut shepherd = koji_shepherd(None);
    let said = verb_line_on(&shepherd, "set", "-p k* ci.fix_attempts 4")
        .await
        .unwrap();
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].starts_with("project koji: ci.fix_attempts = 4;"),
        "{said:?}"
    );
    assert_eq!(shepherd.writes().len(), 1);
    let said = verb_line_on(&shepherd, "set", "-p koji ci.fix_attempts 5")
        .await
        .unwrap();
    assert!(
        said[0].starts_with("project koji: ci.fix_attempts = 5;"),
        "{said:?}"
    );
    let refused = verb_line_on(&shepherd, "set", "-p z* ci.fix_attempts 5")
        .await
        .unwrap_err();
    assert!(refused.contains("z*"), "{refused}");
}

// Counts the dials it refuses.
#[derive(Debug, Default)]
struct Unreachable(std::sync::atomic::AtomicUsize);

impl Dial for Unreachable {
    fn open<'a>(&'a self, _: &'a Path) -> crate::shepherd::Opened<'a> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Err(crate::shepherd::ConnectRefused::Unreachable("no".into())) })
    }
}

// An inbox of one notification, which records what is marked done.
#[derive(Default)]
struct OneInbox(std::sync::Mutex<Vec<String>>);

impl Notifications for OneInbox {
    fn list(&self, repo: &ForgeSlug) -> Result<Vec<Notification>, ForgeError> {
        assert_eq!(repo.as_str(), "shep-pm/koji");
        Ok(vec![Notification { id: "7".into() }])
    }

    fn mark_done(&self, notification: &Notification) -> Result<(), ForgeError> {
        self.0.lock().unwrap().push(notification.id.clone());
        Ok(())
    }
}

// `notifications <args>` in `folder`, with no shepherd to dial.
fn notifications_in(
    folder: &Path,
    inbox: &OneInbox,
    dial: &Unreachable,
    args: &[&str],
) -> Result<Vec<String>, String> {
    let at = Notify {
        dial,
        shep_home: Path::new("/nowhere/shep"),
        home: Path::new("/nowhere/home"),
        kelpie_home: Path::new("/nowhere/kelpie"),
        here: folder,
        ports: inbox,
        clock: &FakeClock::at(1_000),
    };
    shepherd::block_on(notify(at, None, args))
}

// A git checkout whose `origin` is `shep-pm/koji`, a project or not.
fn koji_checkout() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        &["init", "-q"][..],
        &["remote", "add", "origin", "git@github.com:shep-pm/koji.git"],
    ] {
        let ran = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(args)
            .output();
        assert!(ran.unwrap().status.success(), "git {args:?}");
    }
    dir
}

#[test]
fn notifications_with_no_project_never_dials_the_shepherd() {
    let checkout = koji_checkout();
    let (inbox, dial) = (OneInbox::default(), Unreachable::default());
    let said = notifications_in(checkout.path(), &inbox, &dial, &["clear"]);
    assert_eq!(
        said,
        Ok(vec!["cleared 1 notifications on shep-pm/koji".into()])
    );
    assert_eq!(*inbox.0.lock().unwrap(), ["7"]);
    assert_eq!(dial.0.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn notifications_with_a_word_other_than_clear_is_refused() {
    let checkout = koji_checkout();
    for args in [&[][..], &["list"], &["clear", "all"], &["--all"]] {
        let (inbox, dial) = (OneInbox::default(), Unreachable::default());
        let refused = notifications_in(checkout.path(), &inbox, &dial, args);
        assert_eq!(refused, Err(USAGE.to_owned()), "{args:?}");
        assert!(inbox.0.lock().unwrap().is_empty(), "{args:?}");
    }
}
