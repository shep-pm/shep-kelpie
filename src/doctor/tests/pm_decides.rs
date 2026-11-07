use super::*;

const LINE: &str = "golbat: rulings.pm_decides";

#[tokio::test]
async fn a_map_missing_kinds_gives_one_missing_line_naming_the_migration() {
    let scene = Scene::new().await;
    scene.runs("golbat", |t| {
        let map = t["rulings"]["pm_decides"].as_object_mut().unwrap();
        map.remove("rebase");
        map.remove("question");
    });

    let report = scene.report().await;

    let (what, fix) = missing(&report, LINE);
    assert_eq!(
        what,
        "`rulings.pm_decides` does not name question, rebase, so you decide them"
    );
    assert_eq!(fix, "shep kelpie upgrade, or shep kelpie settings migrate");
    assert!(!subjects(&report).contains(&"koji: rulings.pm_decides"));
}

#[tokio::test]
async fn an_old_list_misses_every_kind_it_does_not_name() {
    let scene = Scene::new().await;
    scene.runs("golbat", |t| {
        t["rulings"] = json!({ "pm_alone": ["rebase"] })
    });

    let report = scene.report().await;

    let (what, _) = missing(&report, LINE);
    assert!(what.contains("still-red"), "{what}");
    assert!(!what.contains("rebase"), "{what}");
    // `merge` has a default now, so a map that leaves it out is named too.
    assert!(what.contains("merge, question"), "{what}");
}
