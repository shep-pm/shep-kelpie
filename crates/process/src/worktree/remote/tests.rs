use std::path::PathBuf;

use crate::worktree::{
    Base, Rebase, Remote, Start, Worktree, WorktreeError, adopt, base_of, clone, head, origin_head,
    prepare, push_commits, rebase, remove,
};
use shep_kelpie_test_support::git;

// An origin holding `main`, a clone of it, and a second bare repository as
// the contributor's fork, whose `contrib` and `main` branches both hold a
// commit origin lacks.
struct Rig {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        git(
            &root,
            &["init", "--quiet", "--bare", "-b", "main", "origin.git"],
        );
        git(
            &root,
            &["init", "--quiet", "--bare", "-b", "main", "fork.git"],
        );
        git(&root, &["clone", "--quiet", "origin.git", "repo"]);
        let rig = Self { _dir: dir, root };
        let repo = rig.repo();
        git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "init"]);
        git(&repo, &["push", "--quiet", "origin", "main"]);
        git(&repo, &["checkout", "--quiet", "-b", "contrib"]);
        git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "fork"]);
        git(
            &repo,
            &["push", "--quiet", "../fork.git", "contrib", "contrib:main"],
        );
        git(&repo, &["checkout", "--quiet", "main"]);
        rig
    }

    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    fn worktree(&self) -> PathBuf {
        self.root.join("wt")
    }

    fn build(&self) -> PathBuf {
        self.root.join("build")
    }

    fn fork(&self, branch: &str) -> Remote {
        Remote::Fork {
            url: self.root.join("fork.git").to_string_lossy().into_owned(),
            branch: branch.into(),
        }
    }

    fn prepare(&self, branch: &str, start: Start, remote: &Remote) {
        prepare(
            &self.repo(),
            &self.worktree(),
            branch,
            start,
            "main",
            remote,
            &self.build(),
        )
        .unwrap();
    }

    fn rev(&self, repo: &str, reference: &str) -> String {
        git(&self.root.join(repo), &["rev-parse", reference])
    }

    fn at_worktree(&self, args: &[&str]) -> String {
        git(&self.worktree(), args)
    }

    // A ref as the worktree's clone holds it
    fn in_clone(&self, reference: &str) -> String {
        git(
            &clone::git_dir_of(&self.worktree()),
            &["rev-parse", reference],
        )
    }

    // What the checkout holds under `refs/heads/kelpie/`
    fn checkout_branches(&self) -> String {
        git(&self.repo(), &["for-each-ref", "refs/heads/kelpie"])
    }

    // Take `main` off origin, which is what a fetch of it fails against
    fn drop_main(&self) {
        git(
            &self.root.join("origin.git"),
            &["update-ref", "-d", "refs/heads/main"],
        );
    }

    fn prepared(&self, branch: &str, start: Start, remote: &Remote) -> Worktree {
        prepare(
            &self.repo(),
            &self.worktree(),
            branch,
            start,
            "main",
            remote,
            &self.build(),
        )
        .unwrap()
    }
}

#[test]
fn prepare_on_origin_starts_and_reuses_a_worktree_as_before() {
    let rig = Rig::new();
    let main = rig.rev("origin.git", "main");

    rig.prepare("kelpie/7", Start::Main, &Remote::Origin);
    assert_eq!(rig.at_worktree(&["rev-parse", "HEAD"]), main);
    assert_eq!(rig.in_clone("refs/heads/kelpie/7"), main);
    assert_eq!(
        rig.at_worktree(&["symbolic-ref", "HEAD"]),
        "refs/heads/kelpie/7"
    );
    assert_eq!(rig.checkout_branches(), "");
    assert!(rig.build().is_dir());

    rig.at_worktree(&["commit", "--quiet", "--allow-empty", "-m", "work"]);
    let worked = rig.at_worktree(&["rev-parse", "HEAD"]);
    rig.prepare("kelpie/7", Start::Main, &Remote::Origin);
    assert_eq!(rig.at_worktree(&["rev-parse", "HEAD"]), worked);
    assert_eq!(rig.in_clone("refs/heads/kelpie/7"), worked);
    assert_eq!(rig.checkout_branches(), "");
}

