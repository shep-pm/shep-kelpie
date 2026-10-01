use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;

use super::fleet::merge_in_flight;
use super::host::{cargo_args, pick_commit};
use super::*;

// One machine and one shepherd in a single rig, so that what happens to the
// files, the sheep and the clock lands in one ordered log.
struct Rig {
    home: tempfile::TempDir,
    log: Mutex<Vec<String>>,
    shepherd: String,
    members: Vec<Member>,
    // What each build, named by its bytes, is built for: the shepherd's line by default
    built_for: Mutex<BTreeMap<String, String>>,
    // Polls a runner still has a merge in flight for
    merging: Mutex<BTreeMap<String, u32>>,
    // Polls a restarted sheep takes to answer
    slow: Mutex<BTreeMap<String, u32>>,
    alive: BTreeSet<u32>,
}

impl Rig {
    // An install whose link points at `kelpie-old`, with a dog and two runners all running through it.
    fn new() -> Self {
        let rig = Self::bare();
        let install = rig.install();
        let old = install.builds().join("kelpie-old");
        std::fs::create_dir_all(install.builds()).unwrap();
        write_build(&old, "old");
        install.relink(&old).unwrap();
        rig
    }

    fn bare() -> Self {
        let home = tempfile::tempdir().unwrap();
        let link = Install::under(home.path()).link();
        let member = |name: &str, kind| Member {
            name: name.to_owned(),
            kind,
            program: link.clone(),
            online: true,
        };
        Self {
            home,
            log: Mutex::default(),
            shepherd: "0.12.2".into(),
            members: vec![
                member("kelpie", Kind::Dog),
                member("koji", Kind::Runner),
                member("rotom", Kind::Runner),
            ],
            built_for: Mutex::default(),
            merging: Mutex::default(),
            slow: Mutex::default(),
            alive: BTreeSet::new(),
        }
    }

    fn install(&self) -> Install {
        Install::under(self.home.path())
    }

    fn upgrade(&self, job: &Job) -> Result<Vec<String>, String> {
        run(job, &self.install(), 100, self, self)
    }

    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    fn record(&self, event: impl Into<String>) {
        self.log.lock().unwrap().push(event.into());
    }

    fn built_for(&self, bytes: &str, version: &str) {
        self.built_for
            .lock()
            .unwrap()
            .insert(bytes.to_owned(), version.to_owned());
    }

    fn busy(&self, runner: &str, polls: u32) {
        self.merging
            .lock()
            .unwrap()
            .insert(runner.to_owned(), polls);
    }

    fn answers_after(&self, sheep: &str, polls: u32) {
        self.slow.lock().unwrap().insert(sheep.to_owned(), polls);
    }

    fn target(&self, link: PathBuf) -> String {
        std::fs::read_link(link)
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }
}

