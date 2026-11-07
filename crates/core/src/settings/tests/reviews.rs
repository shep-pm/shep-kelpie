//! The review count keys, refused by name now that the project manager's pick
//! sets each work item's review plan

use super::{EXAMPLE, parse, parse_err};

// The words every review key is refused with
const BECAUSE: &str = "the project manager sets each work item's review plan at the pick, and \
                      kelpie weighs a diff only to report what it touches";

// The example with `keys` written into a `[app.dogs.kelpie.reviews]` table
fn reviews(keys: &str) -> String {
    format!("{EXAMPLE}\n[app.dogs.kelpie.reviews]\n{keys}\n")
}

#[test]
fn each_review_key_is_refused_by_name() {
    for (key, written) in [
        ("need", "need = 3"),
        ("light", "light = 1"),
        ("light_paths", "light_paths = [\"docs/**\"]"),
        ("full_paths", "full_paths = [\"legal/**\"]"),
        ("pm_lightens", "pm_lightens = true"),
        ("light_max_lines", "light_max_lines = 25"),
    ] {
        let err = parse_err(&reviews(written));
        assert_eq!(
            err,
            format!(
                "the [app.dogs.kelpie] table on shep: `reviews.{key}` is no longer a \
                 setting, because {BECAUSE}: delete it"
            ),
            "{written}"
        );
    }
}

#[test]
fn an_empty_reviews_table_is_refused_by_name() {
    let err = parse_err(&reviews(""));
    assert_eq!(
        err,
        format!(
            "the [app.dogs.kelpie] table on shep: `reviews` is no longer a setting, \
             because {BECAUSE}: delete it"
        )
    );
    assert!(!err.contains("unknown field"), "{err}");
}

#[test]
fn a_refusal_says_the_project_manager_sets_the_plan() {
    let err = parse_err(&reviews("need = 2\nlight = 1\npm_lightens = true\n"));
    assert_eq!(
        err,
        format!(
            "the [app.dogs.kelpie] table on shep: these are no longer settings:\n\
             - `reviews.need`, because {BECAUSE}: delete it\n\
             - `reviews.light`, because {BECAUSE}: delete it\n\
             - `reviews.pm_lightens`, because {BECAUSE}: delete it"
        )
    );
    assert!(parse(EXAMPLE).is_ok(), "the example sets no review key");
}
