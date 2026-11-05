//! The dog's desk: what it makes of what shep tells it
//!
//! Pure. Runner notes and runner processes go in, and the grants the
//! dog must deliver come out. The maintainer's triggers are answered in
//! [`super::triggers`]. The shell in [`super`] feeds it from shep and
//! delivers its grants as triggers.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;

use crate::home::ProjectName;
use crate::lease::book::{Asked, Grant, LeaseBook};
use crate::lease::counted::{CARGO_TEST_CAPACITY, CountedLease, Taken, Taker, Ticket};
use crate::lease::saved::{SavedBook, SavedRun, SavedRunId, SavedTotals};
use crate::lease::wire::{Total, TotalName, Totals, WindowFact, WindowName};
use crate::lease::{Epoch, Holder, LeaseKind};
use crate::metrics::Recorder;
use crate::ports::{Clock, Timestamp};
use crate::review_bot::Reviewers;

use super::watch::Watch;

/// A grant the dog must deliver to a runner as a `grant` trigger
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    /// The runner's sheep
    pub project: ProjectName,
    /// What it is granted
    pub kind: LeaseKind,
    /// Which run asked
    pub epoch: Epoch,
}

// One run of a runner and the totals it has raised.
#[derive(Debug)]
struct Run {
    epoch: Epoch,
    totals: BTreeMap<LeaseKind, Totals>,
    // The ignored asks already said, so a runner that asks again says nothing new.
    said: BTreeSet<(LeaseKind, bool, u64)>,
}

/// What the dog made of one note
#[derive(Debug, Default)]
pub struct Heard {
    /// The grants it made
    pub grants: Vec<Delivery>,
    /// A line for the log when the note was an ask or a return the dog did
    /// not act on, once for each
    pub ignored: Option<String>,
}

/// How many replaced runs the desk remembers per runner, newest kept. A
/// late note comes from a run replaced moments before, so a few cover it,
/// and the book file stops growing with every restart.
const RETIRED_PER_RUNNER: usize = 8;

// Runs already replaced, per runner, oldest first. Pids are not ordered,
// so a late note from one is known only by having seen it retired.
#[derive(Debug, Default)]
struct Retired(BTreeMap<String, VecDeque<Epoch>>);

impl Retired {
    fn contains(&self, sheep: &str, epoch: Epoch) -> bool {
        self.0.get(sheep).is_some_and(|runs| runs.contains(&epoch))
    }

    fn insert(&mut self, sheep: &str, epoch: Epoch) {
        let runs = self.0.entry(sheep.to_owned()).or_default();
        runs.retain(|e| *e != epoch);
        runs.push_back(epoch);
        if runs.len() > RETIRED_PER_RUNNER {
            runs.pop_front();
        }
    }

    fn remove(&mut self, sheep: &str, epoch: Epoch) {
        if let Some(runs) = self.0.get_mut(sheep) {
            runs.retain(|e| *e != epoch);
        }
    }
}

/// The lease book and everything the dog knows about each runner's run
#[derive(Debug)]
pub struct Desk {
    pub(super) book: LeaseBook,
    pub(super) tests: CountedLease,
    // One lease per e2e host, made at its first ask.
    pub(super) e2e: BTreeMap<String, CountedLease>,
    runs: HashMap<String, Run>,
    retired: Retired,
    pub(super) metrics: Arc<Recorder>,
    pub(super) watch: Watch,
}

impl Desk {
    /// A desk with an empty book, with a window for each reviewer in
    /// `reviewers`, sending its numbers through `metrics`
    pub fn new(clock: Box<dyn Clock>, reviewers: Reviewers, metrics: Arc<Recorder>) -> Self {
        Self::restore(
            clock,
            SavedBook::new(Vec::new(), Vec::new(), Vec::new()),
            reviewers,
            metrics,
        )
    }

    /// A desk holding what [`Desk::saved`] kept, with each window in
    /// `reviewers` as it defines it
    ///
    /// Its runners are as they were when it was saved: a [`Desk::resync`]
    /// against the live flock reclaims from those since gone or restarted.
    pub fn restore(
        clock: Box<dyn Clock>,
        saved: SavedBook,
        reviewers: Reviewers,
        metrics: Arc<Recorder>,
    ) -> Self {
        let watch = Watch::counted(&saved.leases);
        let mut book = LeaseBook::restore(clock, saved.leases);
        book.set_reviewers(reviewers);
        let runs = saved
            .runs
            .into_iter()
            .map(|run| {
                let totals = run.totals.into_iter().map(|t| {
                    let totals = Totals {
                        want: t.want,
                        give_back: t.give_back,
                    };
                    (t.kind, totals)
                });
                let restored = Run {
                    epoch: run.epoch,
                    totals: totals.collect(),
                    said: BTreeSet::new(),
                };
                (run.sheep, restored)
            })
            .collect();
        let mut retired = Retired::default();
        for run in saved.retired {
            retired.insert(&run.sheep, run.epoch);
        }
        let mut desk = Self {
            book,
            tests: CountedLease::cargo_test(CARGO_TEST_CAPACITY),
            e2e: BTreeMap::new(),
            runs,
            retired,
            metrics,
            watch,
        };
        // What the saved book holds is not granted again.
        desk.look(false);
        desk
    }

