use std::collections::BTreeMap;
use std::sync::Mutex;

use super::*;
use crate::adapters::host::{Host, WindowsInstall};
use crate::settings::{
    AgentName, EndpointUrl, Gateway, GatewayKind, GatewayName, Gateways, Harness, KeyVar,
};
use crate::test::StandInEndpoint;

const UNBOUND_PM: &str = "---\nrole: pm\n---\nYou keep the project's notes.\n";

fn name(name: &str) -> AgentName {
    AgentName::try_from(name.to_owned()).unwrap()
}

// `shep kelpie agents <line>` on `folder`.
fn agents(folder: &Path, line: &str) -> Result<Vec<String>, String> {
    let args: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
    run(folder, &args)
}

fn file(folder: &Path, name: &str) -> String {
    std::fs::read_to_string(folder.join(format!("{name}.md"))).unwrap()
}

fn files(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

const PM_ON_CLAUDE: &str = "new pm --harness claude-code --model claude-opus-5-5 --effort high";

#[test]
fn new_writes_a_file_that_extends_the_baseline_and_loads_with_its_prompt() {
    let dir = tempfile::tempdir().unwrap();

    let said = agents(dir.path(), PM_ON_CLAUDE).unwrap();

    assert_eq!(said, ["wrote pm.md"]);
    assert_eq!(
        file(dir.path(), "pm"),
        "---\nextends: kelpie:pm\nharness: claude-code\nmodel: claude-opus-5-5\neffort: high\n---\n"
    );
    let book = Agents::load(dir.path()).unwrap();
    let pm = book.get(&name("pm")).expect("the new file is in the book");
    let (_, body) = baseline::named("pm")
        .unwrap()
        .text
        .split_once("\n---\n")
        .unwrap();
    assert_eq!(pm.prompt.as_deref(), Some(body.trim()));
}

#[test]
fn name_gives_the_file_its_own_name_and_the_binding_flags_are_written() {
    let dir = tempfile::tempdir().unwrap();
    let line = "new reviewer --baseline spec --name spec-pi --harness pi --model qwen3.8:27b \
                --effort low --url http://box:11434/v1 --context 65536 --second-look";

    let said = agents(dir.path(), line).unwrap();

    assert_eq!(said, ["wrote spec-pi.md"]);
    assert_eq!(
        file(dir.path(), "spec-pi"),
        "---\nextends: kelpie:spec\nharness: pi\nmodel: qwen3.8:27b\neffort: low\n\
         second_look: true\nurl: http://box:11434/v1\ncontext: 65536\n---\n"
    );
    assert!(
        Agents::load(dir.path())
            .unwrap()
            .get(&name("spec-pi"))
            .is_some()
    );
}

#[test]
fn an_implementer_needs_a_name_and_is_written_with_its_role_in_place_of_a_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let bare = "new implementer --harness claude-code --model claude-sonnet-5-5 --effort high";

    let err = agents(dir.path(), bare).unwrap_err();
    assert!(err.contains("--name"), "{err}");
    assert_eq!(files(dir.path()), Vec::<String>::new());

    agents(dir.path(), &format!("{bare} --name mine")).unwrap();
    assert_eq!(
        file(dir.path(), "mine"),
        "---\nrole: implementer\nharness: claude-code\nmodel: claude-sonnet-5-5\neffort: high\n---\n"
    );
    assert!(
        Agents::load(dir.path())
            .unwrap()
            .get(&name("mine"))
            .is_some()
    );
}

#[test]
fn new_never_writes_over_a_file() {
    let dir = tempfile::tempdir().unwrap();
    agents(dir.path(), PM_ON_CLAUDE).unwrap();
    let before = file(dir.path(), "pm");

    let err = agents(dir.path(), &PM_ON_CLAUDE.replace("high", "low")).unwrap_err();

    assert!(err.contains("`pm.md` already exists"), "{err}");
    assert_eq!(file(dir.path(), "pm"), before);
}

#[test]
fn new_writes_over_a_stub_that_is_unbound() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pm.md"), UNBOUND_PM).unwrap();
    assert_eq!(Agents::load(dir.path()).unwrap().unbound().len(), 1);

    let said = agents(dir.path(), PM_ON_CLAUDE).unwrap();

    assert_eq!(said, ["wrote pm.md"]);
    let book = Agents::load(dir.path()).unwrap();
    assert!(book.unbound().is_empty());
    assert!(book.get(&name("pm")).is_some());
}

