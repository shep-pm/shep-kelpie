//! Two runners on one login, one fake clock

use super::*;
use crate::forge_ledger::{self, Ledger};
use crate::ports::{
    CallPacer, CheckRunId, GateCheck, LineComment, PacedVoice, PostedComment, Voice,
};
use crate::test::{FakeClock, FakeForge};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

// A runner of `project` on `clock`, spaced and held by `hold`.
fn runner(
    forge: &FakeForge,
    clock: &FakeClock,
    hold: &ForgeHold,
    project: &str,
    ledger: &Arc<Ledger>,
) -> RateHeld {
    let source = Arc::new(forge.clone());
    RateHeld::new(
        Arc::clone(&source) as Arc<dyn Forge>,
        Arc::clone(&source) as Arc<dyn Tickets>,
        Arc::new(clock.clone()),
        hold.clone(),
        Box::new(|_| {}),
    )
    .with_ledger(Arc::clone(ledger))
    .paced_for(project)
}

#[test]
fn one_runners_used_up_answer_holds_the_other_runner() {
    let home = tempfile::tempdir().unwrap();
    let clock = FakeClock::at(1_000_000);
    let hold = ForgeHold::shared(Arc::new(LocalPace::new()));
    let (koji_forge, golbat_forge) = (
        FakeForge::new(home.path().join("koji")),
        FakeForge::new(home.path().join("golbat")),
    );
    let ledger = Arc::new(Ledger::in_home(home.path(), "x"));
    let (first, second) = (
        runner(&koji_forge, &clock, &hold, "koji", &ledger),
        runner(&golbat_forge, &clock, &hold, "golbat", &ledger),
    );
    koji_forge.set_used_up(Some(Timestamp(1_000_600)));

    first.viewer().unwrap_err();

    assert_eq!(
        second.viewer().unwrap_err(),
        ForgeError::Held(Timestamp(1_000_600))
    );
    assert_eq!(golbat_forge.asked(), 0, "the held runner never called");
    clock.advance(600);
    koji_forge.set_used_up(None);
    second.viewer().unwrap();
}

#[test]
fn calls_past_the_cap_wait_and_then_go_and_no_minute_is_over_it() {
    const CAP: usize = 6;
    let home = tempfile::tempdir().unwrap();
    let clock = FakeClock::at(1_000_000);
    let hold = ForgeHold::shared(Arc::new(LocalPace::with(CallPacer::new(CAP, 3))));
    let (koji_forge, golbat_forge) = (
        FakeForge::new(home.path().join("koji")),
        FakeForge::new(home.path().join("golbat")),
    );
    let ledger = Arc::new(Ledger::in_home(home.path(), "x"));
    let (first, second) = (
        runner(&koji_forge, &clock, &hold, "koji", &ledger),
        runner(&golbat_forge, &clock, &hold, "golbat", &ledger),
    );

    for _ in 0..20 {
        first.viewer().unwrap();
        second.viewer().unwrap();
    }

    assert_eq!(koji_forge.asked() + golbat_forge.asked(), 40, "none failed");
    assert!(clock.now().0 > 1_000_000 + 60, "the later calls waited");
    let lines = forge_ledger::read(&home.path().join(forge_ledger::FILE)).unwrap();
    let mut minutes = std::collections::BTreeMap::<u64, usize>::new();
    for line in &lines {
        *minutes.entry(line.at.0 / 60).or_default() += 1;
    }
    // The ledger marks a call by when it ended; these end as they start.
    let busiest = minutes.values().max().copied().unwrap_or(0);
    assert!(busiest <= CAP, "{minutes:?}");
    assert_eq!(lines.len(), 40);
}

#[test]
fn a_runner_with_no_project_to_pace_still_sees_the_hold() {
    let home = tempfile::tempdir().unwrap();
    let clock = FakeClock::at(1_000_000);
    let hold = ForgeHold::shared(Arc::new(LocalPace::new()));
    let forge = FakeForge::new(home.path().join("koji"));
    let source = Arc::new(forge.clone());
    let unpaced = RateHeld::new(
        Arc::clone(&source) as Arc<dyn Forge>,
        Arc::clone(&source) as Arc<dyn Tickets>,
        Arc::new(clock.clone()),
        hold.clone(),
        Box::new(|_| {}),
    );
    hold.hold(Timestamp(1_000_030));

    assert_eq!(
        unpaced.viewer().unwrap_err(),
        ForgeError::Held(Timestamp(1_000_030))
    );
}