    /// Lets up to `capacity` hold the `cargo-test` lease at once, and returns
    /// the waiters a raise granted. A lower capacity only slows new grants.
    pub fn set_test_capacity(&mut self, capacity: std::num::NonZeroU32) -> Vec<Ticket> {
        let granted = self.tests.set_capacity(capacity, self.book.now());
        self.look(true);
        granted
    }

    /// Asks for the `cargo-test` lease on behalf of `taker`
    pub fn take_test(&mut self, taker: Taker) -> Taken {
        let taken = self.tests.ask(taker, self.book.now());
        self.look(true);
        taken
    }

    /// Seats `taker` in the `cargo-test` lease at once, for a command
    /// already running under a grant from a dog that went away
    pub fn seat_test(&mut self, taker: Taker) -> Taken {
        let taken = self.tests.seat(taker, self.book.now());
        self.look(false);
        taken
    }

    /// Gives the `cargo-test` lease back, or leaves its queue, for the
    /// connection `ticket`, and returns the waiters granted after it
    pub fn leave_test(&mut self, ticket: Ticket) -> Vec<Ticket> {
        let granted = self.tests.leave(ticket, self.book.now());
        self.look(true);
        granted
    }

    /// Everything the dog keeps across a restart, in a stable order
    pub fn saved(&self) -> SavedBook {
        let mut runs: Vec<SavedRun> = self
            .runs
            .iter()
            .map(|(sheep, run)| SavedRun {
                sheep: sheep.clone(),
                epoch: run.epoch,
                totals: run
                    .totals
                    .iter()
                    .map(|(kind, totals)| SavedTotals {
                        kind: kind.clone(),
                        want: totals.want,
                        give_back: totals.give_back,
                    })
                    .collect(),
            })
            .collect();
        runs.sort_by(|a, b| a.sheep.cmp(&b.sheep));
        let retired = self.retired.0.iter().flat_map(|(sheep, runs)| {
            runs.iter().map(|epoch| SavedRunId {
                sheep: sheep.clone(),
                epoch: *epoch,
            })
        });
        SavedBook::new(self.book.saved(), runs, retired.collect())
    }

    /// Takes one lease or window total from sheep `sheep`, sent as a note,
    /// or as a metric sample by a runner on a build from before notes
    ///
    /// Anything that is not a lease total from a runner is ignored. A new
    /// epoch reclaims what the runner's earlier run held.
    pub fn note(&mut self, sheep: &str, name: &str, value: f64) -> Vec<Delivery> {
        self.heard(sheep, name, value).grants
    }

    /// Takes one total as [`Desk::note`] does, and says which a
    /// run's ask or return it did not act on
    pub fn heard(&mut self, sheep: &str, name: &str, value: f64) -> Heard {
        let heard = self.hear(sheep, name, value);
        self.look(true);
        heard
    }

