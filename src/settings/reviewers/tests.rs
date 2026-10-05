use super::*;
use crate::agents::Agents;
use crate::settings::{Effort, Settings};

const EXAMPLE: &str = include_str!("../../../settings.example.toml");

/// The reviewers the example lists, commented out
const LISTED: &str = "# reviewers = [\"qwen\", \"defect-hunter\"]\n";

const OPUS: &str = "---\nrole: reviewer\nharness: claude-code\nmodel: claude-opus-5-5\n\
                    effort: high\npaths: [\"src/runner/merge/**\"]\n---\nRead it for defects.\n";

const GPU_BOX: &str = "---\nrole: reviewer\nharness: endpoint\nurl: http://gpu-box:11434/v1\n\
                       model: coder\ncontext: 32768\nlease: gpu-box\n---\n";

// Kelpie's own agents, with a Claude reviewer on some paths and an endpoint.
fn book() -> Agents {
    Agents::embedded()
        .with("opus", OPUS)
        .with("gpu-box", GPU_BOX)
}

// The example's project, listing `list` when it is given, and its home.
fn project(list: Option<&str>) -> (Settings, tempfile::TempDir) {
    assert!(EXAMPLE.contains(LISTED), "the example's reviewers moved");
    let listed = list.map_or(String::new(), |list| format!("reviewers = {list}\n"));
    let entry = EXAMPLE.replace(LISTED, &listed);
    let home = tempfile::tempdir().unwrap();
    let table = crate::test::project_table(&entry);
    let settings = Settings::from_table(&table, "shep", home.path(), Path::new("/p")).unwrap();
    (settings, home)
}

fn names(lineup: &[ListedReviewer]) -> Vec<&str> {
    lineup.iter().map(|r| r.name.as_str()).collect()
}

#[test]
fn a_project_lists_its_reviewers_in_the_order_the_review_runs_them_each_once() {
    let (settings, home) = project(Some(r#"["opus", "qwen", "gpu-box", "opus"]"#));
    let lineup = settings.lineup(&book(), home.path()).unwrap();
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
fn a_project_that_lists_none_runs_qwen_where_its_script_is_installed_then_defect_hunter() {
    let (settings, home) = project(None);
    let lineup = settings.lineup(&book(), home.path()).unwrap();
    assert_eq!(names(&lineup), ["defect-hunter"]);
    assert!(lineup[0].second_look);

    let script = home.path().join(".claude/scripts/qwen-review.sh");
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    std::fs::write(&script, "#!/bin/sh\n").unwrap();
    let lineup = settings.lineup(&book(), home.path()).unwrap();
    assert_eq!(names(&lineup), ["qwen", "defect-hunter"]);

    let (empty, home) = project(Some("[]"));
    assert!(empty.lineup(&book(), home.path()).unwrap().is_empty());
}

#[test]
fn a_reviewer_with_no_file_or_an_implementers_is_refused_naming_it() {
    let (settings, home) = project(Some(r#"["fable"]"#));
    let err = settings
        .lineup(&book(), home.path())
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "setting `agents`: `agents.reviewers` names fable, which has no agent file: \
         write `agents/fable.md` in kelpie's home"
    );
    let (settings, home) = project(Some(r#"["sonnet-high"]"#));
    let err = settings
        .lineup(&book(), home.path())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains(
            "`agents.reviewers` names sonnet-high, whose agent file's role is `implementer`"
        ),
        "{err}"
    );
}

#[test]
fn a_lease_name_is_lowercase_letters_digits_and_dashes() {
    assert!(LeaseName::try_from("gpu-box".to_owned()).is_ok());
    assert!(LeaseName::try_from("GPU box".to_owned()).is_err());
    assert!(LeaseName::gpu().is_gpu());
}

#[test]
fn a_commands_leading_tilde_is_the_home_folder_and_an_absolute_path_stays() {
    let command =
        |path: &str| format!("---\nrole: reviewer\nharness: command\ncommand: {path}\n---\n");
    let book = book()
        .with("mine", &command("~/bin/review"))
        .with("opt", &command("/opt/review"));
    let (settings, home) = project(Some(r#"["mine", "opt"]"#));
    let lineup = settings.lineup(&book, home.path()).unwrap();
    let paths: Vec<_> = (lineup.iter())
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
