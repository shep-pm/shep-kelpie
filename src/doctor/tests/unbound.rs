use super::*;

#[tokio::test]
async fn an_unbound_file_gives_one_missing_line_naming_the_command() {
    let scene = Scene::new().await;
    scene.runs("golbat", |_| {});
    let stub = "---\nrole: pm\n---\nYou keep the project's notes.\n";
    crate::test::write_in(&scene.kelpie_home.join("agents"), "scribe.md", stub);

    let report = scene.report().await;

    let lines = subjects(&report);
    assert_eq!(lines.iter().filter(|s| **s == "agent scribe").count(), 1);
    let (what, fix) = missing(&report, "agent scribe");
    assert_eq!(what, "`scribe.md` binds no harness, model or effort");
    assert_eq!(fix, "shep kelpie agents new pm --name scribe");
}

#[tokio::test]
async fn a_folder_with_none_gives_no_line() {
    let scene = Scene::new().await;

    let report = scene.report().await;

    let lines = subjects(&report);
    assert!(lines.iter().all(|s| !s.starts_with("agent ")), "{lines:?}");
}

#[tokio::test]
async fn a_stub_the_migration_would_write_names_upgrade_and_migrate() {
    let scene = Scene::new().await;
    scene.runs("golbat", |_| {});
    let stub = "---\nextends: kelpie:pm\n---\n";
    crate::test::write_in(&scene.kelpie_home.join("agents"), "pm.md", stub);

    let report = scene.report().await;

    let (what, fix) = missing(&report, "agent pm");
    assert_eq!(what, "`pm.md` binds no harness, model or effort");
    assert_eq!(fix, "shep kelpie upgrade, or shep kelpie agents migrate");
}

#[tokio::test]
async fn a_file_the_migration_skips_says_why() {
    let scene = Scene::new().await;
    scene.runs("golbat", |_| {});
    let file = "---\nextends: kelpie:spec\nharness: pi\n---\n";
    crate::test::write_in(&scene.kelpie_home.join("agents"), "spec.md", file);

    let report = scene.report().await;

    let (what, fix) = missing(&report, "agent spec");
    assert!(
        what.starts_with("`spec.md` is skipped by the migration: "),
        "{what}"
    );
    assert!(what.contains("model"), "{what}");
    assert!(fix.contains("`model` and `effort`"), "{fix}");
}

#[tokio::test]
async fn a_skipped_file_does_not_hide_a_stub_the_migration_would_write() {
    let scene = Scene::new().await;
    scene.runs("golbat", |_| {});
    let agents = scene.kelpie_home.join("agents");
    crate::test::write_in(&agents, "pm.md", "---\nextends: kelpie:pm\n---\n");
    let skipped = "---\nextends: kelpie:pm\nharness: codex\n---\n";
    crate::test::write_in(&agents, "codex-pm.md", skipped);

    let report = scene.report().await;

    let (_, fix) = missing(&report, "agent pm");
    assert_eq!(fix, "shep kelpie upgrade, or shep kelpie agents migrate");
    let (what, _) = missing(&report, "agent codex-pm");
    assert!(what.contains("is skipped by the migration"), "{what}");
}

#[tokio::test]
async fn a_file_the_migration_leaves_is_named_with_why() {
    let scene = Scene::new().await;
    scene.runs("golbat", |_| {});
    let file = "---\nextends: kelpie:advisor\nmodel: m\n---\nAlso read the changelog.\n";
    crate::test::write_in(&scene.kelpie_home.join("agents"), "mine.md", file);

    let report = scene.report().await;

    let (what, fix) = missing(&report, "agent mine");
    assert!(
        what.starts_with("`mine.md` is left by the migration: "),
        "{what}"
    );
    assert!(
        what.contains("the `advisor` role went with the ruling panel"),
        "{what}"
    );
    assert_eq!(fix, "delete `mine.md`");
}

#[tokio::test]
async fn files_named_with_an_underscore_are_ignored_and_named_in_one_line() {
    let scene = Scene::new().await;
    scene.runs("golbat", |_| {});
    let agents = scene.kelpie_home.join("agents");
    crate::test::write_in(&agents, "_old.md", "---\nrole: implementer\n---\n");
    crate::test::write_in(&agents, "_haiku.md", "no frontmatter\n");

    let report = scene.report().await;

    assert_eq!(
        ok(&report, "agent files"),
        "ignored, since their names start with `_`: _haiku.md, _old.md"
    );
    ok(&report, "golbat: implementers");
}
