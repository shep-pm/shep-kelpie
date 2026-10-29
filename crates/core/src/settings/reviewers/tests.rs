use super::*;
use crate::agents::Agents;
use std::collections::BTreeMap;

use crate::settings::{Effort, EndpointUrl, Gateway, GatewayKind, GatewayName, KeyVar, Settings};

const EXAMPLE: &str = include_str!("../../../../../settings.example.toml");

/// The example's reviewers key, commented out and read by nothing
const GONE: &str = "# reviewers = [\"iq3-xxs-review\", \"defect-hunter-lite\"]\n";

/// Every reviewer file `book()` holds that a pass may pick, in its order
const EVERY_REVIEWER: [&str; 11] = [
    "coderabbit",
    "codex",
    "cubic",
    "defect-hunter",
    "defect-hunter-lite",
    "gpu-box",
    "iq3-xxs-review",
    "opus",
    "qwen",
    "spec",
    "standards",
];

const OPUS: &str = "---\nrole: reviewer\nharness: claude-code\nmodel: claude-opus-5-5\n\
                    effort: high\npaths: [\"src/runner/merge/**\"]\n---\nRead it for defects.\n";

const GPU_BOX: &str = "---\nrole: reviewer\nharness: endpoint\nurl: http://gpu-box:11434/v1\n\
                       model: coder\ncontext: 32768\n---\n";

const CODEX_ON_READY: &str = "---\nrole: reviewer\nharness: bot\nbot: codex\nreviews: 10\n\
                              hours: 168\nreviews_on_ready: true\n---\n";

// Kelpie's own agents with every baseline bound, a Claude reviewer on some paths and an endpoint.
fn book() -> Agents {
    bound(Agents::embedded())
        .with("opus", OPUS)
        .with("gpu-box", GPU_BOX)
}

// `agents` with each baseline bound to Opus 5.5 at high effort.
fn bound(agents: Agents) -> Agents {
    agents.bind_all("harness: claude-code\nmodel: claude-opus-5-5\neffort: high\n")
}

// The example's project, with `agents` added to its `[agents]` table and
// `contribution` as its contributions reviewers, and its home.
fn project(agents: &str, contribution: Option<&str>) -> (Settings, tempfile::TempDir) {
    assert!(EXAMPLE.contains(GONE), "the example's reviewers key moved");
    let mut entry = EXAMPLE.replace(GONE, &format!("{GONE}{agents}"));
    if let Some(list) = contribution {
        entry = crate::test::with_tables(
            &entry,
            &format!("[app.dogs.kelpie.contributions]\nreviewers = {list}\n"),
        );
    }
    let home = tempfile::tempdir().unwrap();
    let table = crate::test::project_table(&entry);
    let settings = Settings::from_table(&table, "shep", home.path(), Path::new("/p")).unwrap();
    (settings, home)
}

// The example's project with `contribution` as its contributions reviewers.
fn contributing(contribution: &str) -> (Settings, tempfile::TempDir) {
    project("", Some(contribution))
}

fn names(lineup: &[ListedReviewer]) -> Vec<&str> {
    lineup.iter().map(|r| r.name.as_str()).collect()
}