#[test]
fn a_reviewer_with_no_baseline_is_refused_naming_the_four() {
    let dir = tempfile::tempdir().unwrap();
    let line = "new reviewer --harness claude-code --model claude-opus-5-5 --effort high";

    let err = agents(dir.path(), line).unwrap_err();

    assert!(
        err.contains("defect-hunter, standards, spec, security-reviewer"),
        "{err}"
    );
    assert_eq!(files(dir.path()), Vec::<String>::new());
}

#[test]
fn a_baseline_of_another_role_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let line = format!("{PM_ON_CLAUDE} --baseline spec");

    let err = agents(dir.path(), &line).unwrap_err();

    assert!(
        err.contains("`spec` is not a baseline of the pm role"),
        "{err}"
    );
    assert_eq!(files(dir.path()), Vec::<String>::new());
}

#[test]
fn a_binding_the_loader_refuses_writes_nothing_and_prints_its_message() {
    let dir = tempfile::tempdir().unwrap();
    let line = "new pm --harness pi --model qwen3.8:27b --effort low";

    let err = agents(dir.path(), line).unwrap_err();

    assert!(err.contains("pi"), "{err}");
    assert_eq!(files(dir.path()), Vec::<String>::new());
}

#[test]
fn a_gateway_and_a_url_together_are_refused_and_a_missing_flag_is_named() {
    let dir = tempfile::tempdir().unwrap();
    let both = format!("{PM_ON_CLAUDE} --gateway paddock --url http://box/v1");
    assert!(agents(dir.path(), &both).unwrap_err().contains("--gateway"));
    let missing = agents(dir.path(), "new pm --harness claude-code --effort high").unwrap_err();
    assert!(missing.contains("--model"), "{missing}");
    let unknown = agents(dir.path(), &format!("{PM_ON_CLAUDE} --fast")).unwrap_err();
    assert!(unknown.contains("--fast"), "{unknown}");
    let role = agents(dir.path(), "new janitor --harness pi").unwrap_err();
    assert!(role.contains("janitor"), "{role}");
    assert_eq!(files(dir.path()), Vec::<String>::new());
}

#[test]
fn list_shows_bound_agents_unbound_files_and_baselines_that_bind_nothing_with_no_file() {
    let dir = tempfile::tempdir().unwrap();
    agents(dir.path(), PM_ON_CLAUDE).unwrap();
    std::fs::write(dir.path().join("scribe.md"), UNBOUND_PM).unwrap();
    let baselines = [
        *baseline::named("pm").unwrap(),
        Baseline {
            name: "triager",
            text: "---\nrole: triager\n---\nYou triage.\n",
        },
        Baseline {
            name: "spec",
            text: "---\nrole: reviewer\n---\nYou check the spec.\n",
        },
    ];

    let lines = lines(&Agents::load_over(dir.path(), &baselines).unwrap());

    let has = |line: &str| assert!(lines.iter().any(|l| l == line), "{line} in {lines:#?}");
    has("pm: pm, claude-code, claude-opus-5-5, high");
    let shipped = "sonnet-high: implementer, claude-code, claude-sonnet-5-5, high, tier standard";
    assert!(lines.iter().any(|l| l.starts_with(shipped)), "{lines:#?}");
    has("scribe: pm, unbound: run `shep kelpie agents new pm --name scribe`");
    has("triager: baseline, no file: run `shep kelpie agents new triager`");
    has("spec: baseline, no file: run `shep kelpie agents new reviewer --baseline spec`");
    let bound = lines.iter().position(|l| l.starts_with("pm: pm")).unwrap();
    let unbound = lines.iter().position(|l| l.starts_with("scribe:")).unwrap();
    let baseline = lines.iter().position(|l| l.starts_with("spec:")).unwrap();
    assert!(bound < unbound && unbound < baseline, "{lines:#?}");
}

