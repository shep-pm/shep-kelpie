//! How a runner and the dog talk about book leases, through shep
//!
//! A runner raises two running totals per kind as channel metrics,
//! `lease.<kind>.want.<epoch>` and `lease.<kind>.return.<epoch>`. Totals
//! survive a dropped metric, since the next one carries the count. The
//! epoch in the name ties each total to one run of the runner. The dog
//! grants with a `grant` trigger whose params are `<kind> <epoch>`.

use std::collections::BTreeMap;
use std::fmt;

use super::{Epoch, LeaseKind};

/// The trigger the dog grants a lease with
pub const GRANT: &str = "grant";

/// Which of a runner's two totals a metric carries
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Total {
    /// How many times this run has asked for the kind
    Want,
    /// How many times this run has given it back
    Return,
}

impl Total {
    fn as_str(self) -> &'static str {
        match self {
            Self::Want => "want",
            Self::Return => "return",
        }
    }
}

/// A lease metric's name, read back
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricName {
    /// The kind it is about
    pub kind: LeaseKind,
    /// Which total it carries
    pub total: Total,
    /// The run that raised it
    pub epoch: Epoch,
}

impl MetricName {
    /// Reads `lease.<kind>.<want|return>.<epoch>`, or `None` for any other
    /// metric
    pub fn parse(name: &str) -> Option<Self> {
        let rest = name.strip_prefix("lease.")?;
        let (rest, epoch) = rest.rsplit_once('.')?;
        let (kind, total) = rest.rsplit_once('.')?;
        let total = match total {
            "want" => Total::Want,
            "return" => Total::Return,
            _ => return None,
        };
        Some(Self {
            kind: LeaseKind::try_from(kind).ok()?,
            total,
            epoch: Epoch(epoch.parse().ok()?),
        })
    }
}

impl fmt::Display for MetricName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { kind, total, epoch } = self;
        write!(f, "lease.{kind}.{}.{}", total.as_str(), epoch.0)
    }
}

/// The params of a `grant` trigger: `<kind> <epoch>`
pub fn grant_params(kind: &LeaseKind, epoch: Epoch) -> String {
    format!("{kind} {}", epoch.0)
}

/// One run's two running totals for one kind
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    /// How many times it has asked
    pub want: u64,
    /// How many times it has given back or withdrawn
    pub give_back: u64,
}

impl Totals {
    /// Whether the run is asking: more wants than give-backs
    pub fn asking(self) -> bool {
        self.want > self.give_back
    }
}

/// A runner's side of its book leases: its totals and what it holds
///
/// A runner waiting on a grant may raise [`Asker::totals`] again at any
/// time: the dog reads totals, so repeats change nothing, and they cover
/// a metric shep dropped.
#[derive(Debug, Clone)]
pub struct Asker {
    epoch: Epoch,
    totals: BTreeMap<LeaseKind, Totals>,
    held: Vec<LeaseKind>,
}

impl Asker {
    /// A fresh run's side, with nothing asked for
    pub fn new(epoch: Epoch) -> Self {
        Self {
            epoch,
            totals: BTreeMap::new(),
            held: Vec::new(),
        }
    }

    /// Asks for `kind`: the metric to raise, and its value
    pub fn want(&mut self, kind: &LeaseKind) -> (String, f64) {
        let entry = self.totals.entry(kind.clone()).or_default();
        entry.want += 1;
        self.metric(kind, Total::Want)
    }

    /// Gives `kind` back, or withdraws the ask: the metric to raise
    pub fn give_back(&mut self, kind: &LeaseKind) -> (String, f64) {
        self.held.retain(|k| k != kind);
        let entry = self.totals.entry(kind.clone()).or_default();
        entry.give_back += 1;
        self.metric(kind, Total::Return)
    }

    /// Takes a `grant` trigger's params, and says which kind is now held
    ///
    /// # Errors
    ///
    /// [`GrantError`] when the params do not parse, name another run, or
    /// grant a kind this run has not asked for.
    pub fn grant(&mut self, params: &str) -> Result<LeaseKind, GrantError> {
        let malformed = || GrantError::Malformed(params.to_owned());
        let (kind, epoch) = params.trim().split_once(' ').ok_or_else(malformed)?;
        let kind = LeaseKind::try_from(kind).map_err(|_| malformed())?;
        let epoch = Epoch(epoch.parse().map_err(|_| malformed())?);
        if epoch != self.epoch {
            return Err(GrantError::OtherRun(epoch));
        }
        let asking = self.totals.get(&kind).is_some_and(|t| t.asking());
        if !asking {
            return Err(GrantError::NotAsked(kind));
        }
        if !self.held.contains(&kind) {
            self.held.push(kind.clone());
        }
        Ok(kind)
    }

    /// Whether this run holds `kind`
    pub fn holds(&self, kind: &LeaseKind) -> bool {
        self.held.contains(kind)
    }