#[test]
fn the_reset_a_used_up_answer_asks_for_takes_its_turn_in_the_pace_too() {
    const CAP: usize = 3;
    let home = tempfile::tempdir().unwrap();
    let clock = FakeClock::at(1_000_000);
    let hold = ForgeHold::shared(Arc::new(LocalPace::with(CallPacer::new(CAP, 3))));
    let forge = FakeForge::new(home.path().join("koji"));
    let ledger = Arc::new(Ledger::in_home(home.path(), "x"));
    let koji = runner(&forge, &clock, &hold, "koji", &ledger);
    koji.viewer().unwrap();
    koji.viewer().unwrap();
    // The last call of the minute is refused with no word of when to ask again.
    forge.set_used_up(None);

    koji.viewer().unwrap_err();

    let lines = forge_ledger::read(&home.path().join(forge_ledger::FILE)).unwrap();
    let mut minutes = std::collections::BTreeMap::<u64, usize>::new();
    for line in &lines {
        *minutes.entry(line.at.0 / 60).or_default() += 1;
    }
    assert_eq!(lines.len(), 4, "three calls and the reset");
    assert!(minutes.values().all(|n| *n <= CAP), "{minutes:?}");
}

// The App as a voice that mints a worker token and does nothing else, counting
// the tokens it minted.
#[derive(Default)]
struct MintingApp {
    minted: AtomicUsize,
}

impl MintingApp {
    fn minted(&self) -> usize {
        self.minted.load(Ordering::SeqCst)
    }
}

fn unscripted<T>() -> Result<T, ForgeError> {
    Err(ForgeError::App("not scripted".into()))
}

impl Voice for MintingApp {
    fn covers(&self, _repo: &ForgeSlug) -> bool {
        true
    }

    fn worker_token(&self, _repo: &ForgeSlug) -> Result<Option<InstallationToken>, ForgeError> {
        self.minted.fetch_add(1, Ordering::SeqCst);
        Ok(Some(InstallationToken::new("ghs_app".to_owned())))
    }

    fn comment(&self, _: &ForgeSlug, _: u64, _: &str) -> Result<(), ForgeError> {
        unscripted()
    }

    fn post(&self, _: &ForgeSlug, _: u64, _: &str) -> Result<PostedComment, ForgeError> {
        unscripted()
    }

    fn review(
        &self,
        _: &ForgeSlug,
        _: u64,
        _: &str,
        _: &str,
        _: &[LineComment],
    ) -> Result<u64, ForgeError> {
        unscripted()
    }

    fn edit_comment(&self, _: &ForgeSlug, _: u64, _: &str) -> Result<(), ForgeError> {
        unscripted()
    }

    fn thumbs_up(&self, _: &ForgeSlug, _: u64) -> Result<(), ForgeError> {
        unscripted()
    }

    fn create_issue(&self, _: &ForgeSlug, _: &str, _: &str, _: &[&str]) -> Result<u64, ForgeError> {
        unscripted()
    }

    fn edit_issue_body(&self, _: &ForgeSlug, _: u64, _: &str) -> Result<(), ForgeError> {
        unscripted()
    }

    fn resolve_thread(&self, _: &ForgeSlug, _: &str) -> Result<(), ForgeError> {
        unscripted()
    }

    fn reply_to_thread(&self, _: &ForgeSlug, _: &str, _: &str) -> Result<(), ForgeError> {
        unscripted()
    }

    fn create_label(&self, _: &ForgeSlug, _: &NewLabel) -> Result<(), ForgeError> {
        unscripted()
    }

    fn rename_label(&self, _: &ForgeSlug, _: &str, _: &str) -> Result<(), ForgeError> {
        unscripted()
    }

    fn set_issue_label(&self, _: &ForgeSlug, _: u64, _: &str, _: bool) -> Result<(), ForgeError> {
        unscripted()
    }

    fn check_run(&self, _: &ForgeSlug, _: &str, _: &GateCheck) -> Result<CheckRunId, ForgeError> {
        unscripted()
    }