// A worker is told to merge `origin/main` and to run no fetch of its own, so
// kelpie brings main to its worktree at the start of each of its turns.
#[test]
fn prepare_fetches_main_into_a_worktree_that_already_exists() {
    let rig = Rig::new();
    rig.prepare("kelpie/7", Start::Main, &Remote::Origin);
    let stale = rig.in_clone("refs/remotes/origin/main");
    git(&rig.root, &["clone", "--quiet", "origin.git", "other"]);
    git(
        &rig.root.join("other"),
        &["commit", "--quiet", "--allow-empty", "-m", "main moves"],
    );
    git(
        &rig.root.join("other"),
        &["push", "--quiet", "origin", "main"],
    );
    assert_eq!(rig.in_clone("refs/remotes/origin/main"), stale);

    rig.prepare("kelpie/7", Start::Main, &Remote::Origin);

    assert_eq!(
        rig.in_clone("refs/remotes/origin/main"),
        rig.rev("origin.git", "main")
    );
}

// A rework's worker merges `main` too, and its worktree is cut from the pull
// request's branch rather than from `origin/main`.
#[test]
fn prepare_on_a_pushed_branch_fetches_main_as_well() {
    let rig = Rig::new();
    git(
        &rig.repo(),
        &["push", "--quiet", "origin", "contrib:kelpie/7"],
    );
    git(&rig.root, &["clone", "--quiet", "origin.git", "other"]);
    git(
        &rig.root.join("other"),
        &["commit", "--quiet", "--allow-empty", "-m", "main moves"],
    );
    git(
        &rig.root.join("other"),
        &["push", "--quiet", "origin", "main"],
    );
    assert_ne!(
        rig.rev("repo", "refs/remotes/origin/main"),
        rig.rev("origin.git", "main")
    );

    rig.prepare("kelpie/7", Start::Pushed, &Remote::Origin);

    assert_eq!(
        rig.in_clone("refs/remotes/origin/main"),
        rig.rev("origin.git", "main")
    );
}

#[test]
fn prepare_on_origin_starts_a_pushed_branch_where_origin_holds_it() {
    let rig = Rig::new();
    git(
        &rig.repo(),
        &["push", "--quiet", "origin", "contrib:kelpie/7"],
    );

    rig.prepare("kelpie/7", Start::Pushed, &Remote::Origin);

    assert_eq!(
        rig.at_worktree(&["rev-parse", "HEAD"]),
        rig.rev("origin.git", "kelpie/7")
    );
}

// The worker runs no fetch of its own, so a fetch of main that fails is told
// to the maintainer rather than left as a ref the worker merges blind.
#[test]
fn a_main_fetch_that_fails_is_reported_and_the_worktree_is_kept() {
    let rig = Rig::new();
    rig.prepare("kelpie/7", Start::Main, &Remote::Origin);
    rig.at_worktree(&["commit", "--quiet", "--allow-empty", "-m", "work"]);
    let worked = rig.at_worktree(&["rev-parse", "HEAD"]);
    rig.drop_main();

    let prepared = rig.prepared("kelpie/7", Start::Main, &Remote::Origin);

    let why = prepared.main_fetch_failed.expect("the failed fetch");
    assert!(why.contains("git fetch"), "{why}");
    assert!(why.contains("main"), "{why}");
    assert_eq!(rig.at_worktree(&["rev-parse", "HEAD"]), worked);
    assert!(rig.build().is_dir());
}

// A branch that is already pushed needs main only for the merge its worker
// runs, so its worktree is made even when that fetch fails.
#[test]
fn a_pushed_branch_starts_and_reports_a_main_fetch_that_failed() {
    let rig = Rig::new();
    git(
        &rig.repo(),
        &["push", "--quiet", "origin", "contrib:kelpie/7"],
    );
    rig.drop_main();

    let prepared = rig.prepared("kelpie/7", Start::Pushed, &Remote::Origin);

    assert!(prepared.main_fetch_failed.is_some());
    assert_eq!(
        rig.at_worktree(&["rev-parse", "HEAD"]),
        rig.rev("origin.git", "kelpie/7")
    );
}

