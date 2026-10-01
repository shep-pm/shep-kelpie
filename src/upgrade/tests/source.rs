//! Building a ref, which runs the real git and cargo on a throwaway repo

use std::process::Command;

use super::*;

#[test]
fn a_ref_is_built_where_it_stands_in_the_repo() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    let write = |file: &str, text: &str| std::fs::write(repo.join(file), text).unwrap();
    write(
        "Cargo.toml",
        "[package]\nname = \"shep-kelpie\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(
        "Cargo.lock",
        "version = 4\n\n[[package]]\nname = \"shep-kelpie\"\nversion = \"0.1.0\"\n",
    );
    let main = |version: &str| {
        format!(
            "fn main() {{ println!(\"{{{{\\\"kelpie\\\":\\\"{version}\\\",\\\"shep\\\":\\\"0.11.0\\\"}}}}\"); }}\n"
        )
    };
    let git = |args: &[&str]| {
        let ran = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(ran.status.success(), "git {args:?}: {ran:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    write("src/main.rs", &main("0.1.0"));
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "first"]);
    git(&["tag", "v0.1.0"]);
    write("src/main.rs", &main("0.2.0"));
    git(&["commit", "-q", "-am", "second"]);

    let work = dir.path().join("work");
    let repo = repo.to_str().unwrap();
    for (reference, version) in [("v0.1.0", "0.1.0"), ("main", "0.2.0")] {
        let binary = fetch::build_ref(&work, repo, reference).unwrap();
        assert_eq!(Build::of(&binary).unwrap().kelpie, version, "{reference}");
    }
    let err = fetch::build_ref(&work, repo, "nope").unwrap_err();
    assert!(err.contains("has no ref nope"), "{err}");
}
