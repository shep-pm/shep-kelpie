//! The one-time step that writes each stub the binding its baseline shipped with

use super::{folder, name};
use crate::agents::legacy::{self, Report};
use crate::agents::{Agents, baseline};

// Each baseline's binding as `git show a22137eb:agents/<name>.md` has it, before the
// baselines dropped their models: (name, harness, model, effort, second_look).
const SHIPPED: [(&str, &str, &str, &str, bool); 7] = [
    (
        "defect-hunter",
        "claude-code",
        "claude-opus-5-5",
        "high",
        true,
    ),
    (
        "security-reviewer",
        "claude-code",
        "claude-opus-5-5",
        "high",
        false,
    ),
    ("spec", "claude-code", "claude-opus-5-5", "high", false),
    ("standards", "claude-code", "claude-opus-5-5", "high", false),
    ("pm", "claude-code", "claude-opus-5-5", "medium", false),
    (
        "representative",
        "claude-code",
        "claude-opus-5-5",
        "low",
        false,
    ),
    ("triager", "claude-code", "claude-opus-5-5", "medium", false),
];

// The binding lines a baseline shipped with.
fn shipped_binding(row: &(&str, &str, &str, &str, bool)) -> String {
    let (_, harness, model, effort, second_look) = row;
    let look = if *second_look {
        "second_look: true\n"
    } else {
        ""
    };
    format!("harness: {harness}\nmodel: {model}\neffort: {effort}\n{look}")
}

fn stub(baseline: &str) -> String {
    format!("---\nextends: kelpie:{baseline}\n---\n")
}

fn read(dir: &tempfile::TempDir, file: &str) -> String {
    std::fs::read_to_string(dir.path().join(file)).unwrap()
}

fn wrote(report: &Report) -> Vec<&str> {
    report.wrote.iter().map(|w| w.file.as_str()).collect()
}

#[test]
fn the_table_matches_the_files_as_they_shipped() {
    assert_eq!(legacy::TABLE.len(), SHIPPED.len());
    for (name, harness, model, effort, second_look) in SHIPPED {
        let row = legacy::named(name).unwrap_or_else(|| panic!("{name} has no row"));
        assert_eq!(
            (row.harness, row.model, row.effort, row.second_look),
            (harness, model, effort, second_look),
            "{name}"
        );
    }
    for shipped in baseline::ALL {
        assert!(legacy::named(shipped.name).is_some(), "{}", shipped.name);
    }
}

#[test]
fn a_bare_stub_gets_the_binding_and_then_loads_as_it_did_before() {
    for row in &SHIPPED {
        let baseline = row.0;
        let dir = folder(&[("mine.md", &stub(baseline))]);

        let report = legacy::migrate(dir.path()).unwrap();

        assert_eq!(wrote(&report), ["mine.md"], "{baseline}");
        let text = read(&dir, "mine.md");
        assert_eq!(
            text,
            format!(
                "---\nextends: kelpie:{baseline}\n{}---\n",
                shipped_binding(row)
            ),
            "{baseline}"
        );
        let loaded = Agents::load(dir.path()).unwrap();
        let before = Agents::embedded().bind(baseline, &shipped_binding(row));
        assert_eq!(
            loaded.get(&name("mine")),
            before.get(&name(baseline)),
            "{baseline}"
        );
    }
}

#[test]
fn a_stub_keeps_its_comments_other_keys_and_body() {
    let text = "---\n# mine, for the nightly run\nextends: kelpie:spec\n\
                usage: none # no meter\n\n# last\n---\nAlso read the changelog.\n";
    let dir = folder(&[("nightly.md", text)]);

    legacy::migrate(dir.path()).unwrap();

    assert_eq!(
        read(&dir, "nightly.md"),
        "---\n# mine, for the nightly run\nextends: kelpie:spec\n\
         harness: claude-code\nmodel: claude-opus-5-5\neffort: high\n\
         usage: none # no meter\n\n# last\n---\nAlso read the changelog.\n"
    );
}

#[test]
fn a_stub_that_sets_its_own_effort_keeps_it() {
    let text = "---\nextends: kelpie:pm\neffort: low\n---\n";
    let dir = folder(&[("quick-pm.md", text)]);

    let report = legacy::migrate(dir.path()).unwrap();

    assert_eq!(
        read(&dir, "quick-pm.md"),
        "---\nextends: kelpie:pm\nharness: claude-code\nmodel: claude-opus-5-5\neffort: low\n---\n"
    );
    assert_eq!(
        report.wrote[0].to_string(),
        "quick-pm.md: wrote claude-code claude-opus-5-5"
    );
}