    fn hear(&mut self, sheep: &str, name: &str, value: f64) -> Heard {
        let (Ok(project), Some(value)) = (ProjectName::try_from(sheep), count(value)) else {
            return Heard::default();
        };
        if let Some(fact) = WindowName::parse(name) {
            let Some(mut grants) = self.run_of(sheep, &project, fact.epoch) else {
                return Heard::default();
            };
            let at = Timestamp(value);
            let window = match fact.fact {
                WindowFact::Summoned => {
                    if self.book.is_live_summon(&fact.kind, at) {
                        self.watch.summoned(&self.metrics, fact.kind.as_str(), at);
                    }
                    self.book.summoned(&fact.kind, at)
                }
                WindowFact::Opens => {
                    if self.book.is_live_refusal(&fact.kind, at) {
                        self.watch.refused(&self.metrics, fact.kind.as_str(), at);
                    }
                    self.book.refused(&fact.kind, at)
                }
                WindowFact::Quota(per_hour) => self.book.quota(&fact.kind, per_hour, at),
            };
            grants.extend(window);
            return Heard {
                grants: deliveries(grants),
                ignored: None,
            };
        }
        let Some(said) = TotalName::parse(name) else {
            return Heard::default();
        };
        let Some(mut grants) = self.run_of(sheep, &project, said.epoch) else {
            return Heard::default();
        };
        let run = self.runs.get_mut(sheep).expect("run_of keeps the run");
        let holder = Holder::Runner {
            project,
            epoch: said.epoch,
        };
        let kind = said.kind;
        let totals = run.totals.entry(kind.clone()).or_default();
        let mut ignored = None;
        match said.total {
            Total::Want if value > totals.want => {
                totals.want = value;
                let asking = totals.asking();
                // A runner asks only when it holds nothing, so a return was dropped.
                if self.book.holder(&kind) == Some(&holder) {
                    grants.extend(self.book.give_back(&kind, &holder));
                }
                if asking && self.book.ask(&kind, holder.clone()) == Asked::Granted {
                    grants.push(Grant { kind, holder });
                }
            }
            Total::Return if value > totals.give_back => {
                totals.give_back = value;
                grants.extend(self.book.give_back(&kind, &holder));
            }
            // A runner sends every kind's totals again while any one of its asks
            // stands, so a want equal to the recorded one is no mismatch.
            Total::Want if value == totals.want => {}
            Total::Want => {
                let why = format!("the dog recorded {} for this run", totals.want);
                ignored = Some((kind, true, value, why));
            }
            Total::Return if value == totals.give_back => {}
            Total::Return => {
                let why = format!("the dog recorded {} for this run", totals.give_back);
                ignored = Some((kind, false, value, why));
            }
        }
        let ignored = ignored.and_then(|(kind, want, value, why)| {
            let first = run.said.insert((kind.clone(), want, value));
            let total = if want { "want" } else { "return" };
            first.then(|| {
                format!(
                    "ignored {kind} {total} total {value} from {sheep} run {}: {why}",
                    said.epoch.0
                )
            })
        });
        Heard {
            grants: deliveries(grants),
            ignored,
        }
    }

    /// Grants whatever an opened review window now allows
    pub fn tick(&mut self) -> Vec<Delivery> {
        let grants = deliveries(self.book.tick());
        self.look(true);
        grants
    }

    // The run `epoch` of `sheep`, started if it is new. `None` for a run
    // already replaced; otherwise the grants a new run's reclaim made.
    fn run_of(&mut self, sheep: &str, project: &ProjectName, epoch: Epoch) -> Option<Vec<Grant>> {
        if self.retired.contains(sheep, epoch) {
            return None;
        }
        let run = self.runs.entry(sheep.to_owned()).or_insert_with(|| Run {
            epoch,
            totals: BTreeMap::new(),
            said: BTreeSet::new(),
        });
        if run.epoch == epoch {
            return Some(Vec::new());
        }
        self.retired.insert(sheep, run.epoch);
        *run = Run {
            epoch,
            totals: BTreeMap::new(),
            said: BTreeSet::new(),
        };
        Some(self.book.reclaim(project, Some(epoch)))
    }

    /// Takes what shep says of sheep `sheep`: its live process, if any
    ///
    /// Reclaims every lease a run other than `pid` held or waited for.
    pub fn runner_is(&mut self, sheep: &str, pid: Option<u32>) -> Vec<Delivery> {
        let grants = self.reclaim(sheep, pid);
        self.look(true);
        grants
    }

    fn reclaim(&mut self, sheep: &str, pid: Option<u32>) -> Vec<Delivery> {
        let Ok(project) = ProjectName::try_from(sheep) else {
            return Vec::new();
        };
        let keep = pid.map(|pid| Epoch(u64::from(pid)));
        // A live pid is a live run, even one reusing a retired pid.
        if let Some(epoch) = keep {
            self.retired.remove(sheep, epoch);
        }
        if let Some(run) = self.runs.get(sheep).filter(|run| Some(run.epoch) != keep) {
            self.retired.insert(sheep, run.epoch);
            self.runs.remove(sheep);
        }
        deliveries(self.book.reclaim(&project, keep))
    }

    /// Checks every known runner against the live flock, on start and
    /// after shep dropped events the dog may have needed
    ///
    /// `live` names each sheep with the pid of its live process.
    pub fn resync(&mut self, live: &HashMap<String, u32>) -> Vec<Delivery> {
        let mut sheep: BTreeSet<String> = self.runs.keys().cloned().collect();
        sheep.extend(self.book.runners().iter().map(|p| p.as_str().to_owned()));
        sheep
            .iter()
            .flat_map(|name| self.runner_is(name, live.get(name).copied()))
            .collect()
    }
}