fn write_build(path: &Path, bytes: &str) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl Machine for Rig {
    fn build(&self, source: &Source, staging: &Path) -> Result<PathBuf, String> {
        let (Source::Release(name) | Source::Ref(name)) = source else {
            panic!("a binary is not built");
        };
        self.record(format!("build {name}"));
        let bin = staging.join("root/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let path = bin.join("shep-kelpie");
        write_build(&path, &format!("new {name}"));
        Ok(path)
    }

    fn shep_version(&self, binary: &Path) -> Result<String, String> {
        let bytes = std::fs::read_to_string(binary).unwrap();
        Ok(self
            .built_for
            .lock()
            .unwrap()
            .get(&bytes)
            .cloned()
            .unwrap_or_else(|| self.shepherd.clone()))
    }

    fn sign(&self, binary: &Path) -> Result<(), String> {
        let name = binary.file_name().unwrap().to_string_lossy();
        self.record(format!("sign {name}"));
        Ok(())
    }

    fn alive(&self, pid: u32) -> bool {
        self.alive.contains(&pid)
    }

    fn sleep(&self, time: Duration) {
        self.record(format!("sleep {}s", time.as_secs()));
    }
}

impl Fleet for Rig {
    fn shepherd_version(&self) -> Result<String, String> {
        Ok(self.shepherd.clone())
    }

    fn members(&self) -> Result<Vec<Member>, String> {
        Ok(self.members.clone())
    }

    fn merging(&self, name: &str) -> Result<bool, String> {
        let mut busy = self.merging.lock().unwrap();
        let polls = busy.entry(name.to_owned()).or_default();
        let merging = *polls > 0;
        *polls = polls.saturating_sub(1);
        Ok(merging)
    }

    fn restart(&self, member: &Member) -> Result<(), String> {
        self.record(format!("restart {}", member.name));
        Ok(())
    }

    fn up(&self, member: &Member) -> Result<bool, String> {
        let mut slow = self.slow.lock().unwrap();
        let polls = slow.entry(member.name.clone()).or_default();
        let up = *polls == 0;
        *polls = polls.saturating_sub(1);
        Ok(up)
    }
}

fn main_ref() -> Job {
    Job::Install(Source::Ref("main".into()))
}

#[test]
fn an_upgrade_keeps_the_build_relinks_and_restarts_the_dog_then_each_runner() {
    let rig = Rig::new();
    let lines = rig.upgrade(&main_ref()).unwrap();
    assert_eq!(
        rig.log(),
        [
            "build main",
            "sign kelpie-main",
            "restart kelpie",
            "restart koji",
            "restart rotom",
        ]
    );
    let install = rig.install();
    assert_eq!(rig.target(install.link()), "kelpie-main");
    assert_eq!(rig.target(install.previous()), "kelpie-old");
    assert_eq!(
        lines.last().map(String::as_str),
        Some("restarted rotom"),
        "{lines:?}"
    );
}

#[test]
fn an_upgrade_waits_out_a_merge_in_flight() {
    let rig = Rig::new();
    rig.busy("koji", 3);
    rig.upgrade(&main_ref()).unwrap();
    assert_eq!(
        rig.log()[2..],
        [
            "restart kelpie",
            "sleep 5s",
            "sleep 5s",
            "sleep 5s",
            "restart koji",
            "restart rotom",
        ]
    );
}

#[test]
fn a_merge_that_never_ends_stops_the_upgrade_before_that_runner() {
    let rig = Rig::new();
    rig.busy("koji", u32::MAX);
    let err = rig.upgrade(&main_ref()).unwrap_err();
    assert!(
        err.contains("koji still has a merge in flight after 30 minutes"),
        "{err}"
    );
    assert!(err.contains("Restarted: kelpie."), "{err}");
    assert!(
        err.contains("Still on the old build: koji, rotom."),
        "{err}"
    );
    assert!(err.contains("`shep kelpie upgrade --rollback`"), "{err}");
    assert!(!rig.log().contains(&"restart koji".to_owned()));
}

#[test]
fn a_minor_mismatch_stops_before_anything_restarts_and_names_the_steps() {
    let rig = Rig::new();
    rig.built_for("new main", "0.13.0");
    let err = rig.upgrade(&main_ref()).unwrap_err();
    let kept = rig.install().builds().join("kelpie-main");
    let kept = kept.display();
    assert!(
        err.contains(&format!(
            "{kept} is built for shep 0.13.0 and the shepherd runs shep 0.12.2"
        )),
        "{err}"
    );
    assert!(
        err.contains(&format!(
            "upgrade shep and restart its shepherd, then run `{kept} upgrade --binary {kept}`"
        )),
        "{err}"
    );
    assert_eq!(rig.log(), ["build main", "sign kelpie-main"]);
    assert_eq!(rig.target(rig.install().link()), "kelpie-old");
    assert!(std::fs::read_link(rig.install().previous()).is_err());
}

#[test]
fn a_build_that_does_not_say_its_shep_is_refused_as_too_old() {
    struct Mute(Rig);
    impl Machine for Mute {
        fn build(&self, source: &Source, staging: &Path) -> Result<PathBuf, String> {
            self.0.build(source, staging)
        }
        fn shep_version(&self, _: &Path) -> Result<String, String> {
            Err("it exited 2".into())
        }
        fn sign(&self, binary: &Path) -> Result<(), String> {
            self.0.sign(binary)
        }
        fn alive(&self, pid: u32) -> bool {
            self.0.alive(pid)
        }
        fn sleep(&self, time: Duration) {
            self.0.sleep(time);
        }
    }
    let rig = Mute(Rig::new());
    let err = run(&main_ref(), &rig.0.install(), 100, &rig, &rig.0).unwrap_err();
    assert!(
        err.contains("did not say which shep it is built for (it exited 2)"),
        "{err}"
    );
    assert!(!rig.0.log().iter().any(|e| e.starts_with("restart")));
}

#[test]
fn a_binary_without_exec_bits_is_refused_with_the_fix() {
    let rig = Rig::new();
    let binary = rig.home.path().join("kelpie-new");
    std::fs::write(&binary, "new").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o644)).unwrap();
    let err = rig
        .upgrade(&Job::Install(Source::Binary(binary.clone())))
        .unwrap_err();
    assert!(
        err.contains(&format!("run `chmod +x {}`", binary.display())),
        "{err}"
    );
    assert_eq!(
        std::fs::metadata(&binary).unwrap().permissions().mode() & 0o777,
        0o644
    );
    assert!(!rig.install().builds().join("kelpie-kelpie-new").exists());
    assert_eq!(rig.log(), Vec::<String>::new());
}

