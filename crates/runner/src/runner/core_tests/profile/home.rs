//! What a worker reads of the machine's home folder

use super::*;

// The whole home is denied, so a `KELPIE_HOME` set outside the shepherd's home
// holds the TOTP secret and other projects' state just the same, and the call
// is let back into its own folders one entry at a time.
#[test]
fn a_kelpie_home_outside_the_shepherds_is_denied_around_the_workers_own() {
    let dir = tempfile::tempdir().unwrap();
    let (kelpie, shep) = (dir.path().join("k"), dir.path().join("shep"));
    let (worktree, build) = (
        kelpie.join("koji/worktrees/7"),
        kelpie.join("koji/builds/7"),
    );
    for folder in [
        &worktree,
        &build,
        &kelpie.join("totp"),
        &kelpie.join("rotom"),
        &shep,
    ] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let s = WorkerProfile {
        worktree: &worktree,
        build: &build,
        git_common_dir: Path::new("/r/.git"),
        git_dir: &kelpie.join("koji/worktrees/7.git"),
        branch: "kelpie/7",
        base: "main",
        kelpie_pushes: false,
        kelpie: Path::new("/opt/kelpie"),
        skills: &kelpie.join("shep/skills"),
        kelpie_home: &kelpie,
        repo: Path::new("/r"),
        guard_hooks: &[],
        allowed_domains: &[],
        registries: &[],
        build_env: &BTreeMap::new(),
        home: dir.path(),
        shep_home: &shep,
        door: Path::new("/s/kelpie/dog/lease.sock"),
        token: None,
        login_token: None,
    }
    .settings();

    let files = &s["sandbox"]["filesystem"];
    let denied = strings(&files["denyRead"]);
    assert!(
        denied.contains(&format!("{}/**", dir.path().display()).as_str()),
        "{denied:?}"
    );
    for home in [&kelpie, &shep] {
        assert!(
            !denied.contains(&format!("{}/**", home.display()).as_str()),
            "{} is denied by the home already",
            home.display()
        );
    }
    for folder in [&worktree, &build] {
        assert!(
            strings(&files["allowRead"]).contains(&folder.to_str().unwrap()),
            "{} is not allowed back into the denied home",
            folder.display()
        );
    }
    let deny = strings(&s["permissions"]["deny"]);
    for rule in ["k/totp/**", "k/rotom/**", "shep/**"] {
        let rule = format!("Read(/{}/{rule})", dir.path().display());
        assert!(deny.contains(&rule.as_str()), "{rule} not in {deny:?}");
    }
    let home_rule = format!("Read(/{}/**)", dir.path().display());
    assert!(
        !deny.contains(&home_rule.as_str()),
        "it would hide the worktree"
    );
}

// A worker could read another repo of the home folder, its `.env` included, and
// Claude Code's session transcripts: what it reads can end up in a pull request
// body it writes.
#[test]
fn a_worker_reads_its_own_toolchain_and_no_other_repo_of_the_home() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let (common, git_dir) = (home.join("checkouts/shep/.git"), home.join("wt/shep/7.git"));
    let (worktree, build, skills) = (
        home.join("wt/shep/7"),
        home.join("targets/shep/7"),
        home.join("skills"),
    );
    for (rel, text) in [
        ("other-repo/.env", "SECRET=1\n"),
        ("other-repo/src/lib.rs", "work\n"),
        (".claude/projects/-x/session.jsonl", "{}\n"),
        (".cargo/registry/src/serde-1.0.2/src/lib.rs", "x\n"),
        (".cargo/credentials.toml", "token\n"),
        (".rustup/toolchains/stable/bin/rustc", "x\n"),
        (".gitconfig", "[user]\n"),
        (".config/git/ignore", ".DS_Store\n"),
        (".config/git/credentials", "https://x:token@github.com\n"),
        ("skills/tdd/SKILL.md", "step skills\n"),
    ] {
        let path = home.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let s = WorkerProfile {
        worktree: &worktree,
        build: &build,
        git_common_dir: &common,
        git_dir: &git_dir,
        branch: "kelpie/7",
        base: "main",
        kelpie_pushes: false,
        kelpie: Path::new("/opt/kelpie"),
        kelpie_home: &home,
        repo: &home.join("checkouts/shep"),
        guard_hooks: &[],
        allowed_domains: &[],
        registries: &[],
        build_env: &BTreeMap::new(),
        home: &home,
        shep_home: &home,
        skills: &skills,
        door: Path::new("/s/kelpie/dog/lease.sock"),
        token: None,
        login_token: None,
    }
    .settings();

    let files = &s["sandbox"]["filesystem"];
    let allow = strings(&files["allowRead"]);
    for folder in [
        worktree.as_path(),
        build.as_path(),
        common.as_path(),
        git_dir.as_path(),
        home.join(".cargo").as_path(),
        home.join(".rustup").as_path(),
        home.join(".gitconfig").as_path(),
        home.join(".config/git/ignore").as_path(),
        skills.as_path(),
    ] {
        assert!(
            allow.contains(&folder.to_str().unwrap()),
            "{} is not allowed back: {allow:?}",
            folder.display()
        );
    }
    let denied = strings(&files["denyRead"]);
    let deny = strings(&s["permissions"]["deny"]);
    for path in [
        home.join("other-repo/.env"),
        home.join("other-repo/src/lib.rs"),
        home.join(".claude/projects/-x/session.jsonl"),
        home.join(".config/git/credentials"),
    ] {
        let spelled = path.to_str().unwrap();
        assert!(
            denied
                .iter()
                .any(|r| spelled.starts_with(r.trim_end_matches("/**"))),
            "its sandbox reads {spelled}: {denied:?}"
        );
        assert!(
            deny.iter().any(|r| covers(r, spelled)),
            "its tools read {spelled}: {deny:?}"
        );
    }
    for folder in [
        home.join(".cargo/registry/src/serde-1.0.2/src/lib.rs")
            .to_str()
            .unwrap(),
        home.join(".rustup/toolchains/stable/bin/rustc")
            .to_str()
            .unwrap(),
        home.join("skills/tdd/SKILL.md").to_str().unwrap(),
    ] {
        assert!(
            !deny.iter().any(|r| covers(r, folder)),
            "a readable file is under a denied rule: {folder} in {deny:?}"
        );
    }
    // A credential beside a readable toolchain stays denied by name, in the
    // spelling of each home the fence reads back whole and not only `~`'s.
    for name in ["credentials", "credentials.toml"] {
        let secret = home.join(".cargo").join(name);
        let spelled = secret.to_str().unwrap();
        assert!(denied.contains(&spelled), "{spelled}: {denied:?}");
        assert!(
            deny.iter().any(|r| covers(r, spelled)),
            "its tools read {spelled}: {deny:?}"
        );
    }
    for secret in ["~/.cargo/credentials.toml", "~/.cargo/credentials"] {
        assert!(denied.contains(&secret), "{secret}: {denied:?}");
        assert!(
            deny.contains(&format!("Read({secret})").as_str()),
            "{secret}: {deny:?}"
        );
    }
}