    fn update_check_run(
        &self,
        _: &ForgeSlug,
        _: CheckRunId,
        _: &GateCheck,
    ) -> Result<(), ForgeError> {
        unscripted()
    }
}

// A runner's `gh` and App on one clock and one shared hold, with what the `gh`
// side said as each hold began.
struct Machine {
    clock: FakeClock,
    forge: FakeForge,
    gh: RateHeld,
    app: PacedVoice,
    minting: Arc<MintingApp>,
    said: Arc<Mutex<Vec<String>>>,
    _home: tempfile::TempDir,
}

impl Machine {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let clock = FakeClock::at(1_000_000);
        let hold = ForgeHold::shared(Arc::new(LocalPace::new()));
        let forge = FakeForge::new(home.path().join("koji"));
        let source = Arc::new(forge.clone());
        let said = Arc::new(Mutex::new(Vec::new()));
        let told = Arc::clone(&said);
        let gh = RateHeld::new(
            Arc::clone(&source) as Arc<dyn Forge>,
            Arc::clone(&source) as Arc<dyn Tickets>,
            Arc::new(clock.clone()),
            hold.clone(),
            Box::new(move |line| told.lock().unwrap().push(line)),
        )
        .paced_for("koji");
        let minting = Arc::new(MintingApp::default());
        let app = PacedVoice::new(
            Arc::clone(&minting) as Arc<dyn Voice>,
            Arc::new(clock.clone()),
            hold,
            "koji",
            Box::new(|_| {}),
        );
        Self {
            clock,
            forge,
            gh,
            app,
            minting,
            said,
            _home: home,
        }
    }

    fn said(&self) -> Vec<String> {
        self.said.lock().unwrap().clone()
    }
}

fn koji() -> ForgeSlug {
    ForgeSlug::try_from("shep-pm/koji".to_owned()).unwrap()
}

// gh's stderr for a call that went out with no login, and used up the
// machine's anonymous limit
const ANONYMOUS: &str = "gh: API rate limit exceeded for 203.0.113.7. (But here's the good news: \
    Authenticated requests get a higher rate limit. Check out the documentation for more \
    details.) (HTTP 403)";
const UNAUTHENTICATED: &str = "HTTP 401: Requires authentication (https://api.github.com/user)";

#[test]
fn an_anonymous_limit_holds_the_login_and_not_the_app() {
    let machine = Machine::new();
    machine.forge.set_failing(ANONYMOUS);

    machine.gh.viewer().unwrap_err();

    let until = Timestamp(1_000_000 + FALLBACK);
    assert_eq!(
        machine.gh.viewer().unwrap_err(),
        ForgeError::Held(until),
        "the login's calls are held"
    );
    assert_eq!(machine.forge.asked(), 1, "the held call never went out");
    assert!(machine.app.worker_token(&koji()).unwrap().is_some());
    assert_eq!(machine.minting.minted(), 1, "the App was asked");
    machine.clock.advance(FALLBACK);
    machine.forge.set_down(false);
    machine.gh.viewer().unwrap();
}

#[test]
fn an_anonymous_limit_is_said_as_no_login() {
    for answer in [ANONYMOUS, UNAUTHENTICATED] {
        let machine = Machine::new();
        machine.forge.set_failing(answer);

        for _ in 0..3 {
            machine.gh.viewer().unwrap_err();
        }

        let said = machine.said();
        assert_eq!(said.len(), 1, "{said:?}");
        let until = time_of(Timestamp(1_000_000 + FALLBACK));
        assert_eq!(
            said[0],
            format!(
                "gh answered without a login, so its calls are held until {until}, and the \
                 App's calls go on"
            )
        );
    }
}

#[test]
fn the_logins_own_limit_still_holds_every_call() {
    let machine = Machine::new();
    machine.forge.set_used_up(Some(Timestamp(1_000_600)));

    machine.gh.viewer().unwrap_err();

    let held = ForgeError::Held(Timestamp(1_000_600));
    assert_eq!(machine.gh.viewer().unwrap_err(), held);
    assert_eq!(machine.app.worker_token(&koji()).unwrap_err(), held);
    assert_eq!(machine.minting.minted(), 0, "the App was not asked");
}