#[test]
fn a_stub_with_a_harness_and_no_model_is_skipped_and_named() {
    let text = "---\nextends: kelpie:pm\nharness: codex\neffort: low\n---\n";
    let dir = folder(&[("codex-pm.md", text)]);

    let report = legacy::migrate(dir.path()).unwrap();

    assert!(report.wrote.is_empty());
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].file, "codex-pm.md");
    assert!(
        report.skipped[0].why.contains("`harness`"),
        "{}",
        report.skipped[0].why
    );
    assert!(
        report.skipped[0].why.contains("`model`"),
        "{}",
        report.skipped[0].why
    );
    assert_eq!(read(&dir, "codex-pm.md"), text, "it is left alone");
}

#[test]
fn a_stub_on_the_baselines_own_harness_gets_the_model_and_effort() {
    let text = "---\nextends: kelpie:pm\nharness: claude-code\n---\n";
    let dir = folder(&[("pm.md", text)]);

    let report = legacy::migrate(dir.path()).unwrap();

    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert_eq!(wrote(&report), ["pm.md"]);
    assert_eq!(
        read(&dir, "pm.md"),
        "---\nextends: kelpie:pm\nmodel: claude-opus-5-5\neffort: medium\nharness: claude-code\n---\n"
    );
    assert!(Agents::load(dir.path()).is_ok());
}

#[test]
fn a_file_on_a_harness_with_keys_of_its_own_is_left_alone() {
    let endpoint = "---\nextends: kelpie:defect-hunter\nharness: endpoint\nurl: http://box:11434/v1\n\
                    model: coder\ncontext: 32768\n---\n";
    let command = "---\nextends: kelpie:standards\nharness: command\ncommand: /bin/true\n---\n";
    let dir = folder(&[("local.md", endpoint), ("script.md", command)]);

    let report = legacy::migrate(dir.path()).unwrap();

    assert_eq!(report, Report::default());
    assert_eq!(read(&dir, "local.md"), endpoint);
    assert_eq!(read(&dir, "script.md"), command);
}

#[cfg(unix)]
#[test]
fn a_symlinked_stub_is_written_through_and_stays_a_link() {
    let dir = folder(&[("elsewhere.txt", &stub("pm"))]);
    let agents = dir.path().join("agents");
    std::fs::create_dir(&agents).unwrap();
    std::os::unix::fs::symlink(dir.path().join("elsewhere.txt"), agents.join("pm.md")).unwrap();

    let report = legacy::migrate(&agents).unwrap();

    assert_eq!(wrote(&report), ["pm.md"]);
    assert!(agents.join("pm.md").is_symlink());
    assert!(read(&dir, "elsewhere.txt").contains("model: claude-opus-5-5"));
    assert!(legacy::pending(&agents).unwrap().wrote.is_empty());
}

#[test]
fn a_second_run_writes_nothing() {
    let skipped = "---\nextends: kelpie:pm\nharness: codex\n---\n";
    let dir = folder(&[
        ("a.md", &stub("pm")),
        ("b.md", &stub("defect-hunter")),
        ("c.md", skipped),
    ]);
    assert_eq!(
        wrote(&legacy::migrate(dir.path()).unwrap()),
        ["a.md", "b.md"]
    );
    let once = (read(&dir, "a.md"), read(&dir, "b.md"));

    let again = legacy::migrate(dir.path()).unwrap();

    assert!(again.wrote.is_empty());
    assert_eq!((read(&dir, "a.md"), read(&dir, "b.md")), once);
    assert!(
        std::fs::read_dir(dir.path()).unwrap().all(|f| {
            let name = f.unwrap().file_name();
            name.to_string_lossy().ends_with(".md")
        }),
        "no temporary file is left"
    );
}

#[test]
fn a_file_extending_a_stub_loads_bound() {
    let dir = folder(&[
        ("base.md", &stub("spec")),
        ("child.md", "---\nextends: base\neffort: low\n---\nShort.\n"),
    ]);
    assert!(
        Agents::load(dir.path())
            .unwrap()
            .get(&name("child"))
            .is_none()
    );

    let report = legacy::migrate(dir.path()).unwrap();

    assert_eq!(wrote(&report), ["base.md"], "the child needs nothing");
    let child = Agents::load(dir.path()).unwrap();
    let child = child.get(&name("child")).unwrap();
    assert_eq!(
        super::pair(child),
        ("claude-opus-5-5", crate::settings::Effort::Low)
    );
}

