use std::path::PathBuf;

use super::borrowed;
use shep_kelpie_test_support::git;

// An origin holding `main`, and a clone whose `head` branch has merged `other`, a branch
// cut from `main` that origin holds too
struct Rig {
    _dir: tempfile::TempDir,
    repo: PathBuf,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        git(
            &root,
            &["init", "--quiet", "--bare", "-b", "main", "origin.git"],
        );
        git(&root, &["clone", "--quiet", "origin.git", "repo"]);
        let repo = root.join("repo");
        git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "init"]);
        git(&repo, &["push", "--quiet", "origin", "main"]);
        Self { _dir: dir, repo }
    }

    fn commit_on(&self, branch: &str, start: &str, message: &str) -> String {
        git(&self.repo, &["checkout", "--quiet", "-B", branch, start]);
        git(
            &self.repo,
            &["commit", "--quiet", "--allow-empty", "-m", message],
        );
        git(&self.repo, &["push", "--quiet", "origin", branch]);
        git(&self.repo, &["rev-parse", "HEAD"])
    }

    fn head_merging(&self, tip: &str) -> String {
        git(&self.repo, &["checkout", "--quiet", "-B", "head", "main"]);
        git(
            &self.repo,
            &["merge", "--quiet", "--no-ff", "-m", "merge", tip],
        );
        git(&self.repo, &["rev-parse", "HEAD"])
    }

    // A second clone of origin, as another worker's checkout
    fn other_clone(&self) -> PathBuf {
        let root = self.repo.parent().unwrap();
        git(root, &["clone", "--quiet", "origin.git", "other"]);
        root.join("other")
    }

    fn borrowed(&self, head: &str, tips: &[String]) -> Vec<String> {
        borrowed(&self.repo, head, "main", tips).unwrap()
    }
}

#[test]
fn a_tip_the_head_merged_and_main_lacks_is_borrowed() {
    let rig = Rig::new();
    let tip = rig.commit_on("kelpie/9", "main", "other");
    let head = rig.head_merging(&tip);
    assert_eq!(rig.borrowed(&head, std::slice::from_ref(&tip)), [tip]);
}

#[test]
fn a_tip_main_already_has_is_not_borrowed() {
    let rig = Rig::new();
    let tip = rig.commit_on("kelpie/9", "main", "other");
    let head = rig.head_merging(&tip);
    // The pull request merged: main holds its tip, as origin's fetch will show
    git(&rig.repo, &["checkout", "--quiet", "main"]);
    git(&rig.repo, &["merge", "--quiet", "--ff-only", &tip]);
    git(&rig.repo, &["push", "--quiet", "origin", "main"]);
    assert_eq!(rig.borrowed(&head, &[tip]), Vec::<String>::new());
}

#[test]
fn a_tip_the_head_lacks_is_not_borrowed() {
    let rig = Rig::new();
    let tip = rig.commit_on("kelpie/9", "main", "other");
    let head = rig.commit_on("head", "main", "own");
    assert_eq!(rig.borrowed(&head, &[tip]), Vec::<String>::new());
}

#[test]
fn a_tip_git_does_not_hold_is_not_borrowed() {
    let rig = Rig::new();
    let tip = rig.commit_on("kelpie/9", "main", "other");
    let head = rig.head_merging(&tip);
    let unknown = "0123456789abcdef0123456789abcdef01234567".to_owned();
    assert_eq!(
        rig.borrowed(&head, &[unknown, tip.clone()]),
        [tip],
        "a tip git lacks is skipped, the rest still read"
    );
}

#[test]
fn main_is_fetched_before_the_tips_are_read() {
    let rig = Rig::new();
    let tip = rig.commit_on("kelpie/9", "main", "other");
    let head = rig.head_merging(&tip);
    // Main moves to hold the tip on origin only; the clone's `origin/main` is stale
    let other = rig.other_clone();
    git(
        &other,
        &["merge", "--quiet", "--ff-only", "origin/kelpie/9"],
    );
    git(&other, &["push", "--quiet", "origin", "main"]);
    assert_eq!(rig.borrowed(&head, &[tip]), Vec::<String>::new());
}

#[test]
fn a_tip_that_moved_on_after_the_head_merged_it_is_still_borrowed() {
    let rig = Rig::new();
    let merged = rig.commit_on("kelpie/9", "main", "other");
    let head = rig.head_merging(&merged);
    // The pull request gets a fix on origin that this clone never fetched
    let other = rig.other_clone();
    git(&other, &["checkout", "--quiet", "kelpie/9"]);
    git(&other, &["commit", "--quiet", "--allow-empty", "-m", "fix"]);
    git(&other, &["push", "--quiet", "origin", "kelpie/9"]);
    let moved = git(&other, &["rev-parse", "HEAD"]);
    assert_eq!(rig.borrowed(&head, std::slice::from_ref(&moved)), [moved]);
}