#[test]
fn list_shows_each_agents_tier_kinds_and_description() {
    let dir = tempfile::tempdir().unwrap();
    agents(dir.path(), PM_ON_CLAUDE).unwrap();
    let heavy = "---\nrole: implementer\nharness: claude-code\nmodel: claude-opus-5-5\n\
                 effort: high\ndescription: Design work that\n  needs the strongest agent.\n\
                 kinds: [design, migration]\ntier: heavy\n---\n";
    let bare = "---\nrole: reviewer\nharness: claude-code\nmodel: claude-sonnet-5\n\
                effort: medium\n---\nReview the change.\n";
    std::fs::write(dir.path().join("heavy.md"), heavy).unwrap();
    std::fs::write(dir.path().join("bare.md"), bare).unwrap();

    let lines = agents(dir.path(), "list").unwrap();

    let has = |line: &str| assert!(lines.iter().any(|l| l == line), "{line} in {lines:#?}");
    has(
        "heavy: implementer, claude-code, claude-opus-5-5, high, tier heavy, \
         kinds [design, migration], description: Design work that needs the strongest agent.",
    );
    has("bare: reviewer, claude-code, claude-sonnet-5, medium, tier standard");
    has("pm: pm, claude-code, claude-opus-5-5, high");
}

#[test]
fn list_shows_every_baseline_as_missing_a_file_in_an_empty_folder() {
    let dir = tempfile::tempdir().unwrap();
    let lines = agents(dir.path(), "list").unwrap();
    let missing = lines.iter().filter(|l| l.contains("baseline, no file"));
    assert_eq!(missing.count(), 7, "{lines:#?}");
}

#[test]
fn an_unknown_verb_or_none_is_refused_with_the_usage() {
    let dir = tempfile::tempdir().unwrap();
    for line in ["", "make pm", "list pm"] {
        let err = agents(dir.path(), line).unwrap_err();
        assert!(err.contains("shep kelpie agents list"), "{line}: {err}");
    }
}

#[test]
fn new_over_an_unbound_file_keeps_its_body_and_the_keys_it_does_not_bind() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("scribe.md"), UNBOUND_PM).unwrap();
    let line = format!("{PM_ON_CLAUDE} --name scribe");

    agents(dir.path(), &line).unwrap();

    let book = Agents::load(dir.path()).unwrap();
    let prompt = book.get(&name("scribe")).unwrap().prompt.clone();
    assert_eq!(prompt.as_deref(), Some("You keep the project's notes."));

    let mine = "---\n# Mine.\nrole: implementer\ntools: Bash,Read\n---\nExtra care.\n";
    std::fs::write(dir.path().join("mine.md"), mine).unwrap();
    let bare = "new implementer --harness claude-code --model claude-sonnet-5-5 --effort high";

    agents(dir.path(), &format!("{bare} --name mine")).unwrap();

    let written = file(dir.path(), "mine");
    assert!(written.contains("tools: Bash,Read\n"), "{written}");
    assert!(written.contains("# Mine.\n"), "{written}");
    assert!(written.ends_with("---\nExtra care.\n"), "{written}");
    assert!(
        Agents::load(dir.path())
            .unwrap()
            .get(&name("mine"))
            .is_some()
    );
}

#[test]
fn an_unbound_reviewer_with_no_baseline_is_bound_by_the_command_kelpie_prints() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("mine.md"),
        "---\nrole: reviewer\n---\nRead it for defects.\n",
    )
    .unwrap();
    let book = Agents::load(dir.path()).unwrap();
    let command = book.unbound()[0].command();
    assert_eq!(command, "shep kelpie agents new reviewer --name mine");

    let said = agents(
        dir.path(),
        &format!(
            "{} --harness claude-code --model claude-opus-5-5 --effort high",
            command.strip_prefix("shep kelpie agents ").unwrap()
        ),
    )
    .unwrap();

    assert_eq!(said, ["wrote mine.md"]);
    let written = file(dir.path(), "mine");
    assert!(
        written.starts_with("---\nrole: reviewer\nharness: claude-code\n"),
        "{written}"
    );
    let book = Agents::load(dir.path()).unwrap();
    let prompt = book.get(&name("mine")).unwrap().prompt.clone();
    assert_eq!(prompt.as_deref(), Some("Read it for defects."));
}