#[test]
fn a_second_upgrade_is_refused_while_one_runs() {
    let mut rig = Rig::new();
    rig.alive.insert(4242);
    std::fs::write(rig.install().lock(), "4242\n").unwrap();
    let err = rig.upgrade(&main_ref()).unwrap_err();
    assert!(
        err.contains("an upgrade is already in progress (process 4242)"),
        "{err}"
    );
    assert_eq!(rig.log(), Vec::<String>::new());
    assert_eq!(rig.target(rig.install().link()), "kelpie-old");
    assert_eq!(
        std::fs::read_to_string(rig.install().lock()).unwrap(),
        "4242\n"
    );
}

#[test]
fn a_lock_with_no_pid_yet_is_never_taken_from_its_holder() {
    let rig = Rig::new();
    std::fs::write(rig.install().lock(), "").unwrap();
    let err = rig.upgrade(&main_ref()).unwrap_err();
    assert!(err.contains("holds no process id"), "{err}");
    assert!(rig.install().lock().exists());
    assert_eq!(rig.log(), Vec::<String>::new());
}

#[test]
fn a_lock_whose_holder_is_gone_is_cleared_and_released_when_the_upgrade_ends() {
    let rig = Rig::new();
    std::fs::write(rig.install().lock(), "4242\n").unwrap();
    rig.upgrade(&main_ref()).unwrap();
    assert!(!rig.install().lock().exists());

    rig.built_for("new again", "0.13.0");
    rig.upgrade(&Job::Install(Source::Ref("again".into())))
        .unwrap_err();
    assert!(!rig.install().lock().exists());
}

#[test]
fn a_rollback_puts_the_previous_build_back_and_restarts_onto_it() {
    let rig = Rig::new();
    rig.upgrade(&main_ref()).unwrap();
    let before = rig.log().len();
    let lines = rig.upgrade(&Job::Rollback).unwrap();
    let install = rig.install();
    assert_eq!(rig.target(install.link()), "kelpie-old");
    assert_eq!(rig.target(install.previous()), "kelpie-main");
    assert_eq!(
        rig.log()[before..],
        ["restart kelpie", "restart koji", "restart rotom"]
    );
    assert_eq!(lines.last().map(String::as_str), Some("restarted rotom"));
}

#[test]
fn a_rollback_with_no_previous_build_says_so() {
    let rig = Rig::new();
    let err = rig.upgrade(&Job::Rollback).unwrap_err();
    assert!(err.contains("no previous build to go back to"), "{err}");
    assert_eq!(rig.log(), Vec::<String>::new());
}