// A new worktree on `Start::Main` starts on that ref, so without it there is
// nothing to cut the branch from.
#[test]
fn a_new_worktree_on_main_without_main_is_an_error() {
    let rig = Rig::new();
    rig.drop_main();

    let err = prepare(
        &rig.repo(),
        &rig.worktree(),
        "kelpie/7",
        Start::Main,
        "main",
        &Remote::Origin,
        &rig.build(),
    )
    .unwrap_err();

    assert!(
        matches!(&err, WorktreeError::Git { args, .. } if args.contains("fetch")),
        "{err}"
    );
    assert!(!rig.worktree().exists());
}

#[test]
fn origin_head_on_origin_returns_the_branch_head_as_before() {
    let rig = Rig::new();
    git(
        &rig.repo(),
        &["push", "--quiet", "origin", "contrib:kelpie/7"],
    );

    let tip = origin_head(&rig.repo(), "kelpie/7", &Remote::Origin).unwrap();

    assert_eq!(tip, rig.rev("origin.git", "kelpie/7"));
}

#[test]
fn prepare_on_a_fork_remote_starts_a_worktree_at_the_forks_branch() {
    let rig = Rig::new();

    rig.prepare("kelpie/7", Start::Pushed, &rig.fork("contrib"));

    assert_eq!(
        rig.at_worktree(&["rev-parse", "HEAD"]),
        rig.rev("fork.git", "contrib")
    );
    assert_eq!(
        rig.at_worktree(&["symbolic-ref", "HEAD"]),
        "refs/heads/kelpie/7"
    );
    assert_eq!(
        rig.at_worktree(&[
            "for-each-ref",
            "--format=%(upstream)",
            "refs/heads/kelpie/7"
        ]),
        ""
    );
}

#[test]
fn origin_head_on_a_fork_remote_returns_the_forks_head() {
    let rig = Rig::new();

    let tip = origin_head(&rig.repo(), "kelpie/7", &rig.fork("contrib")).unwrap();

    assert_eq!(tip, rig.rev("fork.git", "contrib"));
}

#[test]
fn a_fork_remote_cannot_move_origins_main() {
    let rig = Rig::new();
    let main = rig.rev("origin.git", "main");
    assert_ne!(rig.rev("fork.git", "main"), main);
    let hostile = rig.fork("main");

    let tip = origin_head(&rig.repo(), "kelpie/7", &hostile).unwrap();
    rig.prepare("kelpie/7", Start::Pushed, &hostile);
    let base = base_of(&rig.repo(), "kelpie/7", &tip, &hostile, "main").unwrap();

    assert_eq!(tip, rig.rev("fork.git", "main"));
    assert_eq!(base, Base::Current);
    assert_eq!(rig.rev("repo", "refs/remotes/origin/main"), main);
    assert_eq!(rig.rev("origin.git", "main"), main);
}

// A worktree made before worktrees were clones is a linked one on its branch,
// holding work the worker has not committed.
#[test]
fn prepare_turns_a_linked_worktree_into_a_clone_and_keeps_its_files() {
    let rig = Rig::new();
    let at = rig.worktree();
    git(
        &rig.repo(),
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "kelpie/7",
            at.to_str().unwrap(),
            "origin/main",
        ],
    );
    std::fs::write(at.join("done.txt"), "done\n").unwrap();
    rig.at_worktree(&["add", "done.txt"]);
    rig.at_worktree(&["commit", "--quiet", "-m", "work"]);
    let worked = rig.at_worktree(&["rev-parse", "HEAD"]);
    std::fs::write(at.join("half.txt"), "half\n").unwrap();

    let prepared = rig.prepared("kelpie/7", Start::Main, &Remote::Origin);

    assert_eq!(prepared.git_common_dir, rig.repo().join(".git"));
    assert_eq!(prepared.git_dir, clone::git_dir_of(&at));
    assert!(!rig.repo().join(".git/worktrees").join("wt").exists());
    assert!(!git(&rig.repo(), &["worktree", "list", "--porcelain"]).contains(at.to_str().unwrap()));
    assert_eq!(rig.in_clone("refs/heads/kelpie/7"), worked);
    assert_eq!(rig.at_worktree(&["rev-parse", "HEAD"]), worked);
    assert_eq!(
        std::fs::read_to_string(at.join("half.txt")).unwrap(),
        "half\n"
    );
    assert_eq!(rig.at_worktree(&["status", "--porcelain"]), "?? half.txt");
    assert_eq!(
        rig.in_clone("refs/remotes/origin/main"),
        rig.rev("origin.git", "main")
    );
    // The checkout keeps the branch, so its gc keeps what the clone borrows.
    assert_eq!(rig.rev("repo", "refs/heads/kelpie/7"), worked);
}