#[test]
fn a_reviewer_with_no_file_still_needs_a_baseline_when_the_name_is_free() {
    let dir = tempfile::tempdir().unwrap();
    let line =
        "new reviewer --name mine --harness claude-code --model claude-opus-5-5 --effort high";

    let err = agents(dir.path(), line).unwrap_err();

    assert!(err.contains("--baseline"), "{err}");
    assert_eq!(files(dir.path()), Vec::<String>::new());
}

#[test]
fn migrate_prints_a_line_for_each_file_written_and_each_skipped_one() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pm.md"), "---\nextends: kelpie:pm\n---\n").unwrap();
    std::fs::write(
        dir.path().join("hunter.md"),
        "---\nextends: kelpie:defect-hunter\neffort: low\n---\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("codex-pm.md"),
        "---\nextends: kelpie:pm\nharness: codex\n---\n",
    )
    .unwrap();

    let said = agents(dir.path(), "migrate").unwrap();

    assert_eq!(said.len(), 3, "{said:?}");
    assert_eq!(
        said[0],
        "hunter.md: wrote claude-code claude-opus-5-5 second_look"
    );
    assert_eq!(said[1], "pm.md: wrote claude-code claude-opus-5-5 medium");
    assert!(said[2].starts_with("codex-pm.md: skipped, "), "{said:?}");
    assert!(said[2].contains("`model`"), "{said:?}");
    let book = Agents::load(dir.path()).unwrap();
    assert!(
        book.broken(&name("codex-pm")).is_some(),
        "the skipped file is left out of the book until it gets a hand"
    );
    assert!(book.broken(&name("pm")).is_none());
}

#[test]
fn migrate_says_there_is_nothing_to_migrate_on_a_second_run_and_in_a_folder_of_bound_files() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        agents(dir.path(), "migrate").unwrap(),
        ["nothing to migrate"]
    );
    std::fs::write(dir.path().join("pm.md"), "---\nextends: kelpie:pm\n---\n").unwrap();
    agents(dir.path(), "migrate").unwrap();

    let again = agents(dir.path(), "migrate").unwrap();

    assert_eq!(again, ["nothing to migrate"]);
    assert!(Agents::load(dir.path()).unwrap().get(&name("pm")).is_some());
}

#[test]
fn migrate_takes_no_argument() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(agents(dir.path(), "migrate now").unwrap_err(), USAGE);
}

#[test]
fn new_pm_on_codex_writes_nothing_and_names_the_harness_that_can_serve_it() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("agents");
    let err = agents(
        &folder,
        "new pm --harness codex --model gpt-5.5 --effort high",
    )
    .unwrap_err();
    assert!(err.contains("claude-code"), "{err}");
    assert!(
        err.contains("a Claude Code hook is what holds its writes to its notes"),
        "{err}"
    );
    assert!(!folder.exists());
}

#[test]
fn new_implementer_on_each_serving_harness_writes_a_file_that_loads() {
    assert_eq!(Harness::serving(Role::Implementer), Harness::ALL);
    for harness in Harness::serving(Role::Implementer) {
        let dir = tempfile::tempdir().unwrap();
        let server = if harness.facts().needs_model_server {
            " --url http://box:11434/v1 --context 65536"
        } else {
            ""
        };
        let line = format!(
            "new implementer --name mine --harness {} --model m --effort high{server}",
            harness.as_str()
        );
        assert_eq!(agents(dir.path(), &line).unwrap(), ["wrote mine.md"]);
        let book = Agents::load(dir.path()).unwrap();
        let agent = book.get(&name("mine")).unwrap();
        assert_eq!(agent.role, Role::Implementer);
        assert_eq!(agent.runs.session().unwrap().0.harness.harness(), harness);
    }
}

#[test]
fn new_checks_the_needs_kept_in_an_unbound_file_before_writing() {
    let dir = tempfile::tempdir().unwrap();
    let stub = "---\nrole: implementer\nneeds: [role-guard]\n---\nMy instructions.\n";
    std::fs::write(dir.path().join("mine.md"), stub).unwrap();
    let codex = "new implementer --name mine --harness codex --model m --effort high";

    let err = agents(dir.path(), codex).unwrap_err();

    assert!(err.contains("kelpie's PM guard"), "{err}");
    assert!(err.contains("claude-code"), "{err}");
    assert_eq!(file(dir.path(), "mine"), stub);
    agents(dir.path(), &codex.replace("codex", "claude-code")).unwrap();
    let book = Agents::load(dir.path()).unwrap();
    assert!(book.get(&name("mine")).is_some());
    assert!(file(dir.path(), "mine").contains("needs: [role-guard]"));
}

