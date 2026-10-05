//! What a worker is told beyond kelpie's own instructions
//!
//! Kelpie's instructions are the same for every project. Three things vary:
//! the repo's pull request template, found in the worker's worktree, the
//! body of the work item's agent file, and the project's own file of extra
//! instructions, read once when the runner starts.

use std::fs;
use std::path::Path;

use crate::profile;
use crate::settings::{Settings, SettingsError};
use crate::skills::{Skills, Step};

/// Reads the project's extra worker instructions, if its settings name a file
///
/// # Errors
///
/// [`SettingsError::Invalid`] naming `worker.instructions_file` and the file
/// when it cannot be read.
pub(super) fn read_extra(settings: &Settings) -> Result<Option<String>, SettingsError> {
    let Some(file) = &settings.worker.instructions_file else {
        return Ok(None);
    };
    fs::read_to_string(file)
        .map(Some)
        .map_err(|e| SettingsError::Invalid {
            setting: "worker.instructions_file",
            reason: format!("cannot read {}: {}", file.display(), e.kind()),
        })
}

/// What a worker is told after kelpie's own instructions
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Extra<'a> {
    /// The body of the work item's agent file
    pub(super) agent: Option<&'a str>,
    /// The project's file of extra instructions
    pub(super) project: Option<&'a str>,
}

/// The instructions file for one worker turn
///
/// Kelpie's own, naming the `kelpie` binary, then the skills the worker
/// writes tests and its pull request's body with, then the agent's
/// instructions, then the project's. A pull request template in the
/// worktree stands in for the body's skill.
pub(super) fn compose(extra: Extra<'_>, worktree: &Path, skills: &Skills, kelpie: &Path) -> String {
    let mut text = profile::instructions(kelpie);
    let mut lines = Vec::new();
    if let Some(tdd) = skills.command(Step::Tests) {
        lines.push(format!(
            "Write your tests the way the `{tdd}` skill says.\n"
        ));
    }
    match (template(worktree), skills.command(Step::Pr)) {
        (Some(template), _) => lines.push(template_line(&template)),
        (None, Some(pr)) => lines.push(format!(
            "Write your pull request's body with the `{pr}` skill, and still end it with \
             `Resolves #<issue>`.\n"
        )),
        (None, None) => {}
    }
    if !lines.is_empty() {
        text.push('\n');
        text.push_str(&lines.concat());
    }
    if let Some(rules) = skills.worker_rules() {
        text.push('\n');
        text.push_str(&rules);
    }
    let sections = [
        ("This agent's instructions", extra.agent),
        ("This project's instructions", extra.project),
    ];
    for (heading, section) in sections {
        let Some(section) = section.filter(|e| !e.trim().is_empty()) else {
            continue;
        };
        text.push_str(&format!("\n# {heading}\n\n"));
        text.push_str(section);
        if !section.ends_with('\n') {
            text.push('\n');
        }
    }
    text
}

fn template_line(template: &Template) -> String {
    match template {
        Template::File(path) => format!(
            "The repo has a pull request template, `{path}`. Fill it in for your pull request's \
             body instead of writing your own, and still end the body with `Resolves #<issue>`.\n"
        ),
        Template::Folder(path) => format!(
            "The repo has pull request templates in `{path}`. Fill in the one that fits for your \
             pull request's body instead of writing your own, and still end the body with \
             `Resolves #<issue>`.\n"
        ),
    }
}

/// A pull request template, as a path from the worktree's root
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Template {
    File(String),
    Folder(String),
}