#[test]
fn a_rollback_across_a_minor_stops_the_same_way() {
    let rig = Rig::new();
    rig.upgrade(&main_ref()).unwrap();
    rig.built_for("old", "0.11.0");
    let before = rig.log().len();
    let err = rig.upgrade(&Job::Rollback).unwrap_err();
    assert!(
        err.contains("is built for shep 0.11.0 and the shepherd runs shep 0.12.2"),
        "{err}"
    );
    assert_eq!(rig.log().len(), before);
    assert_eq!(rig.target(rig.install().link()), "kelpie-main");
}

#[test]
fn a_sheep_that_does_not_run_through_the_link_stops_everything_first() {
    let mut rig = Rig::new();
    rig.members[1].program = "/usr/local/bin/kelpie".into();
    let err = rig.upgrade(&main_ref()).unwrap_err();
    assert!(
        err.starts_with("koji runs /usr/local/bin/kelpie, not "),
        "{err}"
    );
    assert_eq!(rig.log(), Vec::<String>::new());
    assert_eq!(rig.target(rig.install().link()), "kelpie-old");
}

#[test]
fn a_sheep_that_is_not_running_takes_the_new_build_when_it_starts() {
    let mut rig = Rig::new();
    rig.members[1].online = false;
    let lines = rig.upgrade(&main_ref()).unwrap();
    assert!(!rig.log().contains(&"restart koji".to_owned()));
    assert!(
        lines.contains(&"koji is not running, so it takes the new build when it starts".to_owned())
    );
    assert!(rig.log().contains(&"restart rotom".to_owned()));
}

#[test]
fn a_restart_that_never_answers_stops_before_the_next_sheep() {
    let rig = Rig::new();
    rig.answers_after("koji", u32::MAX);
    let err = rig.upgrade(&main_ref()).unwrap_err();
    assert!(
        err.contains("koji did not answer in 90 seconds after its restart"),
        "{err}"
    );
    assert!(err.contains("Restarted: kelpie, koji."), "{err}");
    assert!(err.contains("Still on the old build: rotom."), "{err}");
    assert!(!rig.log().contains(&"restart rotom".to_owned()));
}

#[test]
fn a_slow_restart_is_waited_for() {
    let rig = Rig::new();
    rig.answers_after("kelpie", 2);
    rig.upgrade(&main_ref()).unwrap();
    assert_eq!(rig.log()[2..5], ["restart kelpie", "sleep 1s", "sleep 1s"]);
}

#[test]
fn the_installed_build_again_restarts_nothing() {
    let rig = Rig::new();
    let old = rig.install().builds().join("kelpie-old");
    let lines = rig.upgrade(&Job::Install(Source::Binary(old))).unwrap();
    assert!(
        lines
            .last()
            .unwrap()
            .ends_with("is already the installed build"),
        "{lines:?}"
    );
    assert_eq!(rig.log(), Vec::<String>::new());
}

#[test]
fn a_build_made_elsewhere_is_kept_under_its_own_name() {
    let rig = Rig::new();
    let binary = rig.home.path().join("kelpie-0.2.0");
    write_build(&binary, "new elsewhere");
    rig.upgrade(&Job::Install(Source::Binary(binary))).unwrap();
    assert_eq!(rig.target(rig.install().link()), "kelpie-kelpie-0.2.0");
    assert_eq!(rig.log()[0], "sign kelpie-kelpie-0.2.0");
}

#[test]
fn a_taken_name_is_never_overwritten() {
    let rig = Rig::new();
    let install = rig.install();
    let (first, second) = (rig.home.path().join("a"), rig.home.path().join("b"));
    write_build(&first, "first");
    write_build(&second, "second");
    let (one, fresh) = install.keep(&first, "main").unwrap();
    let (again, reused) = install.keep(&first, "main").unwrap();
    let (two, _) = install.keep(&second, "main").unwrap();
    assert_eq!((fresh, reused), (true, false));
    assert_eq!(one, again);
    assert_eq!(two.file_name().unwrap(), "kelpie-main-2");
    assert_eq!(std::fs::read_to_string(one).unwrap(), "first");
    assert_eq!(std::fs::read_to_string(two).unwrap(), "second");
}