#[test]
fn new_honors_fewer_needs_when_binding_an_unbound_file() {
    let dir = tempfile::tempdir().unwrap();
    let stub = "---\nrole: implementer\nneeds: []\n---\nMy instructions.\n";
    std::fs::write(dir.path().join("mine.md"), stub).unwrap();

    agents(
        dir.path(),
        "new implementer --name mine --harness codex --model m --effort high",
    )
    .unwrap();

    let book = Agents::load(dir.path()).unwrap();
    let agent = book.get(&name("mine")).unwrap();
    assert_eq!(
        agent.runs.session().unwrap().0.harness.harness(),
        Harness::Codex
    );
    assert!(file(dir.path(), "mine").contains("needs: []"));
}

#[test]
fn new_refuses_to_bind_pm_without_its_role_guard_even_with_fewer_needs() {
    let role = "pm";
    for needs in ["[]", "[repo-reads]"] {
        let dir = tempfile::tempdir().unwrap();
        let stub = format!("---\nrole: {role}\nneeds: {needs}\n---\nMy instructions.\n");
        std::fs::write(dir.path().join(format!("{role}.md")), &stub).unwrap();
        for harness in ["codex", "pi", "goose"] {
            let err = agents(
                dir.path(),
                &format!("new {role} --harness {harness} --model m --effort high"),
            )
            .unwrap_err();
            assert!(err.contains("runs on claude-code"), "{err}");
            assert!(err.contains(&format!("not {harness}")), "{err}");
            assert_eq!(file(dir.path(), role), stub);
        }
        agents(
            dir.path(),
            &format!("new {role} --harness claude-code --model m --effort high"),
        )
        .unwrap();
        assert!(Agents::load(dir.path()).unwrap().get(&name(role)).is_some());
    }
}

#[test]
fn new_reports_a_missing_server_without_a_default_role_harness_hint() {
    let dir = tempfile::tempdir().unwrap();
    let stub = "---\nrole: implementer\nneeds: []\n---\nMy instructions.\n";
    std::fs::write(dir.path().join("mine.md"), stub).unwrap();

    let err = agents(
        dir.path(),
        "new implementer --name mine --harness goose --model m --effort high",
    )
    .unwrap_err();

    assert_eq!(
        err,
        "runs on goose, which needs the model's server as `url` or a `gateway`, \
         and its context size as `context`"
    );
    assert_eq!(file(dir.path(), "mine"), stub);
}

// The models file kelpie ships, which `agents init` reads.
const MODELS: &str = crate::agents::init::MODELS;

// A machine whose `PATH` holds exactly `programs`, which records each program
// it was asked about.
#[derive(Debug, Default)]
struct Machine {
    programs: Vec<&'static str>,
    asked: Mutex<Vec<String>>,
}

impl Machine {
    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

impl Host for Machine {
    fn on_path(&self, program: &str) -> bool {
        self.asked.lock().unwrap().push(program.to_owned());
        self.programs.contains(&program)
    }

    fn sandbox_gaps(&self) -> Vec<&'static str> {
        Vec::new()
    }

    fn has_keychain(&self) -> bool {
        false
    }

    fn os(&self) -> &'static str {
        "macos"
    }

    fn windows_status(&self) -> Result<WindowsInstall, String> {
        Ok(WindowsInstall::provisioned(false))
    }

    fn login_token(&self) -> bool {
        false
    }

    fn program_version(&self, program: &str) -> Result<String, String> {
        Err(format!("`{program}` is asked for by nothing here"))
    }
}

// `shep kelpie agents init <line>` on `folder`, reading `models` on `host` with
// these gateways.
fn init_on(
    folder: &Path,
    line: &str,
    models: &str,
    host: &dyn Host,
    gateways: Gateways,
) -> Result<Vec<String>, String> {
    let args: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
    init::run(
        folder,
        &args,
        &init::Setup {
            models,
            host,
            gateways,
        },
    )
}