/// The template GitHub would offer for a pull request from this worktree
///
/// GitHub reads `pull_request_template.md` (or `.txt`) from the root, `.github/` or
/// `docs/`, in any case, and a `PULL_REQUEST_TEMPLATE` folder of several from
/// the same places.
fn template(worktree: &Path) -> Option<Template> {
    for place in ["", ".github", "docs"] {
        let Ok(entries) = fs::read_dir(worktree.join(place)) else {
            continue;
        };
        let found = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
                let path = if place.is_empty() {
                    name.clone()
                } else {
                    format!("{place}/{name}")
                };
                match name.to_lowercase().as_str() {
                    "pull_request_template.md" | "pull_request_template.txt" if !is_dir => {
                        Some(Template::File(path))
                    }
                    "pull_request_template" if is_dir => Some(Template::Folder(path)),
                    _ => None,
                }
            })
            .min();
        if found.is_some() {
            return found;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::ports::{Cost, Usage};
    use crate::runner::Runner;
    use crate::runner::turn::step;
    use crate::settings::StepSkills;
    use crate::test::{Rig, Scripted};

    const EXTRA: &str = "Never use an em dash in public writing.\n";

    // Kelpie's own instructions, naming the binary the rig says kelpie is
    fn kelpies() -> String {
        profile::instructions(Path::new(Rig::KELPIE))
    }

    // The instructions file the worker's first turn was started with
    fn first_instructions(rig: &Rig, runner: &Mutex<Runner>) -> String {
        rig.ask(runner, "start", None);
        rig.ask(runner, "add", Some("7"));
        let usage = Usage {
            input: 1,
            cache_write: 10,
            cache_read: 100,
            output: 1000,
        };
        rig.claude.script([Scripted::Reply(usage, Cost(1))]);
        step(runner).unwrap();
        let [seen] = rig.claude.seen().try_into().unwrap();
        let file = seen.call.instructions.expect("the worker has instructions");
        fs::read_to_string(file).unwrap()
    }

    #[test]
    fn a_repo_without_a_template_has_the_body_written_with_the_pr_skill() {
        let rig = Rig::new("koji");
        let runner = rig.open().unwrap();
        let text = first_instructions(&rig, &runner);
        let lines = text.strip_prefix(&kelpies()).expect(&text);
        let (named, rules) = lines.split_once("\n\n").expect(lines);
        assert_eq!(
            named,
            "\nWrite your tests the way the `/mattpocock:tdd` skill says.\n\
             Write your pull request's body with the `/mattpocock:pr` skill, and still end \
             it with `Resolves #<issue>`."
        );
        assert!(
            rules.starts_with("Kelpie runs this skill headless."),
            "{rules}"
        );
        assert!(rules.contains("Run no /code-review"), "{rules}");
    }

    #[test]
    fn the_worker_is_told_to_run_its_tests_under_the_lease_with_kelpie_itself() {
        let rig = Rig::new("koji");
        let runner = rig.open().unwrap();
        let text = first_instructions(&rig, &runner);
        assert!(
            text.contains("`/opt/kelpie/bin/kelpie lease run cargo-test -- cargo test`"),
            "{text}"
        );
        assert!(!text.contains("{kelpie}"), "{text}");
    }

    #[test]
    fn a_repo_with_a_template_tells_the_worker_to_fill_it() {
        let rig = Rig::new("koji");
        rig.land_on_origin("PULL_REQUEST_TEMPLATE.md");
        let runner = rig.open().unwrap();
        let text = first_instructions(&rig, &runner);
        assert!(text.starts_with(&kelpies()), "{text}");
        assert!(
            text.contains("pull request template, `PULL_REQUEST_TEMPLATE.md`"),
            "{text}"
        );
        assert!(text.contains("`Resolves #<issue>`"), "{text}");
        assert!(!text.contains("/mattpocock:pr"), "{text}");
    }

    #[test]
    fn a_project_file_reaches_the_workers_first_turn() {
        let rig = Rig::new("koji");
        let file = rig.home.path().join("extra.md");
        fs::write(&file, EXTRA).unwrap();
        let line = format!(
            "build_env = {{}}\ninstructions_file = \"{}\"\n",
            file.display()
        );
        rig.edit_settings(|s| s.replace("build_env = {}\n", &line));
        let runner = rig.open().unwrap();
        let text = first_instructions(&rig, &runner);
        assert!(text.starts_with(&kelpies()), "{text}");
        assert!(text.ends_with(EXTRA), "{text}");
    }

    #[test]
    fn the_template_line_comes_before_the_projects_instructions() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("pull_request_template.md"), "## Summary\n").unwrap();
        let skills = Skills::load(&StepSkills::default(), &dir.path().join("skills"));
        let extra = Extra {
            agent: Some("Mind the migrations."),
            project: Some(EXTRA),
        };
        let text = compose(extra, dir.path(), &skills, Path::new(Rig::KELPIE));
        let at = |needle: &str| text.find(needle).expect(needle);
        assert!(at("/mattpocock:tdd") < at("pull request template"));
        assert!(at("pull request template") < at("# This agent's instructions"));
        assert!(
            text.contains("# This agent's instructions\n\nMind the migrations.\n"),
            "{text}"
        );
        assert!(at("Mind the migrations.") < at("# This project's instructions"));
        assert!(text.ends_with(EXTRA), "{text}");
        let blank = Extra {
            agent: Some("\n"),
            project: Some(" \n"),
        };
        let (blank, none) = (
            compose(blank, dir.path(), &skills, Path::new(Rig::KELPIE)),
            compose(
                Extra::default(),
                dir.path(),
                &skills,
                Path::new(Rig::KELPIE),
            ),
        );
        assert_eq!(blank, none);
    }

    #[test]
    fn an_agent_files_body_reaches_its_workers_turn() {
        let rig = Rig::new("koji");
        rig.write_agent(
            "sonnet-high",
            "---\nrole: implementer\nharness: claude-code\nmodel: claude-sonnet-5-5\n\
             effort: high\n---\n\nKeep each commit small.\n",
        );
        let runner = rig.open().unwrap();
        let text = first_instructions(&rig, &runner);
        assert!(text.starts_with(&kelpies()), "{text}");
        assert!(
            text.ends_with("\n# This agent's instructions\n\nKeep each commit small.\n"),
            "{text}"
        );
    }

    #[test]
    fn a_missing_instructions_file_stops_the_runner() {
        let rig = Rig::new("koji");
        let line = "build_env = {}\ninstructions_file = \"gone.md\"\n";
        rig.edit_settings(|s| s.replace("build_env = {}\n", line));
        let err = rig.open().unwrap_err().to_string();
        assert!(
            err.starts_with("setting `worker.instructions_file`: cannot read "),
            "{err}"
        );
        assert!(err.contains("kelpie/koji/gone.md"), "{err}");
    }

    #[test]
    fn github_reads_a_template_from_three_places_in_any_case() {
        for file in [
            "Pull_Request_Template.md",
            ".github/pull_request_template.md",
            "docs/PULL_REQUEST_TEMPLATE.txt",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "## Summary\n").unwrap();
            assert_eq!(template(dir.path()), Some(Template::File(file.into())));
        }
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".github/PULL_REQUEST_TEMPLATE")).unwrap();
        assert_eq!(
            template(dir.path()),
            Some(Template::Folder(".github/PULL_REQUEST_TEMPLATE".into()))
        );
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(template(empty.path()), None);
    }
}