#[test]
fn a_contributions_list_runs_its_reviewers_in_order_each_once() {
    let (settings, home) = contributing(r#"["opus", "qwen", "gpu-box", "opus"]"#);
    let lineup = settings
        .contribution_lineup(&book(), &Gateways::default(), home.path())
        .unwrap();
    assert_eq!(names(&lineup), ["opus", "qwen", "gpu-box"]);

    let opus = &lineup[0];
    let (model, _) = opus.runs.session().unwrap();
    assert_eq!(
        (model.model.as_str(), model.effort),
        ("claude-opus-5-5", Effort::High)
    );
    assert_eq!(opus.paths[0].as_str(), "src/runner/merge/**");
    assert_eq!(opus.prompt.as_deref(), Some("Read it for defects."));
    assert!(!opus.is_local());

    let Runs::Command(qwen) = &lineup[1].runs else {
        panic!("{:?}", lineup[1]);
    };
    assert_eq!(
        qwen.command,
        home.path().join(".claude/scripts/qwen-review.sh"),
        "`~/` is the home folder"
    );
    assert!(lineup[1].is_local() && lineup[2].is_local());
}

#[test]
fn an_empty_contributions_list_runs_no_review() {
    let (settings, home) = contributing("[]");
    let lineup = settings
        .contribution_lineup(&book(), &Gateways::default(), home.path())
        .unwrap();
    assert_eq!(names(&lineup), Vec::<&str>::new());
}

#[test]
fn a_contributions_reviewer_with_no_file_or_an_implementers_is_refused_naming_it() {
    let (settings, home) = contributing(r#"["fable"]"#);
    let err = (settings.contribution_lineup(&book(), &Gateways::default(), home.path()))
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "setting `agents`: `contributions.reviewers` names fable, which has no agent file: \
         write `agents/fable.md` in kelpie's home"
    );
    let (settings, home) = contributing(r#"["sonnet-high"]"#);
    let err = (settings.contribution_lineup(&book(), &Gateways::default(), home.path()))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains(
            "`contributions.reviewers` names sonnet-high, whose agent file's role is `implementer`"
        ),
        "{err}"
    );
}

// Kelpie's settings holding one gateway, `name`, of `kind`.
fn gateways_with(name: &str, kind: GatewayKind) -> Gateways {
    let gateway = Gateway {
        kind,
        url: EndpointUrl::try_from("http://gpu-box:8700".to_owned()).unwrap(),
        key_env: KeyVar::try_from("PADDOCK_KEY".to_owned()).unwrap(),
    };
    let name = GatewayName::try_from(name.to_owned()).unwrap();
    Gateways::new(BTreeMap::from([(name, gateway)]))
}

#[test]
fn a_commands_leading_tilde_is_the_home_folder_and_an_absolute_path_stays() {
    let command =
        |path: &str| format!("---\nrole: reviewer\nharness: command\ncommand: {path}\n---\n");
    let book = book()
        .with("mine", &command("~/bin/review"))
        .with("opt", &command("/opt/review"));
    let (settings, home) = project("", None);
    let lineup = settings.lineup(&book, &Gateways::default(), home.path());
    let paths: Vec<_> = (lineup.iter())
        .filter(|r| r.name.as_str() == "mine" || r.name.as_str() == "opt")
        .map(|r| match &r.runs {
            Runs::Command(c) => c.command.clone(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        paths,
        [home.path().join("bin/review"), "/opt/review".into()]
    );
}

#[test]
fn a_bot_that_reviews_on_ready_is_refused_after_another_bot_only_in_a_contributions_list() {
    let book = book().with("codex", CODEX_ON_READY);
    let (settings, home) = contributing(r#"["defect-hunter", "coderabbit", "codex"]"#);
    let err = (settings.contribution_lineup(&book, &Gateways::default(), home.path()))
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "setting `contributions.reviewers`: codex reviews a pull request when it leaves \
         draft, so list it before coderabbit: a bot listed before it marks the draft ready, \
         which draws Codex's own review outside its lease"
    );
    let (settings, home) = contributing(r#"["codex", "defect-hunter", "coderabbit"]"#);
    let lineup = settings
        .contribution_lineup(&book, &Gateways::default(), home.path())
        .unwrap();
    assert_eq!(names(&lineup), ["codex", "defect-hunter", "coderabbit"]);

    // A pass's candidates are kelpie's own files in their own order, which no
    // bot may reorder: kelpie's Codex file does not review on ready.
    let (settings, home) = project("", None);
    let lineup = settings.lineup(
        &bound(Agents::embedded()),
        &Gateways::default(),
        home.path(),
    );
    assert!(names(&lineup).contains(&"coderabbit"));
}

#[test]
fn the_candidates_are_every_reviewer_file_allow_holds_but_the_security_reviewer() {
    let (mut settings, home) = project("", None);
    let paddock = gateways_with("paddock", GatewayKind::Paddock);
    let lineup = settings.lineup(&book(), &paddock, home.path());
    assert_eq!(
        names(&lineup),
        EVERY_REVIEWER,
        "security-reviewer, an implementer and the project manager are not reviewers a pass reads"
    );

    let none = settings.lineup(&book(), &Gateways::default(), home.path());
    assert!(
        !names(&none).contains(&"iq3-xxs-review"),
        "a reviewer on a gateway the settings do not hold cannot run"
    );

    settings.agents.allow =
        Some(vec![AgentName::kelpies("spec"), AgentName::kelpies("opus")].into());
    let allowed = settings.lineup(&book(), &paddock, home.path());
    assert_eq!(names(&allowed), ["opus", "spec"]);

    settings.contributions.security_reviewer = AgentName::kelpies("spec");
    let moved = settings.lineup(&book(), &paddock, home.path());
    assert_eq!(names(&moved), ["opus"]);
}

#[test]
fn a_reviewer_the_project_named_stays_a_candidate_whatever_its_gateway() {
    // A project that names the agents it may use is held to what it named, so a
    // start can refuse it. A project that names none is held to none of its
    // candidates, so one kelpie cannot route is no candidate at all.
    let (mut settings, home) = project("", None);
    assert!(
        settings.named_reviewers(&book(), home.path()).is_empty(),
        "a project that names no agent named no reviewer"
    );
    let none = settings.lineup(&book(), &Gateways::default(), home.path());
    assert!(
        !names(&none).contains(&"iq3-xxs-review"),
        "{:?}",
        names(&none)
    );

    settings.agents.allow = Some(vec![AgentName::kelpies("iq3-xxs-review")].into());
    let named = settings.lineup(&book(), &Gateways::default(), home.path());
    assert_eq!(
        names(&named),
        ["iq3-xxs-review"],
        "the project named it, so a start can refuse it"
    );
    assert_eq!(
        names(&settings.named_reviewers(&book(), home.path())),
        ["iq3-xxs-review"]
    );
}

#[test]
fn the_candidates_ignore_an_old_reviewers_list() {
    let paddock = gateways_with("paddock", GatewayKind::Paddock);
    let (settings, home) = project("reviewers = [\"opus\", \"fable\", \"sonnet-high\"]\n", None);
    assert_eq!(
        settings.agents.gone_keys(),
        ["agents.reviewers"],
        "the key loads and is named as gone"
    );
    let lineup = settings.lineup(&book(), &paddock, home.path());
    assert_eq!(names(&lineup), EVERY_REVIEWER);

    // An empty list once meant no review at all. It now means nothing.
    let (empty, home) = project("reviewers = []\n", None);
    let lineup = empty.lineup(&book(), &paddock, home.path());
    assert_eq!(names(&lineup), EVERY_REVIEWER);
    assert_eq!(empty.agents.gone_keys(), ["agents.reviewers"]);

    let (absent, home) = project("", None);
    assert_eq!(absent.agents.gone_keys(), Vec::<&str>::new());
    assert_eq!(
        names(&absent.lineup(&book(), &paddock, home.path())),
        EVERY_REVIEWER
    );
}

#[test]
fn a_contribution_with_no_list_gets_the_default_lineup() {
    let paddock = gateways_with("paddock", GatewayKind::Paddock);
    let (settings, home) = project("", None);
    let lineup = (settings.contribution_lineup(&book(), &paddock, home.path())).unwrap();
    assert_eq!(names(&lineup), ["iq3-xxs-review", "defect-hunter-lite"]);

    let (settings, home) = project("", None);
    let lineup =
        (settings.contribution_lineup(&book(), &Gateways::default(), home.path())).unwrap();
    assert_eq!(
        names(&lineup),
        ["defect-hunter-lite"],
        "no paddock gateway, no local reviewer"
    );

    // An old `agents.reviewers` list is read by nothing here either.
    let (old, home) = project("reviewers = [\"opus\"]\n", None);
    let lineup = (old.contribution_lineup(&book(), &paddock, home.path())).unwrap();
    assert_eq!(names(&lineup), ["iq3-xxs-review", "defect-hunter-lite"]);
}

#[test]
fn a_default_reviewer_that_allow_leaves_out_is_refused_naming_the_contribution_key() {
    let (settings, home) = project("allow = [\"opus\"]\n", None);
    let err = (settings.contribution_lineup(&book(), &Gateways::default(), home.path()))
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "setting `agents`: `contributions.reviewers` names defect-hunter-lite, which \
         `agents.allow` leaves out: add it to `agents.allow`, or name another"
    );
}