#[test]
fn an_install_with_no_link_says_how_to_make_one() {
    let rig = Rig::bare();
    let err = rig.upgrade(&main_ref()).unwrap_err();
    assert!(err.contains("is not a link to a build"), "{err}");
    assert_eq!(rig.log(), Vec::<String>::new());
}

#[test]
fn the_arguments_name_one_source_or_a_rollback() {
    let words = |line: &str| line.split(' ').map(str::to_owned).collect::<Vec<_>>();
    assert_eq!(
        parse(&words("--release v0.2.0")),
        Ok(Job::Install(Source::Release("v0.2.0".into())))
    );
    assert_eq!(
        parse(&words("--ref feature/x")),
        Ok(Job::Install(Source::Ref("feature/x".into())))
    );
    assert_eq!(
        parse(&words("--binary /tmp/k")),
        Ok(Job::Install(Source::Binary("/tmp/k".into())))
    );
    assert_eq!(parse(&words("--rollback")), Ok(Job::Rollback));
    for refused in [
        "",
        "--release",
        "--ref --rollback",
        "--release a --ref b",
        "v0.2.0",
        "--rollback x",
    ] {
        let args: Vec<String> = refused.split_whitespace().map(str::to_owned).collect();
        assert_eq!(parse(&args), Err(USAGE.to_owned()), "{refused:?}");
    }
}

#[test]
fn a_release_or_ref_builds_in_the_staging_folder_with_the_lockfile() {
    let staging = Path::new("/k/upgrade");
    let args = |source| {
        cargo_args("https://example.test/kelpie", &source, staging)
            .unwrap()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    };
    assert_eq!(
        args(Source::Release("v0.2.0".into())),
        "install --git https://example.test/kelpie --locked --force --tag v0.2.0 \
         --root /k/upgrade/root --target-dir /k/upgrade/target"
    );
    assert_eq!(
        args(Source::Ref("abc123".into())),
        "install --git https://example.test/kelpie --locked --force --rev abc123 \
         --root /k/upgrade/root --target-dir /k/upgrade/target"
    );
    assert!(cargo_args("r", &Source::Binary("/x".into()), staging).is_err());
}

#[test]
fn a_branch_is_a_commit_and_a_tag_is_the_commit_it_points_at() {
    let listing = "aaa\trefs/heads/v1\nbbb\trefs/tags/v1\nccc\trefs/tags/v1^{}\n\
                   ddd\trefs/tags/v2\neee\trefs/tags/v2^{}\nfff\trefs/tags/v3\n";
    assert_eq!(pick_commit(listing, "v1").as_deref(), Some("aaa"));
    assert_eq!(pick_commit(listing, "v2").as_deref(), Some("eee"));
    assert_eq!(pick_commit(listing, "v3").as_deref(), Some("fff"));
    assert_eq!(pick_commit(listing, "v4"), None);
    assert_eq!(pick_commit("", "v1"), None);
}