    /// Every metric this run has raised, at its current value
    pub fn totals(&self) -> Vec<(String, f64)> {
        let totals = [Total::Want, Total::Return];
        self.totals
            .keys()
            .flat_map(|kind| totals.map(|total| self.metric(kind, total)))
            .collect()
    }

    fn metric(&self, kind: &LeaseKind, total: Total) -> (String, f64) {
        let totals = self.totals.get(kind).copied().unwrap_or_default();
        let value = match total {
            Total::Want => totals.want,
            Total::Return => totals.give_back,
        };
        let name = MetricName {
            kind: kind.clone(),
            total,
            epoch: self.epoch,
        };
        // Exact: a run asks far fewer than 2^53 times.
        #[allow(clippy::cast_precision_loss)]
        (name.to_string(), value as f64)
    }
}

/// Why a `grant` was not taken
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantError {
    /// The params are not `<kind> <epoch>`, carrying them
    Malformed(String),
    /// It was meant for another run of this runner
    OtherRun(Epoch),
    /// This run is not asking for that kind
    NotAsked(LeaseKind),
}

impl fmt::Display for GrantError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(params) => write!(f, "a grant takes <kind> <epoch>, not {params:?}"),
            Self::OtherRun(epoch) => write!(f, "that grant was for run {}", epoch.0),
            Self::NotAsked(kind) => write!(f, "this run is not asking for {kind}"),
        }
    }
}

impl std::error::Error for GrantError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn stand_in() -> LeaseKind {
        LeaseKind::try_from("stand-in").unwrap()
    }

    #[test]
    fn totals_count_up_under_a_name_that_carries_the_run() {
        let mut asker = Asker::new(Epoch(4242));
        assert_eq!(
            asker.want(&stand_in()),
            ("lease.stand-in.want.4242".into(), 1.0)
        );
        assert_eq!(
            asker.give_back(&stand_in()),
            ("lease.stand-in.return.4242".into(), 1.0)
        );
        assert_eq!(
            asker.want(&stand_in()),
            ("lease.stand-in.want.4242".into(), 2.0)
        );
        assert_eq!(
            asker.totals(),
            [
                ("lease.stand-in.want.4242".into(), 2.0),
                ("lease.stand-in.return.4242".into(), 1.0)
            ]
        );
    }

    #[test]
    fn a_metric_name_reads_back() {
        assert_eq!(
            MetricName::parse("lease.stand-in.return.7"),
            Some(MetricName {
                kind: stand_in(),
                total: Total::Return,
                epoch: Epoch(7)
            })
        );
        for other in [
            "cpu.load",
            "lease.stand-in.want",
            "lease.stand-in.keep.7",
            "lease.gpu.want.7",
            "lease.Stand.want.7",
            "lease.stand-in.want.-7",
        ] {
            assert_eq!(MetricName::parse(other), None, "{other}");
        }
    }

    #[test]
    fn a_grant_for_this_run_is_held_until_given_back() {
        let mut asker = Asker::new(Epoch(9));
        asker.want(&stand_in());
        assert_eq!(
            asker.grant(&grant_params(&stand_in(), Epoch(9))),
            Ok(stand_in())
        );
        assert!(asker.holds(&stand_in()));
        asker.give_back(&stand_in());
        assert!(!asker.holds(&stand_in()));
    }

    #[test]
    fn a_grant_for_another_run_or_an_unasked_kind_is_refused() {
        let mut asker = Asker::new(Epoch(9));
        assert_eq!(
            asker.grant("stand-in 9"),
            Err(GrantError::NotAsked(stand_in()))
        );
        asker.want(&stand_in());
        assert_eq!(
            asker.grant("stand-in 8"),
            Err(GrantError::OtherRun(Epoch(8)))
        );
        for bad in ["", "stand-in", "stand-in nine", "gpu 9"] {
            assert_eq!(
                asker.grant(bad),
                Err(GrantError::Malformed(bad.into())),
                "{bad:?}"
            );
        }
        assert!(!asker.holds(&stand_in()));
    }

    #[test]
    fn a_repeated_grant_is_held_once() {
        let mut asker = Asker::new(Epoch(9));
        asker.want(&stand_in());
        asker.grant("stand-in 9").unwrap();
        asker.grant("stand-in 9").unwrap();
        asker.give_back(&stand_in());
        assert!(!asker.holds(&stand_in()));
    }

    #[test]
    fn a_grant_after_the_ask_was_withdrawn_is_refused() {
        let mut asker = Asker::new(Epoch(9));
        asker.want(&stand_in());
        asker.give_back(&stand_in());
        assert_eq!(
            asker.grant("stand-in 9"),
            Err(GrantError::NotAsked(stand_in()))
        );
        assert!(!asker.holds(&stand_in()));
    }
}
