//! An account's plan limits, as the pacer last read them

use super::{Family, Kept, Label, Recorder};
use crate::pacer::Reading;
use crate::ports::SeriesKind::Gauge;
use crate::ports::Timestamp;
use crate::settings::Account;

const USED: Family = Family {
    name: "kelpie_plan_used_ratio",
    kind: Gauge,
    help: "How much of the plan's 5-hour session or weekly window is used, 0 to 1.",
};
const RESETS_AT: Family = Family {
    name: "kelpie_plan_resets_at_seconds",
    kind: Gauge,
    help: "When the plan's session or weekly window resets, in Unix seconds.",
};

impl Recorder {
    /// Sets `account`'s plan gauges from the pacer's `reading` of its usage
    pub fn plan(&self, account: Account, reading: &Reading) {
        let account = ("account", account.as_str().to_owned());
        let windows = [("session", &reading.session), ("week", &reading.week)];
        for (name, window) in windows {
            let labels: Vec<Label> = vec![account.clone(), ("window", name.to_owned())];
            self.set(&USED, labels.clone(), f64::from(window.used_pct) / 100.0);
            #[expect(
                clippy::cast_precision_loss,
                reason = "Unix seconds stay far below 2^53"
            )]
            self.set(&RESETS_AT, labels, window.resets_at.0 as f64);
        }
    }
}

impl Kept {
    /// Zeroes the share used of each window that reset at or before `now`
    ///
    /// A runner that makes no call reads no usage, so its last reading would
    /// otherwise outlive the window it measured.
    pub(super) fn lapse_plans(&mut self, now: Timestamp) {
        #[expect(
            clippy::cast_precision_loss,
            reason = "Unix seconds stay far below 2^53"
        )]
        let now = now.0 as f64;
        let lapsed: Vec<Vec<Label>> = (self.series.iter())
            .filter(|((name, _), sample)| *name == RESETS_AT.name && sample.value <= now)
            .map(|((_, labels), _)| labels.clone())
            .collect();
        for labels in lapsed {
            if let Some(used) = self.series.get_mut(&(USED.name, labels)) {
                used.value = 0.0;
            }
        }
    }
}