#[test]
fn a_status_with_a_work_item_merging_or_queued_is_a_merge_in_flight() {
    let merging = |item: &str| merge_in_flight(&format!(r#"{{"work_items":[{item}]}}"#));
    assert_eq!(merging(""), Ok(false));
    assert_eq!(
        merging(r#"{"phase":{"state":"ci"},"merge_queued":null}"#),
        Ok(false)
    );
    assert_eq!(merging(r#"{"phase":{"state":"ci"}}"#), Ok(false));
    assert_eq!(
        merging(r#"{"phase":{"state":"merge","head":"a"}}"#),
        Ok(true)
    );
    assert_eq!(
        merging(r#"{"phase":{"state":"ci"},"merge_queued":{"removals":0,"since":4}}"#),
        Ok(true)
    );
    assert_eq!(
        merging(r#"{"phase":{"state":"ci"}},{"phase":{"state":"merge","head":"a"}}"#),
        Ok(true)
    );
    assert!(merge_in_flight(r#"{"run":"paused"}"#).is_err());
    assert!(merge_in_flight("starting").is_err());
}

mod shepherd {
    use shep_client::shep_core::config::AppConfig;
    use shep_client::shep_core::protocol::{Request, SelectorSpec};

    use super::*;
    use crate::flock::Launch;
    use crate::flock::upgrade::fleet::Shepherd;
    use crate::runner::ProjectName;
    use crate::test::FakeShepherd;

    const EXAMPLE: &str = include_str!("../../../settings.example.toml");

    fn runner(shepherd: &FakeShepherd, name: &str, program: &str, online: bool) {
        let launch = Launch {
            kelpie: program.into(),
            shep_home: shepherd.home().to_owned(),
            kelpie_home: None,
        };
        let table = crate::test::project_table(EXAMPLE);
        let name = ProjectName::try_from(name).unwrap();
        shepherd.holds(launch.runner(&name, table), online);
    }

    // The adapter makes its own runtime, so it runs where a test's runtime is not entered.
    async fn asked<T: Send + 'static>(
        shepherd: &Shepherd,
        ask: impl FnOnce(&Shepherd) -> T + Send + 'static,
    ) -> T {
        let shepherd = shepherd.clone();
        tokio::time::timeout(
            Duration::from_secs(40),
            tokio::task::spawn_blocking(move || ask(&shepherd)),
        )
        .await
        .expect("the shepherd neither answered nor failed in time")
        .unwrap()
    }

    #[tokio::test]
    async fn the_dog_and_the_runners_are_listed_with_their_programs() {
        let shepherd = FakeShepherd::new().await;
        shepherd.holds_dog("kelpie", true);
        runner(&shepherd, "koji", "/k/bin/kelpie", true);
        runner(&shepherd, "rotom", "/k/bin/kelpie", false);
        shepherd.holds(AppConfig::minimal("web", "/srv/web"), true);
        let fleet = Shepherd::at(shepherd.home().to_owned());
        let members = asked(&fleet, |f| f.members().unwrap()).await;
        let seen: Vec<_> = members
            .iter()
            .map(|m| {
                (
                    m.name.as_str(),
                    m.kind,
                    m.program.to_str().unwrap(),
                    m.online,
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                ("kelpie", Kind::Dog, "/opt/kelpie", true),
                ("koji", Kind::Runner, "/k/bin/kelpie", true),
                ("rotom", Kind::Runner, "/k/bin/kelpie", false),
            ]
        );
    }

    #[tokio::test]
    async fn a_restart_asks_the_shepherd_to_restart_that_sheep() {
        let mut shepherd = FakeShepherd::new().await;
        runner(&shepherd, "koji", "/k/bin/kelpie", true);
        let fleet = Shepherd::at(shepherd.home().to_owned());
        let member = Member {
            name: "koji".into(),
            kind: Kind::Runner,
            program: "/k/bin/kelpie".into(),
            online: true,
        };
        asked(&fleet, move |f| f.restart(&member).unwrap()).await;
        assert!(matches!(
            shepherd.writes().as_slice(),
            [Request::Restart { selector: SelectorSpec::Name(name) }] if name == "koji"
        ));
    }

    #[tokio::test]
    async fn a_shepherd_on_another_minor_still_says_which_one_it_runs() {
        let shepherd = FakeShepherd::on("0.13.1").await;
        let fleet = Shepherd::at(shepherd.home().to_owned());
        assert_eq!(
            asked(&fleet, |f| f.shepherd_version()).await,
            Ok("0.13.1".to_owned())
        );
    }

    #[tokio::test]
    async fn no_shepherd_is_named_by_its_home() {
        let home = tempfile::tempdir().unwrap();
        let fleet = Shepherd::at(home.path().to_owned());
        let err = asked(&fleet, |f| f.shepherd_version()).await.unwrap_err();
        assert!(
            err.starts_with("cannot reach kelpie's shepherd at "),
            "{err}"
        );
    }
}
