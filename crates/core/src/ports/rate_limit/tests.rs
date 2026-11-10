//! The hold on a source whose rate limit is used up

use super::*;

// The first is from the live runner's log; the rest are GitHub's
// documented messages, as gh prints a failed call.
#[test]
fn each_of_ghs_used_up_answers_is_a_used_up_limit() {
    let said = [
        "GraphQL: API rate limit already exceeded for user ID 1.",
        "gh: API rate limit exceeded for user ID 1. (HTTP 403)",
        "gh: You have exceeded a secondary rate limit. Please wait a few minutes before you \
         try again. (HTTP 403)",
        "gh: Too Many Requests (HTTP 429)",
    ];
    for stderr in said {
        assert!(used_up(&ForgeError::Failed(stderr.into())), "{stderr}");
    }
}

// gh's stderr for the answer an unauthenticated call gets, and for the
// one a logged-in call gets
const ANONYMOUS: &str = "gh: API rate limit exceeded for 203.0.113.7. (But here's the good \
    news: Authenticated requests get a higher rate limit. Check out the documentation for \
    more details.) (HTTP 403)";
const AUTHENTICATED: &str = "gh: API rate limit exceeded for user ID 1. (HTTP 403)";

#[test]
fn says_used_up_tells_the_anonymous_answer_apart() {
    assert_eq!(says_used_up(ANONYMOUS), Some(Limit::Anonymous));
    assert_eq!(says_used_up(AUTHENTICATED), Some(Limit::Login));
    assert_eq!(
        says_used_up("gh: You have exceeded a secondary rate limit. (HTTP 403)"),
        Some(Limit::Login)
    );
    assert_eq!(says_used_up("gh: Not Found (HTTP 404)"), None);
}

#[test]
fn any_other_failure_is_not() {
    let other = [
        ForgeError::Failed("gh: Not Found (HTTP 404)".into()),
        ForgeError::Failed("error connecting to api.github.com".into()),
        ForgeError::Unreadable("API rate limit exceeded".into()),
    ];
    for error in other {
        assert!(!used_up(&error), "{error}");
    }
}

fn bucket(used: u64, remaining: u64, reset: u64) -> crate::ports::RateBucket {
    crate::ports::RateBucket {
        used,
        remaining,
        reset: Timestamp(reset),
    }
}

fn held_over(forge: &crate::test::FakeForge) -> RateHeld {
    let source = Arc::new(forge.clone());
    RateHeld::new(
        Arc::clone(&source) as Arc<dyn Forge>,
        Arc::clone(&source) as Arc<dyn Tickets>,
        Arc::new(crate::test::FakeClock::at(1_000)),
        ForgeHold::default(),
        Box::new(|_| {}),
    )
}

#[test]
fn rate_limit_reads_used_remaining_and_reset_for_core_and_graphql() {
    let home = tempfile::tempdir().unwrap();
    let forge = crate::test::FakeForge::new(home.path().join("origin"));
    let limits = RateLimits {
        core: bucket(120, 4_880, 1_800),
        graphql: bucket(7, 4_993, 1_700),
        search: bucket(3, 27, 1_600),
    };
    forge.set_rate_limits(limits);

    assert_eq!(held_over(&forge).rate_limit(), Ok(limits));
}

#[test]
fn rate_limit_reset_still_answers_from_core_reset() {
    let home = tempfile::tempdir().unwrap();
    let forge = crate::test::FakeForge::new(home.path().join("origin"));
    let held = held_over(&forge);
    let spare = RateLimits {
        core: bucket(120, 4_880, 1_800),
        graphql: bucket(7, 4_993, 1_700),
        search: bucket(3, 27, 1_600),
    };
    forge.set_rate_limits(spare);
    assert_eq!(held.rate_limit_reset(), Ok(None), "none is used up");

    forge.set_rate_limits(RateLimits {
        core: bucket(5_000, 0, 1_800),
        ..spare
    });
    assert_eq!(held.rate_limit_reset(), Ok(Some(Timestamp(1_800))));

    forge.set_rate_limits(RateLimits {
        graphql: bucket(5_000, 0, 1_900),
        ..spare
    });
    assert_eq!(held.rate_limit_reset(), Ok(Some(Timestamp(1_900))));

    forge.set_rate_limits(RateLimits {
        search: bucket(30, 0, 1_650),
        ..spare
    });
    assert_eq!(
        held.rate_limit_reset(),
        Ok(Some(Timestamp(1_650))),
        "`gh search` spends from the search limit, so its reset holds too"
    );
}

mod cooldown {
    use super::*;
    use crate::ports::Cooldown;
    use crate::test::{FakeClock, FakeForge};

    fn repo() -> ForgeSlug {
        ForgeSlug::try_from("shep-pm/koji".to_owned()).unwrap()
    }

