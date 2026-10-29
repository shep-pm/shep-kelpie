//! A pass runs `reviews.need` reviewers, each round the agent board's pick

use super::several::{at_review, reviewer_models, reviewers_until_ci, two_local};
use crate::test::Rig;

// `two_local`'s project, whose candidates are `mine`, `other` and `claude`, and
// whose `reviews.need` is left out.
fn three_candidates() -> Rig {
    let rig = two_local();
    rig.edit_settings(|s| {
        s.replace(
            r#"reviewers = ["mine", "other"]"#,
            r#"reviewers = ["mine", "other", "claude"]"#,
        )
    });
    rig.reviews_need(None);
    rig
}

#[test]
fn two_reviewers_read_by_default_and_the_pass_goes_to_ci() {
    let rig = three_candidates();
    let runner = at_review(&rig, "work.txt");

    reviewers_until_ci(&rig, &runner);

    assert_eq!(rig.reviewer.seen().len(), 2, "mine and other read");
    assert!(reviewer_models(&rig).is_empty(), "claude was not needed");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "ci");
}