#[test]
fn prepare_names_the_checkouts_git_dir_common_and_the_clones_own() {
    let rig = Rig::new();

    let prepared = rig.prepared("kelpie/7", Start::Main, &Remote::Origin);

    assert_eq!(prepared.git_common_dir, rig.repo().join(".git"));
    assert_eq!(prepared.git_dir, clone::git_dir_of(&rig.worktree()));
}

#[test]
fn prepare_mirrors_a_pushed_branchs_tracking_ref_into_its_clone() {
    let rig = Rig::new();
    git(
        &rig.repo(),
        &["push", "--quiet", "origin", "contrib:kelpie/7"],
    );

    rig.prepare("kelpie/7", Start::Pushed, &Remote::Origin);

    assert_eq!(
        rig.in_clone("refs/remotes/origin/kelpie/7"),
        rig.rev("origin.git", "kelpie/7")
    );
    assert_eq!(rig.checkout_branches(), "");
}

#[test]
fn prepare_refuses_a_clone_on_another_branch() {
    let rig = Rig::new();
    rig.prepare("kelpie/7", Start::Main, &Remote::Origin);
    rig.at_worktree(&["checkout", "--quiet", "-b", "other"]);

    let err = prepare(
        &rig.repo(),
        &rig.worktree(),
        "kelpie/7",
        Start::Main,
        "main",
        &Remote::Origin,
        &rig.build(),
    )
    .unwrap_err();

    assert_eq!(err, WorktreeError::Foreign(rig.worktree()));
}

#[test]
fn prepare_refuses_a_clone_that_borrows_another_repos_objects() {
    let rig = Rig::new();
    rig.prepare("kelpie/7", Start::Main, &Remote::Origin);
    let alternates = clone::git_dir_of(&rig.worktree()).join("objects/info/alternates");
    std::fs::write(&alternates, format!("{}\n", rig.root.display())).unwrap();

    let err = prepare(
        &rig.repo(),
        &rig.worktree(),
        "kelpie/7",
        Start::Main,
        "main",
        &Remote::Origin,
        &rig.build(),
    )
    .unwrap_err();

    assert_eq!(err, WorktreeError::Foreign(rig.worktree()));
}

#[test]
fn prepare_refuses_a_folder_that_is_no_worktree() {
    let rig = Rig::new();
    std::fs::create_dir(rig.worktree()).unwrap();

    let err = prepare(
        &rig.repo(),
        &rig.worktree(),
        "kelpie/7",
        Start::Main,
        "main",
        &Remote::Origin,
        &rig.build(),
    )
    .unwrap_err();

    assert_eq!(err, WorktreeError::Foreign(rig.worktree()));
}

#[test]
fn prepare_reuses_a_rework_branch_the_checkout_kept_at_the_remotes_head() {
    let rig = Rig::new();
    git(
        &rig.repo(),
        &["push", "--quiet", "origin", "contrib:kelpie/7"],
    );
    git(&rig.repo(), &["branch", "kelpie/7", "contrib"]);

    rig.prepare("kelpie/7", Start::Pushed, &Remote::Origin);

    assert_eq!(
        rig.in_clone("refs/heads/kelpie/7"),
        rig.rev("origin.git", "kelpie/7")
    );
}