// A models file with rows for claude-code, pi and codex, in `order`: the
// implementer, one reviewer baseline, the PM and the representative on claude-code,
// the implementer and the reviewer on pi, and the representative on codex.
fn lineup(order: &[&str], pi_model: &str) -> String {
    format!(
        "order = {order:?}\n\n\
         [claude-code.implementer]\nmodel = \"claude-sonnet-5-5\"\neffort = \"high\"\n\n\
         [claude-code.reviewer.defect-hunter]\nmodel = \"claude-opus-5-5\"\neffort = \"high\"\n\
         second_look = true\n\n\
         [claude-code.pm]\nmodel = \"claude-opus-5-5\"\neffort = \"medium\"\n\n\
         [claude-code.representative]\nmodel = \"claude-opus-5-5\"\neffort = \"high\"\n\n\
         [pi.implementer]\nmodel = \"{pi_model}\"\neffort = \"high\"\ncontext = 65536\n\n\
         [pi.reviewer.defect-hunter]\nmodel = \"{pi_model}\"\neffort = \"high\"\n\
         second_look = true\ncontext = 65536\n\n\
         [codex.representative]\nmodel = \"gpt-6.1-sol\"\neffort = \"medium\"\n"
    )
}

// Kelpie's settings holding one paddock gateway at `url`, with no key set here.
fn paddock(url: &str) -> Gateways {
    let gateway = Gateway {
        kind: GatewayKind::Paddock,
        url: EndpointUrl::try_from(url.to_owned()).unwrap(),
        key_env: KeyVar::try_from("PADDOCK_KEY".to_owned()).unwrap(),
    };
    Gateways::reading(
        BTreeMap::from([(
            GatewayName::try_from("paddock".to_owned()).unwrap(),
            gateway,
        )]),
        |_| None,
    )
}

const PI_IMPLEMENTER: &str = "---\nrole: implementer\nharness: pi\nmodel: qwen3.8:27b\n\
                             effort: high\ngateway: paddock\ncontext: 65536\n---\n";

#[test]
fn init_claude_code_binds_every_role_it_serves_and_the_files_load() {
    let dir = tempfile::tempdir().unwrap();

    let said = init_on(
        dir.path(),
        "claude-code",
        MODELS,
        &Machine::default(),
        Gateways::default(),
    )
    .unwrap();

    assert_eq!(
        files(dir.path()),
        [
            "defect-hunter.md",
            "implementer.md",
            "pm.md",
            "representative.md",
            "security-reviewer.md",
            "spec.md",
            "standards.md",
            "triager.md",
        ],
        "the implementer and every baseline of every role claude-code serves"
    );
    let has = |line: &str| assert!(said.iter().any(|l| l == line), "{line} in {said:#?}");
    has("implementer: wrote claude-code claude-sonnet-5-5 at high");
    has("defect-hunter: wrote claude-code claude-opus-5-5 at high");
    has("pm: wrote claude-code claude-opus-5-5 at medium");
    has("shep kelpie set agents.implementers '[\"implementer\"]'");
    has(
        "shep kelpie set agents.reviewers '[\"defect-hunter\", \"security-reviewer\", \"spec\", \"standards\"]'",
    );
    assert!(
        file(dir.path(), "implementer")
            .starts_with("---\nrole: implementer\nharness: claude-code\n"),
        "an implementer has no baseline, so its file names its role"
    );
    assert!(file(dir.path(), "pm").starts_with("---\nextends: kelpie:pm\nharness: claude-code\n"));
    assert!(file(dir.path(), "defect-hunter").contains("second_look: true"));

    let book = Agents::load(dir.path()).unwrap();
    assert_eq!(book.skipped().count(), 0);
    assert!(book.unbound().is_empty());
    for (role, file) in [
        ("implementer", "implementer"),
        ("reviewer", "defect-hunter"),
        ("reviewer", "standards"),
        ("reviewer", "spec"),
        ("reviewer", "security-reviewer"),
        ("pm", "pm"),
        ("representative", "representative"),
        ("triager", "triager"),
    ] {
        let agent = book
            .get(&name(file))
            .unwrap_or_else(|| panic!("`{file}.md` is not in the book"));
        assert_eq!(agent.role.as_str(), role, "{file}");
    }
    assert!(book.get(&name("defect-hunter")).unwrap().second_look);
    assert!(!book.get(&name("standards")).unwrap().second_look);
}