// A Go and web project's push hook runs `golangci-lint` from `~/go/bin`, and its
// builds read Go's module cache and run bun from `~/.bun`.
#[test]
fn a_worker_reads_go_s_and_bun_s_homes_and_builds_into_its_own_caches() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let (worktree, build) = (home.join("wt/shep/7"), home.join("targets/shep/7"));
    for rel in [
        "go/bin/golangci-lint",
        "go/pkg/mod/x/go.mod",
        "go/src/other/main.go",
        ".bun/bin/bun",
        "other-repo/.env",
    ] {
        let path = home.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "x\n").unwrap();
    }
    let s = WorkerProfile {
        worktree: &worktree,
        build: &build,
        git_common_dir: &home.join("checkouts/shep/.git"),
        git_dir: &home.join("wt/shep/7.git"),
        branch: "kelpie/7",
        base: "main",
        kelpie_pushes: false,
        kelpie: Path::new("/opt/kelpie"),
        kelpie_home: &home,
        repo: &home.join("checkouts/shep"),
        guard_hooks: &[],
        allowed_domains: &[],
        registries: &[],
        build_env: &BTreeMap::new(),
        home: &home,
        shep_home: &home,
        skills: &home.join("skills"),
        door: Path::new("/s/kelpie/dog/lease.sock"),
        token: None,
        login_token: None,
    }
    .settings();

    let allow = strings(&s["sandbox"]["filesystem"]["allowRead"]);
    for folder in ["go/bin", "go/pkg/mod", ".bun"] {
        let own = home.join(folder);
        assert!(allow.contains(&own.to_str().unwrap()), "{own:?}: {allow:?}");
    }
    for sibling in ["go", "go/src", "other-repo"] {
        let other = home.join(sibling);
        assert!(
            !allow.contains(&other.to_str().unwrap()),
            "{other:?}: {allow:?}"
        );
    }
    let deny = strings(&s["permissions"]["deny"]);
    let lint = home.join("go/bin/golangci-lint");
    assert!(
        !deny.iter().any(|r| covers(r, lint.to_str().unwrap())),
        "{lint:?} in {deny:?}"
    );
    assert_eq!(
        s["env"]["GOCACHE"],
        build.join("go-build").to_str().unwrap()
    );
    assert_eq!(
        s["env"]["GOLANGCI_LINT_CACHE"],
        build.join("golangci-lint").to_str().unwrap()
    );
    assert_eq!(
        s["env"]["BUN_INSTALL_CACHE_DIR"],
        build.join("bun-cache").to_str().unwrap()
    );
    for secret in ["~/.netrc", "~/.npmrc"] {
        let denied = strings(&s["sandbox"]["filesystem"]["denyRead"]);
        assert!(denied.contains(&secret), "{secret}: {denied:?}");
    }
}

// A `Read(...)` rule reaches `path` when the folder it names is a prefix of it
fn covers(rule: &str, path: &str) -> bool {
    let Some(inner) = rule.strip_prefix("Read(").and_then(|r| r.strip_suffix(')')) else {
        return false;
    };
    let folder = inner
        .trim_start_matches('/')
        .trim_end_matches("/**")
        .trim_end_matches('/');
    let path = path.trim_start_matches('/');
    path == folder || path.starts_with(&format!("{folder}/"))
}