#[test]
fn prepare_clears_a_stale_packed_refs_lock_in_the_clones_git_dir() {
    let rig = Rig::new();
    rig.prepare("kelpie/7", Start::Main, &Remote::Origin);
    let lock = clone::git_dir_of(&rig.worktree()).join("packed-refs.lock");
    let file = std::fs::File::create(&lock).unwrap();
    file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(120))
        .unwrap();

    rig.prepare("kelpie/7", Start::Main, &Remote::Origin);

    assert!(!lock.exists());
}

#[test]
fn remove_takes_the_clones_git_dir_away() {
    let rig = Rig::new();
    rig.prepare("kelpie/7", Start::Main, &Remote::Origin);
    let git_dir = clone::git_dir_of(&rig.worktree());
    assert!(git_dir.is_dir());

    remove(&rig.repo(), &rig.worktree(), "kelpie/7", &rig.build(), None).unwrap();

    assert!(!rig.worktree().exists());
    assert!(!git_dir.exists());
    assert!(!rig.build().exists());
}

#[test]
fn remove_takes_a_linked_worktree_and_its_branch_away_as_before() {
    let rig = Rig::new();
    let at = rig.worktree();
    git(
        &rig.repo(),
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "kelpie/7",
            at.to_str().unwrap(),
            "origin/main",
        ],
    );

    remove(&rig.repo(), &at, "kelpie/7", &rig.build(), None).unwrap();

    assert!(!at.exists());
    assert_eq!(rig.checkout_branches(), "");
    assert!(!git(&rig.repo(), &["worktree", "list", "--porcelain"]).contains(at.to_str().unwrap()));
}

#[test]
fn remove_finishes_a_removal_that_took_the_worktree_and_left_the_clones_git_dir() {
    let rig = Rig::new();
    rig.prepare("kelpie/7", Start::Main, &Remote::Origin);
    std::fs::remove_dir_all(rig.worktree()).unwrap();

    remove(&rig.repo(), &rig.worktree(), "kelpie/7", &rig.build(), None).unwrap();

    assert!(!clone::git_dir_of(&rig.worktree()).exists());
}

#[test]
fn rebase_and_remove_on_origin_behave_as_before() {
    let rig = Rig::new();
    let origin = Remote::Origin;
    rig.prepare("kelpie/7", Start::Main, &origin);
    rig.at_worktree(&["commit", "--quiet", "--allow-empty", "-m", "work"]);
    rig.at_worktree(&["push", "--quiet", "origin", "HEAD:refs/heads/kelpie/7"]);
    git(&rig.repo(), &["checkout", "--quiet", "main"]);
    git(
        &rig.repo(),
        &["commit", "--quiet", "--allow-empty", "-m", "main moves"],
    );
    git(&rig.repo(), &["push", "--quiet", "origin", "main"]);
    let work = head(&rig.repo(), &rig.worktree()).unwrap();
    assert_eq!(
        base_of(&rig.repo(), "kelpie/7", &work, &origin, "main").unwrap(),
        Base::Behind
    );

    let rebased = rebase(
        &rig.repo(),
        &rig.worktree(),
        "kelpie/7",
        &work,
        true,
        &origin,
        "main",
    )
    .unwrap();

    let Rebase::Pushed(caught) = rebased else {
        panic!("expected a push, got {rebased:?}");
    };
    assert_eq!(caught, rig.rev("origin.git", "kelpie/7"));
    assert_eq!(
        base_of(&rig.repo(), "kelpie/7", &caught, &origin, "main").unwrap(),
        Base::Current
    );

    remove(
        &rig.repo(),
        &rig.worktree(),
        "kelpie/7",
        &rig.build(),
        Some(&origin),
    )
    .unwrap();
    assert!(!rig.worktree().exists());
    assert!(!rig.build().exists());
    assert_eq!(
        git(
            &rig.root.join("origin.git"),
            &["branch", "--list", "kelpie/7"]
        ),
        ""
    );
}