#[test]
fn init_keeps_a_file_that_exists_and_names_it() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pm.md"), UNBOUND_PM).unwrap();
    let before = file(dir.path(), "pm");

    let said = init_on(
        dir.path(),
        "claude-code",
        MODELS,
        &Machine::default(),
        Gateways::default(),
    )
    .unwrap();

    assert!(
        said.iter().any(|l| l == "pm: kept, it is already there"),
        "{said:#?}"
    );
    assert!(
        !said.iter().any(|l| l.starts_with("pm: wrote")),
        "{said:#?}"
    );
    assert_eq!(file(dir.path(), "pm"), before, "never over a file");
    assert!(
        dir.path().join("spec.md").is_file(),
        "the rest of the lineup is still written"
    );
}

#[test]
fn init_on_a_harness_with_no_rows_is_refused_naming_the_data_file() {
    let dir = tempfile::tempdir().unwrap();

    let err = init_on(
        dir.path(),
        "codex",
        MODELS,
        &Machine::default(),
        Gateways::default(),
    )
    .unwrap_err();

    assert!(err.contains("agents/models.toml"), "{err}");
    assert!(err.contains("codex"), "{err}");
    assert_eq!(files(dir.path()), Vec::<String>::new());
}

#[test]
fn init_pi_without_a_gateway_is_refused_naming_the_flag() {
    let dir = tempfile::tempdir().unwrap();
    let models = lineup(&["pi"], "qwen3.8:27b");

    let err = init_on(
        dir.path(),
        "pi",
        &models,
        &Machine::default(),
        Gateways::default(),
    )
    .unwrap_err();

    assert!(err.contains("--gateway"), "{err}");
    assert!(err.contains("--url"), "{err}");
    assert_eq!(files(dir.path()), Vec::<String>::new());

    init_on(
        dir.path(),
        "pi --gateway paddock",
        &models,
        &Machine::default(),
        Gateways::default(),
    )
    .unwrap();
    assert_eq!(file(dir.path(), "implementer"), PI_IMPLEMENTER);
    assert!(file(dir.path(), "defect-hunter").contains("gateway: paddock"));

    let both = init_on(
        dir.path(),
        "pi --gateway paddock --url http://box:11434/v1",
        &models,
        &Machine::default(),
        Gateways::default(),
    )
    .unwrap_err();
    assert!(both.contains("--gateway"), "{both}");
    assert_eq!(file(dir.path(), "implementer"), PI_IMPLEMENTER);

    let named = tempfile::tempdir().unwrap();
    init_on(
        named.path(),
        "pi --url http://box:11434/v1",
        &models,
        &Machine::default(),
        Gateways::default(),
    )
    .unwrap();
    assert!(
        file(named.path(), "implementer").contains("url: http://box:11434/v1\ncontext: 65536\n"),
        "{}",
        file(named.path(), "implementer")
    );
    let claude = init_on(
        named.path(),
        "claude-code --gateway paddock",
        &models,
        &Machine::default(),
        Gateways::default(),
    )
    .unwrap_err();
    assert!(claude.contains("--gateway"), "{claude}");
}

// `--detect` reads the gateways kelpie's own settings hold, so a server flag on it
// is refused rather than dropped.
#[test]
fn init_detect_refuses_a_server_flag_naming_it() {
    let dir = tempfile::tempdir().unwrap();

    for line in [
        "--detect --url http://box:11434/v1",
        "--detect --gateway paddock",
    ] {
        let err = init_on(
            dir.path(),
            line,
            MODELS,
            &Machine::default(),
            Gateways::default(),
        )
        .unwrap_err();
        assert!(err.contains("--gateway"), "{err}");
        assert!(err.contains("--url"), "{err}");
        assert!(err.contains("`--detect`"), "{err}");
    }
    assert_eq!(
        files(dir.path()),
        Vec::<String>::new(),
        "nothing is written"
    );
}