    fn held_on(forge: &FakeForge, clock: &FakeClock) -> RateHeld {
        let source = Arc::new(forge.clone());
        RateHeld::new(
            Arc::clone(&source) as Arc<dyn Forge>,
            Arc::clone(&source) as Arc<dyn Tickets>,
            Arc::new(clock.clone()),
            ForgeHold::default(),
            Box::new(|_| {}),
        )
    }

    #[test]
    fn a_retry_after_holds_for_that_long_and_not_for_the_reset_or_the_fallback() {
        let home = tempfile::tempdir().unwrap();
        let forge = FakeForge::new(home.path().join("origin"));
        let clock = FakeClock::at(1_000);
        let held = held_on(&forge, &clock);
        // The reset would hold for an hour, the fallback for ten minutes.
        forge.set_used_up(Some(Timestamp(4_600)));
        forge.set_limited(Cooldown {
            retry_after: Some(90),
            reset: None,
        });

        held.visibility(&repo()).unwrap_err();

        clock.advance(89);
        let held_for = held.viewer().unwrap_err();
        assert_eq!(held_for, ForgeError::Held(Timestamp(1_090)));
        forge.set_unlimited();
        forge.set_down(false);
        clock.advance(1);
        held.viewer().unwrap();
    }

    #[test]
    fn a_reset_in_the_answer_holds_until_then_without_asking_the_forge_for_it() {
        let home = tempfile::tempdir().unwrap();
        let forge = FakeForge::new(home.path().join("origin"));
        let clock = FakeClock::at(1_000);
        let held = held_on(&forge, &clock);
        forge.set_limited(Cooldown {
            retry_after: None,
            reset: Some(Timestamp(1_500)),
        });

        held.visibility(&repo()).unwrap_err();

        assert_eq!(forge.asked(), 1, "the reset was not asked for");
        assert_eq!(
            held.viewer().unwrap_err(),
            ForgeError::Held(Timestamp(1_500))
        );
    }

    #[test]
    fn an_answer_that_says_nothing_of_when_holds_for_the_fallback() {
        let home = tempfile::tempdir().unwrap();
        let forge = FakeForge::new(home.path().join("origin"));
        let clock = FakeClock::at(1_000);
        let held = held_on(&forge, &clock);
        forge.set_limited(Cooldown::default());

        held.visibility(&repo()).unwrap_err();

        assert_eq!(
            held.viewer().unwrap_err(),
            ForgeError::Held(Timestamp(1_000 + FALLBACK))
        );
    }
}

mod branch_runs {
    use super::*;
    use crate::test::{FakeClock, FakeForge};

    fn repo() -> ForgeSlug {
        ForgeSlug::try_from("shep-pm/koji".to_owned()).unwrap()
    }

    fn run(workflow: &str, id: u64, conclusion: &str) -> BranchRun {
        BranchRun {
            workflow: workflow.to_owned(),
            id,
            head: "abc".to_owned(),
            event: "push".to_owned(),
            conclusion: conclusion.to_owned(),
            created: Timestamp(1_000),
        }
    }

    fn held_on(forge: &FakeForge, clock: &FakeClock) -> RateHeld {
        let source = Arc::new(forge.clone());
        RateHeld::new(
            Arc::clone(&source) as Arc<dyn Forge>,
            Arc::clone(&source) as Arc<dyn Tickets>,
            Arc::new(clock.clone()),
            ForgeHold::default(),
            Box::new(|_| {}),
        )
    }

    #[test]
    fn each_branch_lists_the_runs_a_test_scripted_for_it_and_a_run_has_its_log_tail() {
        let home = tempfile::tempdir().unwrap();
        let forge = FakeForge::new(home.path().join("origin"));
        let held = held_on(&forge, &FakeClock::at(1_000));
        forge.set_branch_runs("kelpie/1", vec![run("test", 7, "failure")]);
        forge.set_branch_runs("kelpie/2", vec![run("test", 8, "cancelled")]);
        forge.set_run_log_tail(7, "error[E0308]");

        assert_eq!(
            held.branch_runs(&repo(), "kelpie/1").unwrap(),
            [run("test", 7, "failure")]
        );
        assert_eq!(
            held.branch_runs(&repo(), "kelpie/2").unwrap(),
            [run("test", 8, "cancelled")]
        );
        assert_eq!(held.branch_runs(&repo(), "kelpie/3").unwrap(), []);
        assert_eq!(held.run_log_tail(&repo(), 7).unwrap(), "error[E0308]");
        held.run_log_tail(&repo(), 8).unwrap_err();
    }

    #[test]
    fn a_branchs_runs_fail_with_the_error_a_test_scripted() {
        let home = tempfile::tempdir().unwrap();
        let forge = FakeForge::new(home.path().join("origin"));
        let held = held_on(&forge, &FakeClock::at(1_000));
        forge.set_branch_runs_error("gh: Not Found (HTTP 404)");

        assert_eq!(
            held.branch_runs(&repo(), "kelpie/1").unwrap_err(),
            ForgeError::Failed("gh: Not Found (HTTP 404)".into())
        );
    }