#[test]
fn rebase_and_remove_on_a_fork_remote_push_and_delete_on_the_fork() {
    let rig = Rig::new();
    git(
        &rig.repo(),
        &["commit", "--quiet", "--allow-empty", "-m", "main moves"],
    );
    git(&rig.repo(), &["push", "--quiet", "origin", "main"]);
    let fork = rig.fork("contrib");
    rig.prepare("kelpie/7", Start::Pushed, &fork);
    let work = head(&rig.repo(), &rig.worktree()).unwrap();
    assert_eq!(
        base_of(&rig.repo(), "kelpie/7", &work, &fork, "main").unwrap(),
        Base::Behind
    );

    let rebased = rebase(
        &rig.repo(),
        &rig.worktree(),
        "kelpie/7",
        &work,
        true,
        &fork,
        "main",
    )
    .unwrap();

    let Rebase::Pushed(caught) = rebased else {
        panic!("expected a push, got {rebased:?}");
    };
    assert_eq!(caught, rig.rev("fork.git", "contrib"));
    assert_eq!(
        git(
            &rig.root.join("origin.git"),
            &["branch", "--list", "contrib"]
        ),
        ""
    );

    remove(
        &rig.repo(),
        &rig.worktree(),
        "kelpie/7",
        &rig.build(),
        Some(&fork),
    )
    .unwrap();
    assert_eq!(
        git(&rig.root.join("fork.git"), &["branch", "--list", "contrib"]),
        ""
    );
}

// A worker commits and kelpie pushes: the commits land on the fork's branch,
// by commit, so nothing but a fast-forward is accepted.
#[test]
fn push_commits_fast_forwards_the_forks_branch() {
    let rig = Rig::new();
    let remote = rig.fork("contrib");
    rig.prepare("kelpie/7", Start::Pushed, &remote);
    rig.at_worktree(&["commit", "--quiet", "--allow-empty", "-m", "work"]);

    push_commits(&rig.repo(), &rig.worktree(), &remote).unwrap();

    assert_eq!(
        rig.rev("fork.git", "contrib"),
        rig.at_worktree(&["rev-parse", "HEAD"])
    );
}

// A clone's commits live in its own object folder, so the push has to run
// from the clone rather than from the checkout.
#[test]
fn push_commits_pushes_a_commit_only_the_clone_holds() {
    let rig = Rig::new();
    let remote = rig.fork("contrib");
    let at = rig.rev("repo", "contrib");
    clone::make(&rig.repo(), &rig.worktree(), "kelpie/7", &at).unwrap();
    rig.at_worktree(&["commit", "--quiet", "--allow-empty", "-m", "work"]);
    let work = head(&rig.repo(), &rig.worktree()).unwrap();
    assert!(crate::worktree::git(&rig.repo(), ["cat-file", "-e", &work]).is_err());

    push_commits(&rig.repo(), &rig.worktree(), &remote).unwrap();

    assert_eq!(rig.rev("fork.git", "contrib"), work);
}

#[test]
fn push_commits_never_overwrites_a_head_the_worktree_lacks() {
    let rig = Rig::new();
    let remote = rig.fork("contrib");
    rig.prepare("kelpie/7", Start::Pushed, &remote);
    rig.at_worktree(&["commit", "--quiet", "--allow-empty", "-m", "work"]);
    git(&rig.repo(), &["checkout", "--quiet", "contrib"]);
    git(
        &rig.repo(),
        &["commit", "--quiet", "--allow-empty", "-m", "theirs"],
    );
    git(&rig.repo(), &["push", "--quiet", "../fork.git", "contrib"]);
    let theirs = rig.rev("fork.git", "contrib");

    let refused = push_commits(&rig.repo(), &rig.worktree(), &remote);

    assert!(matches!(refused, Err(WorktreeError::Git { .. })));
    assert_eq!(rig.rev("fork.git", "contrib"), theirs);
}

#[test]
fn push_commits_leaves_origin_to_its_worker() {
    let rig = Rig::new();
    rig.prepare("kelpie/7", Start::Main, &Remote::Origin);

    let refused = push_commits(&rig.repo(), &rig.worktree(), &Remote::Origin);

    assert!(refused.is_err());
}

