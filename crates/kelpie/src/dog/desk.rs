//! The dog's desk: what it makes of what shep tells it
//!
//! Pure. Runner metrics and runner processes go in, and the grants the
//! dog must deliver come out. The maintainer's triggers are answered in
//! [`super::triggers`]. The shell in [`super`] feeds it from shep and
//! delivers its grants as triggers.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::lease::book::{Asked, Grant, LeaseBook};
use crate::lease::gpu::GpuLock;
use crate::lease::wire::{MetricName, Total, Totals, WindowFact, WindowMetric};
use crate::lease::{Epoch, Holder, LeaseKind};
use crate::ports::{Clock, Timestamp};
use crate::runner::ProjectName;

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
}

/// The lease book and everything the dog knows about each runner's run
#[derive(Debug)]
pub struct Desk {
    pub(super) book: LeaseBook,
    pub(super) gpu: GpuLock,
    runs: HashMap<String, Run>,
    // Runs already replaced. Pids are not ordered, so a late metric from
    // one is known only by having seen it retired.
    retired: HashSet<(String, Epoch)>,
}

impl Desk {
    /// A desk with an empty book, reading the GPU lock at `gpu`
    pub fn new(clock: Box<dyn Clock>, gpu: GpuLock) -> Self {
        let mut book = LeaseBook::new(clock);
        book.open_window(LeaseKind::coderabbit());
        Self {
            book,
            gpu,
            runs: HashMap::new(),
            retired: HashSet::new(),
        }
    }

    /// Takes one channel metric from sheep `sheep`
    ///
    /// Anything that is not a lease metric from a runner is ignored. A new
    /// epoch reclaims what the runner's earlier run held.
    pub fn metric(&mut self, sheep: &str, name: &str, value: f64) -> Vec<Delivery> {
        let (Ok(project), Some(value)) = (ProjectName::try_from(sheep), count(value)) else {
            return Vec::new();
        };
        if let Some(fact) = WindowMetric::parse(name) {
            let Some(mut grants) = self.run_of(sheep, &project, fact.epoch) else {
                return Vec::new();
            };
            let at = Timestamp(value);
            let window = match fact.fact {
                WindowFact::Summoned => self.book.summoned(&fact.kind, at),
                WindowFact::Opens => self.book.refused(&fact.kind, at),
                WindowFact::Quota => {
                    let quota = u32::try_from(value).unwrap_or(u32::MAX);
                    self.book.quota(&fact.kind, quota)
                }
            };
            grants.extend(window);
            return deliveries(grants);
        }
        let Some(metric) = MetricName::parse(name) else {
            return Vec::new();
        };
        let Some(mut grants) = self.run_of(sheep, &project, metric.epoch) else {
            return Vec::new();
        };
        let run = self.runs.get_mut(sheep).expect("run_of keeps the run");
        let holder = Holder::Runner {
            project,
            epoch: metric.epoch,
        };
        let kind = metric.kind;
        let totals = run.totals.entry(kind.clone()).or_default();
        match metric.total {
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
            Total::Want | Total::Return => {}
        }
        deliveries(grants)
    }

    /// Grants whatever an opened review window now allows
    pub fn tick(&mut self) -> Vec<Delivery> {
        deliveries(self.book.tick())
    }

    // The run `epoch` of `sheep`, started if it is new. `None` for a run
    // already replaced; otherwise the grants a new run's reclaim made.
    fn run_of(&mut self, sheep: &str, project: &ProjectName, epoch: Epoch) -> Option<Vec<Grant>> {
        if self.retired.contains(&(sheep.to_owned(), epoch)) {
            return None;
        }
        let run = self.runs.entry(sheep.to_owned()).or_insert_with(|| Run {
            epoch,
            totals: BTreeMap::new(),
        });
        if run.epoch == epoch {
            return Some(Vec::new());
        }
        self.retired.insert((sheep.to_owned(), run.epoch));
        *run = Run {
            epoch,
            totals: BTreeMap::new(),
        };
        Some(self.book.reclaim(project, Some(epoch)))
    }

    /// Takes what shep says of sheep `sheep`: its live process, if any
    ///
    /// Reclaims every lease a run other than `pid` held or waited for.
    pub fn runner_is(&mut self, sheep: &str, pid: Option<u32>) -> Vec<Delivery> {
        let Ok(project) = ProjectName::try_from(sheep) else {
            return Vec::new();
        };
        let keep = pid.map(|pid| Epoch(u64::from(pid)));
        // A live pid is a live run, even one reusing a retired pid.
        if let Some(epoch) = keep {
            self.retired.remove(&(sheep.to_owned(), epoch));
        }
        if let Some(run) = self.runs.get(sheep).filter(|run| Some(run.epoch) != keep) {
            self.retired.insert((sheep.to_owned(), run.epoch));
            self.runs.remove(sheep);
        }
        deliveries(self.book.reclaim(&project, keep))
    }

    /// Checks every known runner against the live flock, after shep
    /// dropped events the dog may have needed
    ///
    /// `live` names each sheep with the pid of its live process.
    pub fn resync(&mut self, live: &HashMap<String, u32>) -> Vec<Delivery> {
        let sheep: Vec<String> = self.runs.keys().cloned().collect();
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
    use crate::lease::wire::Asker;
    use crate::test::FakeClock;

    const EPOCH: u64 = 1_790_000_000;

    pub(crate) struct World {
        pub(crate) desk: Desk,
        pub(crate) clock: FakeClock,
        _temp: tempfile::TempDir,
    }

    pub(crate) fn world() -> World {
        let _temp = tempfile::tempdir().unwrap();
        let clock = FakeClock::at(EPOCH);
        let desk = Desk::new(Box::new(clock.clone()), GpuLock::under(_temp.path()));
        World { desk, clock, _temp }
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
        // Raises a metric the way a runner's channel would reach the dog.
        pub(crate) fn raise(&mut self, sheep: &str, (name, value): (String, f64)) -> Vec<Delivery> {
            self.desk.metric(sheep, &name, value)
        }

        pub(crate) fn ask(&mut self, action: &str, params: Option<&str>) -> (Value, Vec<Delivery>) {
            let (body, out) = self.desk.answer(action, params);
            (serde_json::from_str(&body).unwrap(), out)
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
        for metric in koji.metrics() {
            assert_eq!(w.raise("koji", metric), []);
        }
        assert_eq!(w.raise("koji", want), []);
        assert_eq!(w.book_line()["holder"], json!({ "runner": "koji" }));
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
    fn a_late_metric_from_a_replaced_run_changes_nothing() {
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
    fn metrics_that_are_not_lease_totals_are_ignored() {
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
                w.desk.metric(sheep, name, value),
                [],
                "{sheep} {name} {value}"
            );
        }
        let leases = &w.ask("status", None).0["leases"];
        assert_eq!(
            leases.as_array().unwrap().len(),
            2,
            "the GPU and the CodeRabbit window alone: {leases}"
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
        for metric in koji.metrics() {
            assert_eq!(w.raise("koji", metric), [], "a repeat changes nothing");
        }
        w.clock.advance(12 * 60);
        assert_eq!(w.desk.tick(), [coderabbit_grant("koji", 101)]);
    }

    #[test]
    fn a_footer_quota_reaches_the_window() {
        let mut w = world();
        let cr = LeaseKind::coderabbit();
        let mut koji = Asker::new(Epoch(101));
        w.raise("koji", koji.window(&cr, WindowFact::Quota, 10));
        assert_eq!(w.line("coderabbit")["window"]["quota"], 10);
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
}
