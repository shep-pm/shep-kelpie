//! What a worker is told beyond kelpie's own instructions
//!
//! Kelpie's instructions are the same for every project. Two things vary:
//! the repo's pull request template, found in the worker's worktree, and the
//! project's own file of extra instructions, read once when the runner starts.

use std::fs;
use std::path::Path;

use crate::profile::INSTRUCTIONS;
use crate::settings::{Settings, SettingsError};

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

/// The instructions file for one worker turn
///
/// Kelpie's own, then a line about the pull request template if the worktree
/// has one, then the project's extra instructions.
pub(super) fn compose(extra: Option<&str>, worktree: &Path) -> String {
    let mut text = INSTRUCTIONS.to_owned();
    if let Some(template) = template(worktree) {
        text.push('\n');
        text.push_str(&template_line(&template));
    }
    if let Some(extra) = extra.filter(|e| !e.trim().is_empty()) {
        text.push_str("\n# This project's instructions\n\n");
        text.push_str(extra);
        if !extra.ends_with('\n') {
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
    use crate::test::{Rig, Scripted};

    const EXTRA: &str = "Never use an em dash in public writing.\n";

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
    fn a_repo_without_a_template_gets_no_template_line() {
        let rig = Rig::new("koji");
        let runner = rig.open().unwrap();
        assert_eq!(first_instructions(&rig, &runner), INSTRUCTIONS);
    }

    #[test]
    fn a_repo_with_a_template_tells_the_worker_to_fill_it() {
        let rig = Rig::new("koji");
        rig.land_on_origin("PULL_REQUEST_TEMPLATE.md");
        let runner = rig.open().unwrap();
        let text = first_instructions(&rig, &runner);
        assert!(text.starts_with(INSTRUCTIONS), "{text}");
        assert!(
            text.contains("pull request template, `PULL_REQUEST_TEMPLATE.md`"),
            "{text}"
        );
        assert!(text.contains("`Resolves #<issue>`"), "{text}");
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
        assert!(text.starts_with(INSTRUCTIONS), "{text}");
        assert!(text.ends_with(EXTRA), "{text}");
    }

    #[test]
    fn the_template_line_comes_before_the_projects_instructions() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("pull_request_template.md"), "## Summary\n").unwrap();
        let text = compose(Some(EXTRA), dir.path());
        let at = |needle: &str| text.find(needle).expect(needle);
        assert!(at("pull request template") < at("# This project's instructions"));
        assert!(text.ends_with(EXTRA), "{text}");
        assert_eq!(compose(Some(" \n"), dir.path()), compose(None, dir.path()));
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
        assert!(err.contains("projects/koji/gone.md"), "{err}");
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