// A contributor pushes while the worker's commits wait: the maintainer's yes
// merges their head into those commits rather than refusing the worktree.
#[test]
fn adopt_merges_the_forks_new_head_into_commits_kelpie_has_not_pushed() {
    let rig = Rig::new();
    let remote = rig.fork("contrib");
    rig.prepare("kelpie/7", Start::Pushed, &remote);
    let known = rig.at_worktree(&["rev-parse", "HEAD"]);
    std::fs::write(rig.worktree().join("mine.txt"), "mine\n").unwrap();
    rig.at_worktree(&["add", "mine.txt"]);
    rig.at_worktree(&["commit", "--quiet", "-m", "work"]);
    let mine = rig.at_worktree(&["rev-parse", "HEAD"]);
    git(&rig.repo(), &["checkout", "--quiet", "contrib"]);
    std::fs::write(rig.repo().join("theirs.txt"), "theirs\n").unwrap();
    git(&rig.repo(), &["add", "theirs.txt"]);
    git(&rig.repo(), &["commit", "--quiet", "-m", "theirs"]);
    git(&rig.repo(), &["push", "--quiet", "../fork.git", "contrib"]);
    let theirs = rig.rev("fork.git", "contrib");

    adopt(
        &rig.repo(),
        &rig.worktree(),
        "kelpie/7",
        &known,
        &theirs,
        &remote,
    )
    .unwrap();

    let parents = rig.at_worktree(&["rev-list", "--parents", "-n1", "HEAD"]);
    assert!(
        parents.contains(&mine) && parents.contains(&theirs),
        "{parents}"
    );
    push_commits(&rig.repo(), &rig.worktree(), &remote).unwrap();
    assert_eq!(
        rig.rev("fork.git", "contrib"),
        rig.at_worktree(&["rev-parse", "HEAD"])
    );
}

#[test]
fn adopt_abandons_a_merge_that_conflicts_and_refuses_the_worktree() {
    let rig = Rig::new();
    let remote = rig.fork("contrib");
    rig.prepare("kelpie/7", Start::Pushed, &remote);
    let known = rig.at_worktree(&["rev-parse", "HEAD"]);
    std::fs::write(rig.worktree().join("same.txt"), "mine\n").unwrap();
    rig.at_worktree(&["add", "same.txt"]);
    rig.at_worktree(&["commit", "--quiet", "-m", "work"]);
    let mine = rig.at_worktree(&["rev-parse", "HEAD"]);
    git(&rig.repo(), &["checkout", "--quiet", "contrib"]);
    std::fs::write(rig.repo().join("same.txt"), "theirs\n").unwrap();
    git(&rig.repo(), &["add", "same.txt"]);
    git(&rig.repo(), &["commit", "--quiet", "-m", "theirs"]);
    git(&rig.repo(), &["push", "--quiet", "../fork.git", "contrib"]);
    let theirs = rig.rev("fork.git", "contrib");

    let refused = adopt(
        &rig.repo(),
        &rig.worktree(),
        "kelpie/7",
        &known,
        &theirs,
        &remote,
    );

    assert!(matches!(refused, Err(WorktreeError::Unsettled(_))));
    assert_eq!(rig.at_worktree(&["rev-parse", "HEAD"]), mine);
    assert_eq!(rig.at_worktree(&["status", "--porcelain"]), "");
}

// A repo's own pre-push hook can fail on a branch kelpie only deletes, and
// the work item's cleanup would retry for hours.
#[test]
fn remove_deletes_the_branch_on_origin_when_its_pre_push_hook_fails() {
    let rig = Rig::new();
    rig.prepare("kelpie/7", Start::Main, &Remote::Origin);
    rig.at_worktree(&["push", "--quiet", "origin", "kelpie/7"]);
    let hook = rig.repo().join(".git/hooks/pre-push");
    shep_kelpie_test_support::write_script(&hook, "#!/bin/sh\nexit 1\n");

    remove(
        &rig.repo(),
        &rig.worktree(),
        "kelpie/7",
        &rig.build(),
        Some(&Remote::Origin),
    )
    .unwrap();

    assert_eq!(
        git(
            &rig.root.join("origin.git"),
            &["branch", "--list", "kelpie/7"]
        ),
        ""
    );
}
