use super::*;

#[tokio::test]
async fn a_project_that_spends_codex_checks_codex_answers_its_usage() {
    let codex = "---\nrole: implementer\nharness: stand-in\nmodel: gpt-5-codex\n\
                 effort: medium\nusage: codex\n---\n";
    let scene = Scene::leaving_out_on_koji("codex", codex).await;
    scene.runs("golbat", |t| {
        holds(t, &["codex"]);
    });
    let report = scene.report().await;
    let found = ok(&report, "golbat: codex usage");
    assert_eq!(found, "reads Codex usage: 5-hour window 0%, week 0%");
    assert!(!subjects(&report).contains(&"koji: codex usage"));

    let fixes = [
        (
            "cannot run codex: No such file or directory",
            "install the Codex CLI",
        ),
        ("codex refused: 402 Payment Required", "codex login"),
    ];
    for (reason, fix) in fixes {
        scene.codex_meter.fail(MeterError::Codex(reason.into()));
        let (what, said) = missing(&scene.report().await, "golbat: codex usage");
        assert_eq!(what, reason);
        assert!(said.contains(fix), "{said}");
    }
    scene
        .codex_meter
        .fail(MeterError::Codex("codex did not answer in time".into()));
    let report = scene.report().await;
    assert!(matches!(
        verdict(&report, "golbat: codex usage"),
        Verdict::Unsure { .. }
    ));
}

// A pi agent on a server of its own, which no gateway fronts
const QWEN: &str = "---\nrole: implementer\nharness: pi\nmodel: qwen3.8:27b\neffort: low\n\
                    url: http://box:11434/v1\ncontext: 65536\n---\n";

impl Scene {
    /// A flock holding the agent file `text` as `name`, which `koji`'s `agents.allow` leaves out
    async fn leaving_out_on_koji(name: &str, text: &str) -> Self {
        Self::with_koji(|scene, t| {
            let agents = scene.kelpie_home.join("agents");
            crate::test::write_in(&agents, &format!("{name}.md"), text);
            let book = crate::agents::Agents::load(&agents).unwrap();
            // `iq3-xxs-review` is left out too: a project that names a reviewer on a
            // gateway kelpie's settings hold none of is the reviewers line's to refuse.
            let rest: Vec<&str> = (book.iter().map(|(n, _)| n.as_str()))
                .filter(|n| *n != name && *n != crate::agents::IQ3_XXS_REVIEW)
                .collect();
            t["agents"]["allow"] = json!(rest);
        })
        .await
    }

    /// A flock whose `golbat` may run a pi agent, which `koji` does not run
    async fn pi_on_golbat() -> Self {
        let scene = Self::leaving_out_on_koji("qwen", QWEN).await;
        scene.runs("golbat", |t| {
            holds(t, &["qwen"]);
        });
        scene
    }
}

fn unsure(report: &Report, subject: &str) -> (String, String) {
    match verdict(report, subject) {
        Verdict::Unsure { what, next } => (what.clone(), next.clone()),
        other => panic!("`{subject}` is {other:?}"),
    }
}

#[tokio::test]
async fn a_project_with_a_pi_agent_gets_a_pi_line_that_is_ok_at_the_minimum_and_above() {
    let mut scene = Scene::pi_on_golbat().await;
    let report = scene.report().await;
    assert_eq!(
        ok(&report, "golbat: pi"),
        "pi 1.0.4, kelpie needs 1.0.4 or later"
    );
    assert!(report.passed(), "{:#?}", report.render());

    scene.host.pi_version = Some(Ok("1.0.10"));
    let report = scene.report().await;
    assert_eq!(
        ok(&report, "golbat: pi"),
        "pi 1.0.10, kelpie needs 1.0.4 or later"
    );
}

#[tokio::test]
async fn a_pi_older_than_the_minimum_is_missing_and_says_to_upgrade() {
    let mut scene = Scene::pi_on_golbat().await;
    scene.host.pi_version = Some(Ok("0.85.1"));
    let report = scene.report().await;
    let (what, fix) = missing(&report, "golbat: pi");
    assert_eq!(what, "pi 0.85.1 is older than the 1.0.4 kelpie needs");
    assert!(fix.contains("brew upgrade pi-coding-agent"), "{fix}");
    assert!(!report.passed());
}

#[tokio::test]
async fn a_pi_off_the_path_is_missing_and_says_to_install() {
    let mut scene = Scene::pi_on_golbat().await;
    scene.host.off_path = vec!["pi"];
    let report = scene.report().await;
    let (what, fix) = missing(&report, "golbat: pi");
    assert!(what.contains("not on your PATH"), "{what}");
    assert!(fix.contains("brew install pi-coding-agent"), "{fix}");
    assert!(!report.passed());
}