#[test]
fn detect_reads_the_path_and_configured_gateways_and_nothing_else() {
    let listed = StandInEndpoint::start([]).like_paddock("pk-right", &["qwen3.8:27b"]);
    let unconfigured = StandInEndpoint::start([]);
    let dir = tempfile::tempdir().unwrap();
    let host = Machine {
        programs: vec!["claude", "pi"],
        ..Machine::default()
    };

    let said = init_on(
        dir.path(),
        "--detect",
        &lineup(&["claude-code", "pi"], "qwen3.8:27b"),
        &host,
        paddock(listed.host()),
    )
    .unwrap();

    assert_eq!(
        host.asked(),
        ["claude", "pi", "goose", "codex"],
        "every CLI kelpie runs is looked for"
    );
    assert_eq!(
        listed.seen(),
        ["GET /v1/models HTTP/1.0"],
        "a configured gateway is asked only for its model list, as `paddock::request` writes it"
    );
    assert_eq!(
        unconfigured.seen(),
        Vec::<String>::new(),
        "nothing goes to an address kelpie's settings do not name"
    );
    let has = |line: &str| assert!(said.iter().any(|l| l == line), "{line} in {said:#?}");
    has("claude: on the PATH");
    has("pi: on the PATH");
    has("goose: not on the PATH");
    has("codex: not on the PATH");
    has("gateway paddock: lists 1 models");
}

#[test]
fn detect_binds_pi_only_where_a_gateway_lists_the_rows_model() {
    let listed = StandInEndpoint::start([]).like_paddock("pk-right", &["qwen3.8:27b"]);
    let other = StandInEndpoint::start([]).like_paddock("pk-right", &["llama"]);
    let models = lineup(&["pi", "claude-code"], "qwen3.8:27b");
    let host = Machine {
        programs: vec!["claude", "pi"],
        ..Machine::default()
    };
    let on = tempfile::tempdir().unwrap();
    let off = tempfile::tempdir().unwrap();

    init_on(
        on.path(),
        "--detect",
        &models,
        &host,
        paddock(listed.host()),
    )
    .unwrap();
    init_on(
        off.path(),
        "--detect",
        &models,
        &host,
        paddock(other.host()),
    )
    .unwrap();

    assert_eq!(file(on.path(), "implementer"), PI_IMPLEMENTER);
    assert_eq!(
        file(off.path(), "implementer"),
        "---\nrole: implementer\nharness: claude-code\nmodel: claude-sonnet-5-5\neffort: high\n---\n",
        "no gateway lists qwen3.8:27b, so claude-code gets the role"
    );
    assert!(
        file(on.path(), "defect-hunter").contains("harness: pi"),
        "the reviewer row is listed too"
    );
}

#[test]
fn detect_gives_each_role_the_first_harness_in_order_that_serves_it() {
    let listed = StandInEndpoint::start([]).like_paddock("pk-right", &["qwen3.8:27b"]);
    let dir = tempfile::tempdir().unwrap();
    let host = Machine {
        programs: vec!["claude", "pi", "codex"],
        ..Machine::default()
    };

    let said = init_on(
        dir.path(),
        "--detect",
        &lineup(&["pi", "codex", "claude-code"], "qwen3.8:27b"),
        &host,
        paddock(listed.host()),
    )
    .unwrap();

    let bound = |which: &str, harness: &str| {
        assert!(
            file(dir.path(), which).contains(&format!("harness: {harness}\n")),
            "{which} is not on {harness}: {said:#?}"
        );
    };
    bound("implementer", "pi");
    bound("defect-hunter", "pi");
    bound("representative", "codex");
    bound("pm", "claude-code");
    assert!(
        said.iter()
            .any(|l| l.starts_with("triager: none: pi has no row in agents/models.toml")),
        "{said:#?}"
    );
}

#[test]
fn detect_with_nothing_found_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();

    let said = init_on(
        dir.path(),
        "--detect",
        MODELS,
        &Machine::default(),
        Gateways::default(),
    )
    .unwrap();

    assert_eq!(
        said,
        [
            "nothing found: no harness kelpie runs is on the PATH, and kelpie's settings hold no gateway"
        ]
    );
    assert_eq!(files(dir.path()), Vec::<String>::new());
}