#[test]
fn a_file_with_no_legacy_parent_is_left_alone() {
    let own = "---\nrole: implementer\nharness: claude-code\nmodel: m\neffort: low\n---\n";
    let whole = "---\nextends: kelpie:codex\n---\n";
    let bare = "---\nextends: pm\n---\n";
    let dir = folder(&[
        ("own.md", own),
        ("whole.md", whole),
        ("bare.md", bare),
        ("n.txt", "x"),
    ]);

    let report = legacy::migrate(dir.path()).unwrap();

    assert_eq!(report, Report::default());
    assert_eq!(read(&dir, "whole.md"), whole);
}

// `opus-high` as kelpie shipped it, which a bare stub of it is written out as.
const FORMER_OPUS_HIGH: &str = r#"---
# Opus 5.5 at high effort, for work that is hard to undo. The agent board
# picks it for heavy work, and an `agent:opus-high!` label on an issue pins
# it there. Anything below the closing line is added to kelpie's own
# instructions for this agent; it adds nothing here.
role: implementer
harness: claude-code
model: claude-opus-5-5
effort: high
description: Builds work that is hard to undo, such as a data migration, a release step or a change to a public interface.
tier: heavy
---
"#;

#[test]
fn a_bare_stub_of_an_agent_kelpie_no_longer_ships_is_written_out_once() {
    let dir = folder(&[("opus-high.md", &stub("opus-high"))]);

    let report = legacy::migrate(dir.path()).unwrap();

    assert_eq!(wrote(&report), ["opus-high.md"]);
    assert_eq!(read(&dir, "opus-high.md"), FORMER_OPUS_HIGH);
    let agents = Agents::load(dir.path()).unwrap();
    let agent = agents.get(&name("opus-high")).unwrap();
    assert_eq!(
        super::pair(agent),
        ("claude-opus-5-5", crate::settings::Effort::High)
    );
    assert_eq!(legacy::migrate(dir.path()).unwrap(), Report::default());
}

#[test]
fn a_stub_of_a_removed_agent_with_keys_of_its_own_keeps_them() {
    let mine = "---\nextends: kelpie:sonnet-high\neffort: low\n---\n";
    let dir = folder(&[("mine.md", mine)]);

    legacy::migrate(dir.path()).unwrap();

    let agents = Agents::load(dir.path()).unwrap();
    let agent = agents.get(&name("mine")).unwrap();
    assert_eq!(
        super::pair(agent),
        ("claude-sonnet-5-5", crate::settings::Effort::Low)
    );
}

#[test]
fn a_folder_that_is_not_there_has_nothing_to_migrate() {
    let dir = tempfile::tempdir().unwrap();
    let report = legacy::migrate(&dir.path().join("agents")).unwrap();
    assert_eq!(report, Report::default());
}

#[test]
fn a_report_lists_what_it_would_write_without_writing_it() {
    let dir = folder(&[("a.md", &stub("pm"))]);
    let report = legacy::pending(dir.path()).unwrap();
    assert_eq!(wrote(&report), ["a.md"]);
    assert_eq!(read(&dir, "a.md"), stub("pm"));
}

#[test]
fn a_bare_stub_of_a_role_the_panel_took_is_deleted_and_named() {
    let bound = "---\nextends: kelpie:advisor\nharness: claude-code\nmodel: claude-opus-5-5\n\
                 effort: high\n---\n";
    let dir = folder(&[
        ("advisor.md", bound),
        ("researcher.md", &stub("researcher")),
    ]);

    let report = legacy::migrate(dir.path()).unwrap();

    assert_eq!(
        report.lines(),
        [
            "advisor.md: deleted, its role went with the ruling panel",
            "researcher.md: deleted, its role went with the ruling panel",
        ]
    );
    assert!(!dir.path().join("advisor.md").exists());
    assert!(!dir.path().join("researcher.md").exists());
    assert!(legacy::migrate(dir.path()).unwrap().lines().is_empty());
}

