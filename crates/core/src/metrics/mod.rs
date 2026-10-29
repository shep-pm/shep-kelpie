//! Kelpie's numbers, sent to shep's metrics dog as each changes (ADR 0012)
//!
//! A counter is kept whole here, and each change sends its new total, so a
//! dropped sample costs nothing: the next one carries it. Every series is
//! sent again once a minute, so a metrics dog that starts after the runner,
//! or restarts, holds them all within the minute.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

use crate::ports::{Sample, SeriesKind, Sink, Timestamp};

mod dog;
mod errors;
mod forge;
mod lock;
mod mcp;
mod model_lease;
mod plan;
mod requests;
mod series;
mod shepherd_sink;

pub use mcp::OWN_SERVERS;
pub use series::{CiTests, FEATURE_RUNS, Feature};
pub use shepherd_sink::ShepherdSink;

/// The most series a runner sends, one under the metrics dog's 1024 a sender
/// (shep-pm/shep#1222), which leaves room for the series counting the ones past it. The worst
/// case the budget test drives is 755 series: 126 of the roles' calls, tokens, cost and requests,
/// the 39 of the failed tool calls of #968, the 18 of the forge's calls and rate limits, the 21 of
/// the features' runs and the MCP servers, the lock's 3, the 20 of five models' leases, and the 502
/// of the finished work items of five agents across the ten kinds each ending each way. A shep
/// release without #1222 holds 256 a sender
pub const SERIES_CAP: usize = 1023;

// Seconds between sending every series again. A minute is a scrape or four
// at Prometheus's usual interval, and a few hundred samples a minute is
// nothing on the channel.
const RESEND_SECS: u64 = 60;

/// One series' name, kind and help, its labels apart
#[derive(Debug)]
pub struct Family {
    /// The series' name, `kelpie_` first and `_total` last on a counter
    pub name: &'static str,
    /// Whether it can go down
    pub kind: SeriesKind,
    /// What it counts
    pub help: &'static str,
}

const DROPPED: Family = Family {
    name: "kelpie_series_dropped_total",
    kind: SeriesKind::Counter,
    help: "Samples of a new series not sent because the runner already sends SERIES_CAP series.",
};

/// A label's name and value
pub type Label = (&'static str, String);

type Key = (&'static str, Vec<Label>);

/// The runner's series as they stand, and where they go
#[derive(Debug)]
pub struct Recorder {
    sink: Arc<dyn Sink>,
    kept: Mutex<Kept>,
    // The most series it sends
    cap: usize,
}

#[derive(Debug, Default)]
struct Kept {
    series: BTreeMap<Key, Sample>,
    // The names whose new series the cap turned away, told once each
    dropped: BTreeSet<&'static str>,
    sent_at: Option<Timestamp>,
}

impl Recorder {
    /// A recorder whose samples go to `sink`, holding no series yet
    pub fn new(sink: Arc<dyn Sink>) -> Self {
        Self {
            sink,
            kept: Mutex::new(Kept::default()),
            cap: SERIES_CAP,
        }
    }

    /// A recorder with no cap on its series, for a ledger replayed whole, which no metrics dog
    /// limits
    pub fn uncapped(sink: Arc<dyn Sink>) -> Self {
        Self {
            cap: usize::MAX,
            ..Self::new(sink)
        }
    }

    /// Adds `by` to the counter `family` with `labels`
    pub fn add(&self, family: &'static Family, labels: Vec<Label>, by: f64) {
        self.change(family, labels, |total| total + by);
    }

    /// Sets the gauge `family` with `labels` to `value`
    pub fn set(&self, family: &'static Family, labels: Vec<Label>, value: f64) {
        self.change(family, labels, |_| value);
    }

    /// Sends every series again, when a minute has passed since it last did
    pub fn refresh(&self, now: Timestamp) {
        let mut kept = self.kept();
        if kept
            .sent_at
            .is_some_and(|at| now.0.saturating_sub(at.0) < RESEND_SECS)
        {
            return;
        }
        kept.sent_at = Some(now);
        for sample in kept.series.values() {
            self.sink.send(sample);
        }
    }

    fn change(&self, family: &'static Family, mut labels: Vec<Label>, to: impl Fn(f64) -> f64) {
        labels.sort();
        let mut kept = self.kept();
        let key = (family.name, labels);
        let new = !kept.series.contains_key(&key);
        if new && kept.series.len() >= self.cap {
            return self.drop_new(&mut kept, family);
        }
        let sample = kept.series.entry(key.clone()).or_insert(Sample {
            name: family.name,
            kind: family.kind,
            help: family.help,
            labels: key.1,
            value: 0.0,
        });
        let value = to(sample.value);
        // A gauge set to what it holds is not news, and each save sets them all.
        if !new && family.kind == SeriesKind::Gauge && value.to_bits() == sample.value.to_bits() {
            return;
        }
        sample.value = value;
        self.sink.send(sample);
    }

    // The cap's own counter is the one series let past it.
    fn drop_new(&self, kept: &mut Kept, family: &'static Family) {
        if kept.dropped.insert(family.name) {
            eprintln!(
                "metrics: {} has a new series past the {SERIES_CAP} a runner sends, so it is not \
                 sent; kelpie_series_dropped_total counts each one",
                family.name
            );
        }
        let sample = (kept.series)
            .entry((DROPPED.name, Vec::new()))
            .or_insert(Sample {
                name: DROPPED.name,
                kind: DROPPED.kind,
                help: DROPPED.help,
                labels: Vec::new(),
                value: 0.0,
            });
        sample.value += 1.0;
        self.sink.send(sample);
    }

    fn kept(&self) -> std::sync::MutexGuard<'_, Kept> {
        self.kept.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
