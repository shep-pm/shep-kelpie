use std::path::PathBuf;

use super::*;
use crate::agents::Agents;
use crate::settings::Settings;
use crate::webhook::KelpieSettings;

const EXAMPLE: &str = include_str!("../../../settings.example.toml");

const HOME: &str = "/home/me";

// The example's project listing `list`, over kelpie's `section`.
fn lineup(list: &str, section: &str) -> Result<Vec<LoopReviewer>, String> {
    let table = "[app.dogs.kelpie.review]\n";
    assert!(EXAMPLE.contains(table), "the example's review table moved");
    let entry = EXAMPLE.replace(table, &format!("{table}reviewers = {list}\n"));
    let table = crate::test::project_table(&entry);
    let settings = Settings::from_table(&table, "shep", Path::new(HOME), Path::new("/p"))
        .map_err(|e| e.to_string())?;
    let kelpie = KelpieSettings::from_section(section).map_err(|e| e.to_string())?;
    settings
        .lineup(&kelpie, &Agents::embedded(), Path::new(HOME))
        .map_err(|e| e.to_string())
}

fn names(lineup: &[LoopReviewer]) -> Vec<&str> {
    lineup.iter().map(|r| r.name.as_str()).collect()
}

const DEFINED: &str = "[local_reviewers.qwen]\nkind = \"command\"\n\
                       command = \"~/.claude/scripts/qwen-review.sh\"\n\
                       [local_reviewers.gpu-box]\nkind = \"endpoint\"\n\
                       url = \"http://gpu-box:11434/v1\"\nmodel = \"coder\"\n\
                       context = 32768\nlease = \"gpu-box\"\n\
                       [local_reviewers.opus]\nkind = \"claude\"\n\
                       model = \"claude-opus-5-5\"\neffort = \"high\"\n\
                       paths = [\"src/runner/merge/**\"]\n";

#[test]
fn a_project_lists_its_reviewers_in_the_order_the_review_runs_them() {
    let lineup = lineup(r#"["opus", "qwen", "claude", "gpu-box", "qwen"]"#, DEFINED).unwrap();
    assert_eq!(names(&lineup), ["opus", "qwen", "claude", "gpu-box"]);
    let Runs::Claude(opus) = &lineup[0].runs else {
        panic!("{:?}", lineup[0]);
    };
    assert_eq!(opus.model().model.as_str(), "claude-opus-5-5");
    assert_eq!(opus.model().effort, Effort::High);
    assert_eq!(lineup[0].paths()[0].as_str(), "src/runner/merge/**");
    let Runs::Local(LocalRound::Command(qwen)) = &lineup[1].runs else {
        panic!("{:?}", lineup[1]);
    };
    assert_eq!(
        qwen.command,
        PathBuf::from("/home/me/.claude/scripts/qwen-review.sh")
    );
    let Runs::Claude(claude) = &lineup[2].runs else {
        panic!("{:?}", lineup[2]);
    };
    assert_eq!(claude.model().model.as_str(), "claude-sonnet-5");
    let Runs::Local(gpu_box @ LocalRound::Endpoint(_)) = &lineup[3].runs else {
        panic!("{:?}", lineup[3]);
    };
    assert_eq!(gpu_box.lease().unwrap().as_str(), "gpu-box");
}

#[test]
fn no_list_is_the_local_round_and_then_the_deep_round() {
    let lineup = lineup("[]", DEFINED).unwrap();
    assert_eq!(names(&lineup), ["qwen", "deep"]);
    assert_eq!(lineup[1].runs, Runs::Deep);
    assert_eq!(
        lineup[0].runs,
        Runs::Local(LocalRound::default_at(Path::new(HOME))),
        "the maintainer's script, not kelpie's own `qwen`"
    );
}

#[test]
fn a_reviewer_kelpie_does_not_define_is_named() {
    let err = lineup(r#"["qwen", "sonnet"]"#, DEFINED).unwrap_err();
    assert_eq!(
        err,
        "setting `review.reviewers`: sonnet is not defined: kelpie's own settings \
         need a [local_reviewers.sonnet] table"
    );
}

#[test]
fn claude_is_each_project_s_own_and_cannot_be_defined() {
    let section = "[local_reviewers.claude]\nkind = \"claude\"\nmodel = \"m\"\neffort = \"low\"\n";
    let err = lineup(r#"["claude"]"#, section).unwrap_err();
    assert!(err.contains("`[local_reviewers.claude]` is taken"), "{err}");
}

#[test]
fn deep_is_each_project_s_own_deep_round_and_may_be_listed_but_not_defined() {
    let listed = lineup(r#"["qwen", "deep"]"#, DEFINED).unwrap();
    assert_eq!(names(&listed), ["qwen", "deep"]);
    assert_eq!(listed[1].runs, Runs::Deep);

    let section = "[local_reviewers.deep]\nkind = \"claude\"\nmodel = \"m\"\neffort = \"low\"\n";
    let err = lineup(r#"["deep"]"#, section).unwrap_err();
    assert!(err.contains("`[local_reviewers.deep]` is taken"), "{err}");
}

#[test]
fn the_older_local_round_cannot_be_set_beside_a_list() {
    let local = "[app.dogs.kelpie.review.local]\nkind = \"off\"\n";
    let entry = crate::test::with_tables(EXAMPLE, local).replace(
        "[app.dogs.kelpie.review]\n",
        "[app.dogs.kelpie.review]\nreviewers = [\"claude\"]\n",
    );
    let table = crate::test::project_table(&entry);
    let settings = Settings::from_table(&table, "shep", Path::new(HOME), Path::new("/p")).unwrap();
    let err = settings
        .lineup(
            &KelpieSettings::default(),
            &Agents::embedded(),
            Path::new(HOME),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("cannot both be set"), "{err}");
}

#[test]
fn a_definition_that_cannot_work_is_named() {
    let relative = "[local_reviewers.mine]\nkind = \"command\"\ncommand = \"review.sh\"\n";
    let err = lineup(r#"["mine"]"#, relative).unwrap_err();
    assert!(
        err.contains("`local_reviewers.mine.command` must start with `/` or `~/`"),
        "{err}"
    );
    let unleased = "[local_reviewers.mine]\nkind = \"command\"\ncommand = \"/opt/review\"\n\
                    ollama = \"http://h:1\"\n";
    let err = lineup(r#"["mine"]"#, unleased).unwrap_err();
    assert!(
        err.contains("`local_reviewers.mine.ollama` needs a lease"),
        "{err}"
    );
    let err = lineup(r#"["mine"]"#, "[local_reviewers.mine]\nkind = \"claude\"\n").unwrap_err();
    assert!(err.contains("line 1 is not right"), "{err}");
}

#[test]
fn a_name_is_lowercase_letters_digits_and_dashes() {
    for good in ["qwen", "gpu-box", "opus5"] {
        assert!(ReviewerName::try_from(good.to_owned()).is_ok(), "{good}");
    }
    for bad in ["", "Qwen", "gpu box", "a_b"] {
        assert!(ReviewerName::try_from(bad.to_owned()).is_err(), "{bad:?}");
    }
    let err = lineup(r#"["Qwen"]"#, "").unwrap_err();
    assert!(err.contains("must be lowercase letters"), "{err}");
}