#[test]
fn a_file_of_a_role_the_panel_took_that_holds_more_is_left_and_named() {
    let body = "---\nextends: kelpie:advisor\nmodel: m\n---\nAlso read the changelog.\n";
    let keys = "---\nextends: kelpie:researcher\nusage: none\n---\n";
    let dir = folder(&[("mine.md", body), ("nightly.md", keys)]);

    let report = legacy::migrate(dir.path()).unwrap();

    let left: Vec<&str> = report.left.iter().map(|l| l.file.as_str()).collect();
    assert_eq!(left, ["mine.md", "nightly.md"]);
    assert!(
        report.lines()[0].starts_with("mine.md: left, "),
        "{:?}",
        report.lines()
    );
    assert!(
        report.left[0]
            .why
            .contains("the `advisor` role went with the ruling panel, so delete the file"),
        "{:?}",
        report.left
    );
    assert_eq!(read(&dir, "mine.md"), body);
    assert_eq!(read(&dir, "nightly.md"), keys);
}

#[test]
fn a_hand_made_file_of_a_role_the_panel_took_is_left_and_named() {
    let advisor = "---\nrole: advisor\nharness: claude-code\nmodel: m\neffort: low\n---\nVote.\n";
    let researcher = "---\nrole: researcher\n---\n";
    let dir = folder(&[("mine.md", advisor), ("web.md", researcher)]);

    let report = legacy::migrate(dir.path()).unwrap();

    assert_eq!(
        report.lines(),
        [
            "mine.md: left, the `advisor` role went with the ruling panel, so delete the file",
            "web.md: left, the `researcher` role went with the ruling panel, so delete the file",
        ]
    );
    assert!(report.deleted.is_empty(), "{report:?}");
    assert_eq!(read(&dir, "mine.md"), advisor);
    assert_eq!(read(&dir, "web.md"), researcher);
}

#[test]
fn a_bare_stub_of_the_issue_writer_is_deleted_and_any_other_file_of_it_left_and_named() {
    let bound = "---\nextends: kelpie:issue-writer\nharness: claude-code\n\
                 model: claude-opus-5-5\neffort: medium\n---\n";
    let more = "---\nextends: kelpie:issue-writer\nmodel: m\n---\nFile it small.\n";
    let own = "---\nrole: issue-writer\nharness: claude-code\nmodel: m\neffort: low\n---\nWrite.\n";
    let dir = folder(&[
        ("issue-writer.md", bound),
        ("mine.md", more),
        ("scribe.md", own),
    ]);

    let report = legacy::migrate(dir.path()).unwrap();

    let went = "went with the issue writer, and an issue is written with kelpie's `issue` skill";
    let lines = report.lines();
    assert_eq!(
        lines[0],
        format!("issue-writer.md: deleted, its role {went}")
    );
    assert!(
        lines[1].starts_with("mine.md: left, the `issue-writer` role "),
        "{lines:?}"
    );
    assert!(lines[1].contains("holds more than a binding"), "{lines:?}");
    assert_eq!(
        lines[2],
        format!("scribe.md: left, the `issue-writer` role {went}, so delete the file")
    );
    assert!(!dir.path().join("issue-writer.md").exists());
    assert_eq!(read(&dir, "mine.md"), more);
    assert_eq!(read(&dir, "scribe.md"), own);
}

#[test]
fn a_file_written_out_from_a_bare_stub_is_the_agent_kelpie_once_shipped_and_an_edit_is_not() {
    let dir = folder(&[
        ("iq3-xxs-review.md", &stub("iq3-xxs-review")),
        ("opus-high.md", &stub("opus-high")),
    ]);
    legacy::migrate(dir.path()).unwrap();
    let agents = Agents::load(dir.path()).unwrap();
    let once = |n: &str| legacy::is_once_shipped(&name(n), agents.get(&name(n)).unwrap());
    assert!(once("iq3-xxs-review") && once("opus-high"));

    // A file of the same name that says more, or one kelpie never shipped under that name, is not.
    let edited = FORMER_OPUS_HIGH.replace("effort: high", "effort: low");
    let agents = agents.with("opus-high", &edited);
    assert!(!legacy::is_once_shipped(
        &name("opus-high"),
        agents.get(&name("opus-high")).unwrap()
    ));
    let mine = agents.with("mine", &edited);
    assert!(!legacy::is_once_shipped(
        &name("mine"),
        mine.get(&name("mine")).unwrap()
    ));
}
