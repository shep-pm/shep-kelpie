//! Filing retries saved before follow-up rulings became mandatory

use super::*;

#[test]
fn a_yes_to_a_saved_file_policy_refusal_retries_unread_without_another_ruling() {
    let found = [racy()];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));
    rig.forge.set_issues_down(true);
    assert!(matches!(
        after_yes(&runner),
        Some(StepReport::GateFailed { .. })
    ));
    rig.clock.advance(6 * 60 * 60);
    let Some(StepReport::Ruling { id, .. }) = after_merge(&runner) else {
        panic!("the maintainer was not asked after the window");
    };
    drop(runner);

    // The old `file` policy reached a refusal ruling without an earlier yes.
    let path = rig.paths().state;
    let mut saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let pending = saved["work_items"][0]["follow_ups"]
        .as_object_mut()
        .unwrap();
    pending.remove("ruled");
    pending.remove("unread");
    std::fs::write(path, saved.to_string()).unwrap();
    rig.edit_settings(|s| s.replace("issues = \"ask\"", "issues = \"file\""));
    let runner = rig.open().unwrap();

    rig.forge.set_issues_down(false);
    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    assert_eq!(after_merge(&runner), filed(&[900], &[], 0));
    let [issue] = rig.forge.created().try_into().unwrap();
    assert_eq!(issue.labels, [TRIAGE]);
    assert!(finished(after_merge(&runner)));
}