// A total is whole, not negative, and exact in an f64: at most 2^53.
fn count(value: f64) -> Option<u64> {
    const EXACT: f64 = 9_007_199_254_740_992.0;
    let whole = (0.0..=EXACT).contains(&value) && value.fract() == 0.0;
    whole.then_some(value as u64)
}

/// The grants that go out as triggers: the maintainer's are only answered
pub(super) fn deliveries(grants: impl IntoIterator<Item = Grant>) -> Vec<Delivery> {
    grants
        .into_iter()
        .filter_map(|grant| match grant.holder {
            Holder::Maintainer => None,
            Holder::Runner { project, epoch } => Some(Delivery {
                project,
                kind: grant.kind,
                epoch,
            }),
        })
        .collect()
}

#[cfg(test)]
pub(super) mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::lease::saved::BookFile;
    use crate::lease::wire::Asker;
    use crate::test::{EPOCH, FakeClock, RecordedSink};

    pub(crate) struct World {
        pub(crate) desk: Desk,
        pub(crate) clock: FakeClock,
        reviewers: Reviewers,
        pub(crate) samples: RecordedSink,
        _temp: tempfile::TempDir,
    }

    pub(crate) fn world() -> World {
        world_with(Reviewers::default())
    }

    // A dog whose agent files define `reviewers`.
    pub(crate) fn world_with(reviewers: Reviewers) -> World {
        let _temp = tempfile::tempdir().unwrap();
        let clock = FakeClock::at(EPOCH);
        let samples = RecordedSink::default();
        let desk = Desk::new(
            Box::new(clock.clone()),
            reviewers.clone(),
            samples.recorder(),
        );
        World {
            desk,
            clock,
            reviewers,
            samples,
            _temp,
        }
    }

    pub(crate) fn stand_in() -> LeaseKind {
        LeaseKind::try_from("stand-in").unwrap()
    }

    pub(crate) fn grant(project: &str, pid: u64) -> Delivery {
        Delivery {
            project: ProjectName::try_from(project).unwrap(),
            kind: stand_in(),
            epoch: Epoch(pid),
        }
    }

    impl World {
        // Sends a note the way a runner's channel would reach the dog.
        pub(crate) fn raise(&mut self, sheep: &str, (name, value): (String, f64)) -> Vec<Delivery> {
            self.desk.note(sheep, &name, value)
        }

        // What the dog says of a note it does not act on, if anything.
        pub(crate) fn ignored(
            &mut self,
            sheep: &str,
            (name, value): (String, f64),
        ) -> Option<String> {
            self.desk.heard(sheep, &name, value).ignored
        }

        pub(crate) fn ask(&mut self, action: &str, params: Option<&str>) -> (Value, Vec<Delivery>) {
            let (body, out) = self.desk.answer(action, params);
            (serde_json::from_str(&body).unwrap(), out)
        }

        // The dog stopped and started again: a new desk from the book file.
        pub(crate) fn restart(&mut self) {
            let file = BookFile::new(self._temp.path().join("book.json"));
            file.save(&self.desk.saved()).unwrap();
            let saved = file.load().unwrap().expect("a saved book");
            let clock = Box::new(self.clock.clone());
            let metrics = self.samples.recorder();
            self.desk = Desk::restore(clock, saved, self.reviewers.clone(), metrics);
        }

        pub(crate) fn book_line(&mut self) -> Value {
            self.line("stand-in")
        }

        pub(crate) fn line(&mut self, kind: &str) -> Value {
            let leases = self.ask("status", None).0["leases"].clone();
            let found = leases
                .as_array()
                .unwrap()
                .iter()
                .find(|l| l["kind"] == kind);
            found.cloned().unwrap_or_default()
        }
    }

    #[test]
    fn a_runner_that_asks_for_a_free_lease_is_granted_it() {
        let mut w = world();
        let mut koji = Asker::new(Epoch(101));
        assert_eq!(
            w.raise("koji", koji.want(&stand_in())),
            [grant("koji", 101)]
        );
        assert_eq!(
            w.book_line(),
            json!({
                "kind": "stand-in",
                "holder": { "runner": "koji" },
                "since": EPOCH,
                "queue": [],
            })
        );
    }

    #[test]
    fn a_second_runner_queues_and_is_granted_when_the_first_gives_back() {
        let mut w = world();
        let (mut koji, mut reactmap) = (Asker::new(Epoch(101)), Asker::new(Epoch(202)));
        w.raise("koji", koji.want(&stand_in()));
        assert_eq!(w.raise("reactmap", reactmap.want(&stand_in())), []);
        w.clock.advance(40);
        assert_eq!(
            w.raise("koji", koji.give_back(&stand_in())),
            [grant("reactmap", 202)]
        );
        assert_eq!(w.book_line()["since"], EPOCH + 40);
    }

    #[test]
    fn a_repeated_total_changes_nothing() {
        let mut w = world();
        let mut koji = Asker::new(Epoch(101));
        let want = koji.want(&stand_in());
        w.raise("koji", want.clone());
        for note in koji.notes() {
            assert_eq!(w.raise("koji", note), []);
        }
        assert_eq!(w.raise("koji", want), []);
        assert_eq!(w.book_line()["holder"], json!({ "runner": "koji" }));
    }

    #[test]
    fn a_total_below_the_one_recorded_is_said_to_be_ignored_once() {
        let mut w = world();
        let mut old = Asker::new(Epoch(101));
        for _ in 0..3 {
            let _ = old.want(&stand_in());
            let _ = old.give_back(&stand_in());
        }
        for note in old.notes() {
            w.raise("koji", note);
        }
        // The new image of the run counts from zero.
        let mut new = Asker::new(Epoch(101));
        let want = new.want(&stand_in());
        let said = w.ignored("koji", want.clone());
        assert_eq!(
            said.as_deref(),
            Some(
                "ignored stand-in want total 1 from koji run 101: \
                 the dog recorded 3 for this run"
            )
        );
        assert_eq!(w.ignored("koji", want), None, "the same ask is said once");
    }

    #[test]
    fn a_want_the_dog_already_has_is_not_said_to_be_ignored() {
        let mut w = world();
        let mut koji = Asker::new(Epoch(101));
        let want = koji.want(&stand_in());
        assert_eq!(w.ignored("koji", want.clone()), None);
        assert_eq!(
            w.ignored("koji", want),
            None,
            "an ask repeated while it stands"
        );
        for note in koji.notes() {
            assert_eq!(w.ignored("koji", note), None);
        }
    }

    #[test]
    fn a_want_sent_again_after_the_return_is_not_said_to_be_ignored() {
        let mut w = world();
        let mut koji = Asker::new(Epoch(101));
        let want = koji.want(&stand_in());
        w.raise("koji", want.clone());
        w.raise("koji", koji.give_back(&stand_in()));
        assert_eq!(w.ignored("koji", want), None);
    }

    #[test]
    fn a_dropped_return_is_read_from_the_next_want() {
        let mut w = world();
        let (mut koji, mut reactmap) = (Asker::new(Epoch(101)), Asker::new(Epoch(202)));
        w.raise("koji", koji.want(&stand_in()));
        w.raise("reactmap", reactmap.want(&stand_in()));
        let _dropped = koji.give_back(&stand_in());
        assert_eq!(
            w.raise("koji", koji.want(&stand_in())),
            [grant("reactmap", 202)]
        );
        assert_eq!(w.book_line()["queue"], json!([{ "runner": "koji" }]));
    }

    #[test]
    fn a_restarted_runner_loses_its_lease_to_the_next_waiter() {
        let mut w = world();
        let (mut koji, mut reactmap) = (Asker::new(Epoch(101)), Asker::new(Epoch(202)));
        w.raise("koji", koji.want(&stand_in()));
        w.raise("reactmap", reactmap.want(&stand_in()));
        let mut koji_again = Asker::new(Epoch(303));
        assert_eq!(
            w.raise("koji", koji_again.want(&stand_in())),
            [grant("reactmap", 202)],
            "the new run's want reclaims the old run's lease"
        );
        assert_eq!(w.book_line()["queue"], json!([{ "runner": "koji" }]));
    }

    #[test]
    fn shep_reporting_a_restart_reclaims_before_the_new_run_asks() {
        let mut w = world();
        let (mut koji, mut reactmap) = (Asker::new(Epoch(101)), Asker::new(Epoch(202)));
        w.raise("koji", koji.want(&stand_in()));
        w.raise("reactmap", reactmap.want(&stand_in()));
        assert_eq!(
            w.desk.runner_is("koji", Some(303)),
            [grant("reactmap", 202)]
        );
    }

    #[test]
    fn a_restart_reported_late_keeps_what_the_new_run_holds() {
        let mut w = world();
        let mut koji = Asker::new(Epoch(303));
        w.raise("koji", koji.want(&stand_in()));
        assert_eq!(w.desk.runner_is("koji", Some(303)), []);
        assert_eq!(w.book_line()["holder"], json!({ "runner": "koji" }));
    }

    #[test]
    fn a_late_note_from_a_replaced_run_changes_nothing() {
        let mut w = world();
        let (mut old, mut new) = (Asker::new(Epoch(101)), Asker::new(Epoch(303)));
        w.raise("koji", old.want(&stand_in()));
        w.raise("koji", new.want(&stand_in()));
        assert_eq!(w.raise("koji", old.give_back(&stand_in())), []);
        assert_eq!(w.raise("koji", old.want(&stand_in())), []);
        assert_eq!(w.book_line()["holder"], json!({ "runner": "koji" }));
        assert_eq!(w.desk.runner_is("koji", Some(303)), [], "303 still holds");
    }

    #[test]
    fn a_pid_reused_by_a_restart_counts_again() {
        let mut w = world();
        let (mut old, mut new) = (Asker::new(Epoch(101)), Asker::new(Epoch(303)));
        w.raise("koji", old.want(&stand_in()));
        w.raise("koji", new.want(&stand_in()));
        w.desk.runner_is("koji", Some(101));
        let mut reused = Asker::new(Epoch(101));
        assert_eq!(
            w.raise("koji", reused.want(&stand_in())),
            [grant("koji", 101)]
        );
    }

    #[test]
    fn a_runner_that_dies_loses_its_lease_to_the_next_waiter() {
        let mut w = world();
        let (mut koji, mut reactmap) = (Asker::new(Epoch(101)), Asker::new(Epoch(202)));
        w.raise("koji", koji.want(&stand_in()));
        w.raise("reactmap", reactmap.want(&stand_in()));
        assert_eq!(w.desk.runner_is("koji", None), [grant("reactmap", 202)]);
    }

    #[test]
    fn a_resync_reclaims_from_runners_that_are_gone_or_restarted() {
        let mut w = world();
        let other = LeaseKind::try_from("other").unwrap();
        let (mut koji, mut reactmap) = (Asker::new(Epoch(101)), Asker::new(Epoch(202)));
        let mut golbat = Asker::new(Epoch(404));
        w.raise("koji", koji.want(&stand_in()));
        w.raise("reactmap", reactmap.want(&other));
        w.raise("golbat", golbat.want(&stand_in()));
        w.raise("golbat", golbat.want(&other));
        let live = HashMap::from([("reactmap".into(), 999), ("golbat".into(), 404)]);
        let mut out = w.desk.resync(&live);
        out.sort_by(|a, b| a.kind.cmp(&b.kind));
        assert_eq!(
            out,
            [
                Delivery {
                    kind: other,
                    ..grant("golbat", 404)
                },
                grant("golbat", 404)
            ]
        );
    }

    #[test]
    fn notes_that_are_not_lease_totals_are_ignored() {
        let mut w = world();
        for (sheep, name, value) in [
            ("koji", "cpu.load", 1.0),
            ("koji", "lease.stand-in.want.101", 1.5),
            ("koji", "lease.stand-in.want.101", -1.0),
            ("koji", "lease.stand-in.want.101", f64::NAN),
            ("koji", "lease.stand-in.want.101", 1e19),
            ("a/b", "lease.stand-in.want.101", 1.0),
        ] {
            assert_eq!(
                w.desk.note(sheep, name, value),
                [],
                "{sheep} {name} {value}"
            );
        }
        let leases = &w.ask("status", None).0["leases"];
        assert_eq!(
            leases.as_array().unwrap().len(),
            2,
            "cargo-test and the CodeRabbit window alone: {leases}"
        );
    }

    fn coderabbit_grant(project: &str, pid: u64) -> Delivery {
        Delivery {
            kind: LeaseKind::coderabbit(),
            ..grant(project, pid)
        }
    }

    #[test]
    fn a_runner_that_saw_its_summon_accepted_holds_the_window_for_the_hour() {
        let mut w = world();
        let cr = LeaseKind::coderabbit();
        let (mut koji, mut reactmap) = (Asker::new(Epoch(101)), Asker::new(Epoch(202)));
        assert_eq!(
            w.raise("koji", koji.want(&cr)),
            [coderabbit_grant("koji", 101)]
        );
        w.raise("reactmap", reactmap.want(&cr));
        w.clock.advance(20);
        assert_eq!(
            w.raise("koji", koji.window(&cr, WindowFact::Summoned, EPOCH)),
            []
        );
        assert_eq!(w.raise("koji", koji.give_back(&cr)), []);
        assert_eq!(w.line("coderabbit")["window"]["opens"], EPOCH + 3600);

        w.clock.advance(3600 - 20);
        assert_eq!(w.desk.tick(), [coderabbit_grant("reactmap", 202)]);
    }

    #[test]
    fn a_refusal_a_runner_heard_reschedules_the_window() {
        let mut w = world();
        let cr = LeaseKind::coderabbit();
        let mut koji = Asker::new(Epoch(101));
        w.raise("koji", koji.want(&cr));
        let opens = EPOCH + 12 * 60;
        w.raise("koji", koji.window(&cr, WindowFact::Opens, opens));
        w.raise("koji", koji.give_back(&cr));
        assert_eq!(w.raise("koji", koji.want(&cr)), []);
        for note in koji.notes() {
            assert_eq!(w.raise("koji", note), [], "a repeat changes nothing");
        }
        w.clock.advance(12 * 60);
        assert_eq!(w.desk.tick(), [coderabbit_grant("koji", 101)]);
    }

    #[test]
    fn a_footer_quota_reaches_the_window() {
        let mut w = world();
        let cr = LeaseKind::coderabbit();
        let mut koji = Asker::new(Epoch(101));
        w.raise("koji", koji.window(&cr, WindowFact::Quota(10), EPOCH));
        assert_eq!(w.line("coderabbit")["window"]["quota"], 10);
    }

    // Each runner reads its own pull request's footers, and raises its
    // quota again every look, so an older footer keeps arriving late.
    #[test]
    fn two_runners_reporting_footers_out_of_order_leave_the_newest_quota() {
        let mut w = world();
        let cr = LeaseKind::coderabbit();
        let (mut koji, mut reactmap) = (Asker::new(Epoch(101)), Asker::new(Epoch(202)));
        w.raise(
            "reactmap",
            reactmap.window(&cr, WindowFact::Quota(1), EPOCH + 600),
        );
        w.raise("koji", koji.window(&cr, WindowFact::Quota(10), EPOCH));
        assert_eq!(w.line("coderabbit")["window"]["quota"], 1);
        for note in koji.notes() {
            w.raise("koji", note);
        }
        assert_eq!(w.line("coderabbit")["window"]["quota"], 1, "raised again");
    }

    #[test]
    fn a_window_fact_from_a_replaced_run_changes_nothing() {
        let mut w = world();
        let cr = LeaseKind::coderabbit();
        let (mut old, mut new) = (Asker::new(Epoch(101)), Asker::new(Epoch(303)));
        w.raise("koji", old.want(&cr));
        w.raise("koji", new.want(&cr));
        w.raise("koji", old.window(&cr, WindowFact::Opens, EPOCH + 600));
        assert_eq!(w.line("coderabbit")["window"]["opens"], json!(null));
    }

    fn live(runs: &[(&str, u32)]) -> HashMap<String, u32> {
        runs.iter()
            .map(|(s, pid)| ((*s).to_owned(), *pid))
            .collect()
    }

    #[test]
    fn a_restarted_dog_keeps_the_lease_its_holder_still_runs_with() {
        let mut w = world();
        let (mut koji, mut reactmap) = (Asker::new(Epoch(101)), Asker::new(Epoch(202)));
        w.raise("koji", koji.want(&stand_in()));
        w.raise("reactmap", reactmap.want(&stand_in()));
        w.restart();
        assert_eq!(
            w.desk.resync(&live(&[("koji", 101), ("reactmap", 202)])),
            []
        );
        for note in koji.notes() {
            assert_eq!(w.raise("koji", note), [], "a repeat changes nothing");
        }
        for note in reactmap.notes() {
            assert_eq!(w.raise("reactmap", note), [], "a repeat changes nothing");
        }
        assert_eq!(
            w.book_line(),
            json!({
                "kind": "stand-in",
                "holder": { "runner": "koji" },
                "since": EPOCH,
                "queue": [{ "runner": "reactmap" }],
            })
        );
        assert_eq!(
            w.raise("koji", koji.give_back(&stand_in())),
            [grant("reactmap", 202)]
        );
    }

    #[test]
    fn a_restarted_dog_reclaims_from_a_holder_whose_sheep_has_a_new_pid() {
        let mut w = world();
        let (mut koji, mut reactmap) = (Asker::new(Epoch(101)), Asker::new(Epoch(202)));
        w.raise("koji", koji.want(&stand_in()));
        w.raise("reactmap", reactmap.want(&stand_in()));
        w.restart();
        assert_eq!(
            w.desk.resync(&live(&[("koji", 303), ("reactmap", 202)])),
            [grant("reactmap", 202)]
        );
        assert_eq!(
            w.raise("koji", koji.give_back(&stand_in())),
            [],
            "a late note from the replaced run changes nothing"
        );
    }

    #[test]
    fn a_restarted_dog_reclaims_from_a_holder_whose_sheep_is_gone() {
        let mut w = world();
        let (mut koji, mut reactmap) = (Asker::new(Epoch(101)), Asker::new(Epoch(202)));
        w.raise("koji", koji.want(&stand_in()));
        w.raise("reactmap", reactmap.want(&stand_in()));
        w.restart();
        assert_eq!(
            w.desk.resync(&live(&[("reactmap", 202)])),
            [grant("reactmap", 202)]
        );
    }

    #[test]
    fn a_restarted_dog_keeps_the_maintainers_lease() {
        let mut w = world();
        w.ask("take", Some("stand-in"));
        w.restart();
        assert_eq!(w.desk.resync(&live(&[])), []);
        assert_eq!(w.book_line()["holder"], "maintainer");
    }

    // A restart that forgot the window would grant a second summon inside
    // the hour: the whole reason the book is saved.
    #[test]
    fn a_restarted_dog_grants_no_second_summon_in_the_window() {
        let mut w = world();
        let cr = LeaseKind::coderabbit();
        let (mut koji, mut reactmap) = (Asker::new(Epoch(101)), Asker::new(Epoch(202)));
        w.raise("koji", koji.want(&cr));
        w.raise("koji", koji.window(&cr, WindowFact::Summoned, EPOCH));
        w.raise("koji", koji.give_back(&cr));
        w.clock.advance(300);
        w.restart();
        assert_eq!(w.desk.resync(&live(&[("koji", 101)])), []);
        assert_eq!(w.raise("reactmap", reactmap.want(&cr)), []);
        assert_eq!(w.line("coderabbit")["window"]["opens"], EPOCH + 3600);

        w.clock.advance(3600 - 300);
        assert_eq!(w.desk.tick(), [coderabbit_grant("reactmap", 202)]);
    }

    #[test]
    fn a_restarted_dog_keeps_a_refusals_wait_and_the_footers_quota() {
        let mut w = world();
        let cr = LeaseKind::coderabbit();
        let mut koji = Asker::new(Epoch(101));
        w.raise("koji", koji.window(&cr, WindowFact::Quota(3), EPOCH));
        w.raise("koji", koji.window(&cr, WindowFact::Opens, EPOCH + 600));
        w.restart();
        assert_eq!(
            w.line("coderabbit")["window"],
            json!({ "quota": 3, "summons": [], "opens": EPOCH + 600 })
        );
        assert_eq!(w.raise("koji", koji.want(&cr)), []);
        w.clock.advance(600);
        assert_eq!(w.desk.tick(), [coderabbit_grant("koji", 101)]);
    }

    #[test]
    fn each_defined_reviewer_gets_its_own_window_and_keeps_it_across_a_restart() {
        let month = crate::review_bot::ReviewWindow {
            reviews: std::num::NonZeroU32::new(20).unwrap(),
            hours: std::num::NonZeroU32::new(720).unwrap(),
        };
        let mut w = world_with(Reviewers::default().with(crate::review_bot::Bot::Cubic, month));
        let (cr, cubic) = (
            LeaseKind::coderabbit(),
            crate::review_bot::Bot::Cubic.lease(),
        );
        assert_eq!(w.line("cubic")["window"]["quota"], 20);
        let mut koji = Asker::new(Epoch(101));
        w.raise("koji", koji.window(&cr, WindowFact::Summoned, EPOCH));
        assert_eq!(
            w.raise("koji", koji.want(&cr)),
            [],
            "CodeRabbit's hour is spent"
        );
        let granted = w.raise("koji", koji.want(&cubic));
        assert_eq!(
            granted,
            [Delivery {
                kind: cubic.clone(),
                ..grant("koji", 101)
            }]
        );
        w.raise(
            "koji",
            koji.window(&cubic, WindowFact::Opens, EPOCH + 7 * 86_400),
        );
        w.restart();
        assert_eq!(w.line("cubic")["window"]["opens"], EPOCH + 7 * 86_400);
        assert_eq!(w.line("coderabbit")["window"]["opens"], EPOCH + 3600);
    }

    #[test]
    fn a_runner_restarted_many_times_is_remembered_for_its_newest_runs() {
        let mut w = world();
        for pid in 1..=20 {
            w.desk.runner_is("koji", Some(pid));
            w.raise("koji", Asker::new(Epoch(u64::from(pid))).want(&stand_in()));
        }
        w.desk.runner_is("koji", Some(21));
        w.restart();
        let retired: Vec<u64> = w.desk.saved().retired.iter().map(|r| r.epoch.0).collect();
        assert_eq!(retired, (13..=20).collect::<Vec<_>>());
        let mut newest_replaced = Asker::new(Epoch(20));
        assert_eq!(
            w.raise("koji", newest_replaced.want(&stand_in())),
            [],
            "a late note from a recent run still changes nothing"
        );
    }
}