// pi's own `grep` and `find` run `rg` and `fd`. It looks for them in its own
// `bin` folder, which is empty for every kelpie call, then on `PATH`, then
// downloads them, which the sandbox refuses. `docs/facts/tool-errors.md`.
#[tokio::test]
async fn a_pi_tool_off_the_path_is_missing_and_says_to_install() {
    let mut scene = Scene::pi_on_golbat().await;
    scene.host.off_path = vec!["rg"];
    let report = scene.report().await;
    let (what, fix) = missing(&report, "golbat: pi tools");
    assert_eq!(
        what,
        "`grep` runs `rg`, which is not on your PATH, so pi's `grep` tool fails in every call"
    );
    assert_eq!(
        fix,
        "install them: `brew install ripgrep fd` on macOS, `apt-get install ripgrep fd-find` on Debian"
    );
    assert!(!report.passed(), "{:#?}", report.render());
    assert!(!subjects(&report).contains(&"koji: pi tools"));
}

#[tokio::test]
async fn each_pi_tool_off_the_path_is_named_by_the_tool_it_breaks() {
    let mut scene = Scene::pi_on_golbat().await;

    scene.host.path = Some(vec!["pi", "rg"]);
    let (what, _) = missing(&scene.report().await, "golbat: pi tools");
    assert_eq!(
        what,
        "`find` runs `fd` or `fdfind`, and neither is on your PATH, so pi's `find` tool fails in every call"
    );

    scene.host.path = Some(vec!["pi"]);
    let (what, _) = missing(&scene.report().await, "golbat: pi tools");
    assert_eq!(
        what,
        "`grep` runs `rg`, which is not on your PATH, so pi's `grep` tool fails in every call; \
         `find` runs `fd` or `fdfind`, and neither is on your PATH, so pi's `find` tool fails in every call"
    );
}

// pi takes either name for `fd`, and Debian ships it as `fdfind`. The line
// names the program it actually found, so an operator looking for `fd` is not
// told it is there when it is not.
#[tokio::test]
async fn the_ok_pi_tools_line_names_the_program_it_found_on_the_path() {
    let mut scene = Scene::pi_on_golbat().await;

    scene.host.path = Some(vec!["pi", "rg", "fd"]);
    assert_eq!(
        ok(&scene.report().await, "golbat: pi tools"),
        "`rg` and `fd` are on your PATH, which pi's `grep` and `find` run"
    );

    scene.host.path = Some(vec!["pi", "rg", "fdfind"]);
    assert_eq!(
        ok(&scene.report().await, "golbat: pi tools"),
        "`rg` and `fdfind` are on your PATH, which pi's `grep` and `find` run (`fdfind` is `fd` under another name)"
    );
}

#[tokio::test]
async fn a_project_with_no_pi_agent_gets_no_pi_tools_line() {
    let mut scene = Scene::pi_on_golbat().await;
    scene.host.off_path = vec!["rg", "fd"];
    let report = scene.report().await;
    assert!(!subjects(&report).contains(&"koji: pi tools"));
    assert!(subjects(&report).contains(&"golbat: pi tools"));
}

#[tokio::test]
async fn a_pi_whose_version_cannot_be_read_is_unsure_and_never_fails_the_run() {
    let mut scene = Scene::pi_on_golbat().await;
    let unreadable = [
        Err("pi --version did not answer in 10s"),
        Ok("1.0.4-beta.1"),
        Ok("pi coding agent"),
    ];
    for answer in unreadable {
        scene.host.pi_version = Some(answer);
        let report = scene.report().await;
        let (what, next) = unsure(&report, "golbat: pi");
        assert!(what.contains("pi --version"), "{what}");
        assert_eq!(next, "run `pi --version`");
        assert!(report.passed(), "{:#?}", report.render());
    }
}

#[tokio::test]
async fn a_project_with_no_pi_agent_gets_no_pi_line() {
    let mut scene = Scene::pi_on_golbat().await;
    scene.host.off_path = vec!["pi"];
    let report = scene.report().await;
    assert!(!subjects(&report).contains(&"koji: pi"));
    assert!(subjects(&report).contains(&"golbat: pi"));
    assert!(subjects(&report).iter().any(|s| s.starts_with("koji: ")));
}

// A pi reviewer on a server of its own, which no gateway fronts
const QWEN_REVIEWER: &str = "---\nrole: reviewer\nharness: pi\nmodel: qwen3.8:27b\neffort: low\n\
                             url: http://box:11434/v1\ncontext: 65536\n---\nReview the diff.\n";

#[tokio::test]
async fn a_pi_reviewer_of_contributions_gets_a_pi_line_only_while_contributions_are_picked_up() {
    let mut scene = Scene::new().await;
    crate::test::write_in(
        &scene.kelpie_home.join("agents"),
        "qwen-r.md",
        QWEN_REVIEWER,
    );
    scene.host.pi_version = Some(Ok("0.85.1"));
    for (project, pick, key, value) in [
        ("lists", "label", "reviewers", json!(["qwen-r"])),
        ("lists-off", "off", "reviewers", json!(["qwen-r"])),
        ("secures", "all", "security_reviewer", json!("qwen-r")),
        ("secures-off", "off", "security_reviewer", json!("qwen-r")),
    ] {
        scene.runs(project, |t| {
            t.insert("contributions".into(), json!({ "pick": pick, key: value }));
            // A reviewer file is a candidate for every review round, so only a
            // project picking contributions may name one at all.
            if pick != "off" {
                holds(t, &["qwen-r"]);
            }
        });
    }
    let report = scene.report().await;
    for project in ["lists", "secures"] {
        let (what, _) = missing(&report, &format!("{project}: pi"));
        assert!(what.contains("older"), "{what}");
    }
    for project in ["lists-off", "secures-off"] {
        assert!(!subjects(&report).contains(&format!("{project}: pi").as_str()));
    }
}