    #[test]
    fn listing_a_branchs_runs_is_held_while_the_rate_limit_is_used_up() {
        let home = tempfile::tempdir().unwrap();
        let forge = FakeForge::new(home.path().join("origin"));
        let held = held_on(&forge, &FakeClock::at(1_000));
        forge.set_limited(crate::ports::Cooldown::default());
        held.visibility(&repo()).unwrap_err();

        assert_eq!(
            held.branch_runs(&repo(), "kelpie/1").unwrap_err(),
            ForgeError::Held(Timestamp(1_000 + FALLBACK))
        );
        assert_eq!(
            held.run_log_tail(&repo(), 7).unwrap_err(),
            ForgeError::Held(Timestamp(1_000 + FALLBACK))
        );
    }
}

mod ledger {
    use super::*;
    use crate::forge_ledger::{self, Api, Line};
    use crate::test::{FakeClock, FakeForge};

    fn repo() -> ForgeSlug {
        ForgeSlug::try_from("shep-pm/koji".to_owned()).unwrap()
    }

    struct Rig {
        held: RateHeld,
        forge: FakeForge,
        clock: FakeClock,
        home: tempfile::TempDir,
    }

    impl Rig {
        fn new() -> Self {
            let home = tempfile::tempdir().unwrap();
            let forge = FakeForge::new(home.path().join("origin"));
            let clock = FakeClock::at(1_000);
            let source = Arc::new(forge.clone());
            let held = RateHeld::new(
                Arc::clone(&source) as Arc<dyn Forge>,
                Arc::clone(&source) as Arc<dyn Tickets>,
                Arc::new(clock.clone()),
                ForgeHold::default(),
                Box::new(|_| {}),
            )
            .with_ledger(Arc::new(Ledger::in_home(home.path(), "koji")));
            Self {
                held,
                forge,
                clock,
                home,
            }
        }

        fn lines(&self) -> Vec<Line> {
            forge_ledger::read(&self.home.path().join(forge_ledger::FILE)).unwrap()
        }
    }

    #[test]
    fn each_call_adds_a_line_with_its_kind_and_how_it_ended() {
        let rig = Rig::new();
        rig.held.visibility(&repo()).unwrap();
        rig.clock.advance(30);
        rig.held.default_branch(&repo()).unwrap();
        rig.forge.set_down(true);
        rig.clock.advance(30);
        rig.held.repo_labels(&repo()).unwrap_err();

        let lines = rig.lines();
        let seen: Vec<_> = lines
            .iter()
            .map(|l| (l.at.0, l.project.as_str(), l.kind, l.api, l.ended))
            .collect();
        assert_eq!(
            seen,
            [
                (1_000, "koji", Kind::Visibility, Api::GraphQl, Ended::Ok),
                (1_030, "koji", Kind::DefaultBranch, Api::GraphQl, Ended::Ok),
                (1_060, "koji", Kind::RepoLabels, Api::GraphQl, Ended::Failed),
            ]
        );
    }

    #[test]
    fn the_reads_that_came_back_304_are_put_on_the_line_of_the_call_they_were_made_in() {
        let revalidated = Revalidated::default();
        let rig = Rig::new();
        let held = rig.held.counting(revalidated.clone());

        held.viewer().unwrap();
        revalidated.note();
        revalidated.note();
        held.visibility(&repo()).unwrap();
        held.default_branch(&repo()).unwrap();

        let lines = forge_ledger::read(&rig.home.path().join(forge_ledger::FILE)).unwrap();
        let counts: Vec<_> = lines.iter().map(|l| l.not_modified).collect();
        assert_eq!(counts, [0, 2, 0]);
    }

    #[test]
    fn a_used_up_answer_is_recorded_and_the_calls_it_holds_are_recorded_as_held() {
        let rig = Rig::new();
        rig.forge.set_used_up(Some(Timestamp(1_600)));
        rig.held.visibility(&repo()).unwrap_err();
        rig.clock.advance(60);
        rig.held.viewer().unwrap_err();
        rig.forge.set_down(false);
        rig.clock.advance(600);
        rig.held.viewer().unwrap();

        let lines = rig.lines();
        let seen: Vec<_> = lines.iter().map(|l| (l.at.0, l.kind, l.ended)).collect();
        assert_eq!(
            seen,
            [
                (1_000, Kind::Visibility, Ended::UsedUp),
                (1_000, Kind::RateLimitReset, Ended::Ok),
                (1_060, Kind::Viewer, Ended::Held),
                (1_660, Kind::Viewer, Ended::Ok),
            ]
        );
        assert_eq!(lines[1].api, Api::Rest);
        assert_eq!(lines[1].millis, 0);
    }
}
