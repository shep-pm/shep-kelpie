//! The worker folder's files from before each work item's calls had their own
//!
//! A call's harness files are named for its work item, `settings-<issue>.json`
//! and the files beside it. The unsuffixed ones an older kelpie wrote are
//! read by nothing now, so a runner removes them as it starts.

use std::fs;
use std::io;
use std::path::Path;

/// The settings files' stems an older kelpie shared between work items
const OLD_STEMS: [&str; 2] = ["settings", "review-settings"];

/// What each harness kept beside a call's settings file, by extension
const BESIDE: [&str; 9] = [
    "json",
    "codex",
    "threads",
    "pi",
    "tmp",
    "sandbox.json",
    "stdout.jsonl",
    "stderr.log",
    "guard.ts",
];

/// Removes the old shared files from `folder`, and says what it removed
///
/// A link is removed itself, never what it points to: a Codex home links
/// the maintainer's login.
pub(super) fn remove_old(folder: &Path) -> Vec<String> {
    let old = OLD_STEMS
        .iter()
        .flat_map(|stem| BESIDE.iter().map(move |ext| format!("{stem}.{ext}")))
        .chain(["instructions.md".to_owned()]);
    let mut said = Vec::new();
    for name in old {
        let path = folder.join(name);
        match remove(&path) {
            Ok(false) => {}
            Ok(true) => said.push(format!(
                "removed {}, which no work item's call reads now",
                path.display()
            )),
            Err(e) => said.push(format!("cannot remove {}: {e}", path.display())),
        }
    }
    said
}

// Removes `path` without following a link, and says whether it was there.
fn remove(path: &Path) -> io::Result<bool> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    // `remove_dir_all` removes the links inside a folder, not their targets.
    match meta.is_dir() {
        true => fs::remove_dir_all(path)?,
        false => fs::remove_file(path)?,
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use crate::test::Rig;

    #[test]
    fn a_start_removes_the_old_shared_files_and_never_what_a_link_points_to() {
        let rig = Rig::new("acme");
        let worker = rig.paths().worker;
        let login = rig.home.path().join("codex-login");
        fs_write(&login.join("auth.json"), "secret");
        let elsewhere = rig.home.path().join("elsewhere");
        fs_write(&elsewhere.join("kept.txt"), "kept");
        fs_write(&worker.join("settings.json"), "{}");
        fs_write(&worker.join("instructions.md"), "old");
        fs_write(&worker.join("review-settings.sandbox.json"), "{}");
        std::fs::create_dir_all(worker.join("settings.codex")).unwrap();
        symlink(
            login.join("auth.json"),
            worker.join("settings.codex/auth.json"),
        )
        .unwrap();
        symlink(&elsewhere, worker.join("settings.tmp")).unwrap();
        fs_write(&worker.join("settings-7.json"), "{}");

        let runner = rig.open().unwrap();

        for gone in [
            "settings.json",
            "instructions.md",
            "review-settings.sandbox.json",
            "settings.codex",
            "settings.tmp",
        ] {
            assert!(
                std::fs::symlink_metadata(worker.join(gone)).is_err(),
                "{gone} is still there"
            );
        }
        assert!(worker.join("settings-7.json").exists(), "a new file went");
        assert_eq!(
            std::fs::read_to_string(login.join("auth.json")).unwrap(),
            "secret"
        );
        assert!(elsewhere.join("kept.txt").exists(), "a link was followed");
        let notes = runner.lock().unwrap().take_notes();
        let removed = notes.iter().filter(|n| n.starts_with("removed ")).count();
        assert_eq!(removed, 5, "{notes:?}");
    }

    fn fs_write(path: &std::path::Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}