// A goose reviewer on a server of its own, which no gateway fronts
const GOOSE: &str = "---\nrole: reviewer\nharness: goose\nmodel: qwen3.8:27b\neffort: low\n\
                     url: http://box:11434/v1\ncontext: 65536\n---\nReview.\n";

impl Scene {
    /// Lists a goose agent for `golbat`, which `koji` does not run
    fn runs_goose_on_golbat(&self) {
        crate::test::write_in(&self.kelpie_home.join("agents"), "goose.md", GOOSE);
        self.runs("golbat", |t| {
            holds(t, &["goose"]);
        });
    }
}

#[tokio::test]
async fn a_project_with_a_goose_agent_gets_a_goose_line_that_is_ok_at_the_minimum_and_above() {
    let mut scene = Scene::new().await;
    scene.runs_goose_on_golbat();
    let report = scene.report().await;
    assert_eq!(
        ok(&report, "golbat: goose"),
        "goose 1.53.0, kelpie needs 1.53.0 or later"
    );
    assert!(report.passed(), "{:#?}", report.render());

    scene.host.goose_version = Some(Ok("1.54.0"));
    let report = scene.report().await;
    assert_eq!(
        ok(&report, "golbat: goose"),
        "goose 1.54.0, kelpie needs 1.53.0 or later"
    );
}

#[tokio::test]
async fn a_goose_older_than_the_minimum_is_missing_and_says_to_upgrade() {
    let mut scene = Scene::new().await;
    scene.runs_goose_on_golbat();
    scene.host.goose_version = Some(Ok("1.47.9"));
    let report = scene.report().await;
    let (what, fix) = missing(&report, "golbat: goose");
    assert_eq!(what, "goose 1.47.9 is older than the 1.53.0 kelpie needs");
    assert!(fix.contains("brew upgrade block-goose-cli"), "{fix}");
    assert!(!report.passed());
}

#[tokio::test]
async fn a_goose_off_the_path_is_missing_and_says_to_install() {
    let mut scene = Scene::new().await;
    scene.runs_goose_on_golbat();
    scene.host.off_path = vec!["goose"];
    let report = scene.report().await;
    let (what, fix) = missing(&report, "golbat: goose");
    assert!(what.contains("not on your PATH"), "{what}");
    assert!(fix.contains("brew install block-goose-cli"), "{fix}");
    assert!(!report.passed());
}

#[tokio::test]
async fn a_goose_whose_version_cannot_be_read_is_unsure_and_never_fails_the_run() {
    let mut scene = Scene::new().await;
    scene.runs_goose_on_golbat();
    let unreadable = [
        Err("goose --version did not answer in 10s"),
        Ok("1.53.0-beta.1"),
        Ok("goose cli"),
    ];
    for answer in unreadable {
        scene.host.goose_version = Some(answer);
        let report = scene.report().await;
        let (what, next) = unsure(&report, "golbat: goose");
        assert!(what.contains("goose --version"), "{what}");
        assert_eq!(next, "run `goose --version`");
        assert!(report.passed(), "{:#?}", report.render());
    }
}

#[tokio::test]
async fn a_project_with_no_goose_agent_gets_no_goose_line() {
    let mut scene = Scene::new().await;
    scene.host.off_path = vec!["goose"];
    scene.runs_goose_on_golbat();
    let report = scene.report().await;
    assert!(!subjects(&report).contains(&"koji: goose"));
    assert!(subjects(&report).contains(&"golbat: goose"));
    assert!(subjects(&report).iter().any(|s| s.starts_with("koji: ")));
}

// The shared reader already trims the line `program --version` prints, so
// nothing was added to it. The fake host trims as it does, and the reader's
// own test in `crates/adapters/src/adapters/host.rs` runs a script that prints ` 1.54.0`.
#[tokio::test]
async fn goose_prints_its_version_with_a_leading_space_and_it_is_read() {
    let mut scene = Scene::new().await;
    scene.runs_goose_on_golbat();
    for (printed, passes) in [(" 1.54.0", true), (" 1.52.9", false)] {
        scene.host.goose_version = Some(Ok(printed));
        let report = scene.report().await;
        if passes {
            assert_eq!(
                ok(&report, "golbat: goose"),
                "goose 1.54.0, kelpie needs 1.53.0 or later"
            );
        } else {
            let (what, _) = missing(&report, "golbat: goose");
            assert_eq!(what, "goose 1.52.9 is older than the 1.53.0 kelpie needs");
        }
    }
}
