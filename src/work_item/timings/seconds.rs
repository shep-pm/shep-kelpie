//! Seconds per phase

use std::collections::BTreeMap;

use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::TimingPhase;

/// Seconds per phase, always written with every phase, zeros included
// wire format: changing this is a breaking change to the state file and to `status`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seconds(BTreeMap<TimingPhase, u64>);

impl Default for Seconds {
    fn default() -> Self {
        Self(TimingPhase::ALL.iter().map(|p| (*p, 0)).collect())
    }
}

// A phase a saved file leaves out reads as zero.
impl<'de> Deserialize<'de> for Seconds {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut seconds = Self::default();
        for (phase, s) in BTreeMap::<TimingPhase, u64>::deserialize(deserializer)? {
            seconds.0.insert(phase, s);
        }
        Ok(seconds)
    }
}

impl Seconds {
    /// The seconds in `phase`
    pub fn get(&self, phase: TimingPhase) -> u64 {
        self.0.get(&phase).copied().unwrap_or(0)
    }

    /// Adds `seconds` to `phase`
    pub fn add(&mut self, phase: TimingPhase, seconds: u64) {
        let slot = self.0.entry(phase).or_insert(0);
        *slot = slot.saturating_add(seconds);
    }

    /// Adds every phase of `other`
    pub fn add_all(&mut self, other: &Self) {
        for phase in TimingPhase::ALL {
            self.add(phase, other.get(phase));
        }
    }

    /// The seconds in every phase together
    pub fn total(&self) -> u64 {
        self.0.values().fold(0, |sum, s| sum.saturating_add(*s))
    }

    #[cfg(test)]
    pub(crate) fn of(pairs: &[(TimingPhase, u64)]) -> Self {
        let mut seconds = Self::default();
        for (phase, s) in pairs {
            seconds.add(*phase, *s);
        }
        seconds
    }
}

impl Serialize for Seconds {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(TimingPhase::ALL.len()))?;
        for phase in TimingPhase::ALL {
            map.serialize_entry(&phase, &self.get(phase))?;
        }
        map.end()
    }
}

/// Writes `seconds` as the state file keeps them: every phase, but the ones
/// added after the first build only once they hold time
///
/// An older build reads each key as one of its own phases and refuses the
/// state file over one it does not know, so a work item that never ran a deep
/// round saves what that build reads. `status` and `timings` still show every phase.
///
/// # Errors
///
/// The serializer's own.
pub fn saved<S: Serializer>(seconds: &Seconds, serializer: S) -> Result<S::Ok, S::Error> {
    let kept = |phase: &&TimingPhase| **phase != TimingPhase::DeepRound || seconds.get(**phase) > 0;
    let phases: Vec<&TimingPhase> = TimingPhase::ALL.iter().filter(kept).collect();
    let mut map = serializer.serialize_map(Some(phases.len()))?;
    for phase in phases {
        map.serialize_entry(phase, &seconds.get(*phase))?;
    }
    map.end()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn seconds_serialize_every_phase_zeros_included() {
        let seconds = Seconds::of(&[(TimingPhase::Ci, 7)]);
        let value = serde_json::to_value(&seconds).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 13);
        assert_eq!((&value["ci"], &value["worker"]), (&json!(7), &json!(0)));
        let back: Seconds = serde_json::from_value(value).unwrap();
        assert_eq!(back, seconds);
        let sparse: Seconds = serde_json::from_value(json!({ "ci": 7 })).unwrap();
        assert_eq!(sparse, seconds, "a phase left out reads as zero");
    }

    #[derive(Serialize)]
    struct Saved<'a>(#[serde(serialize_with = "saved")] &'a Seconds);

    #[test]
    fn the_state_file_leaves_out_a_deep_round_that_took_no_time() {
        let none = serde_json::to_value(Saved(&Seconds::of(&[(TimingPhase::Ci, 7)]))).unwrap();
        let keys: Vec<&str> = none
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys.len(), 12, "{keys:?}");
        assert!(
            !keys.contains(&"deep_round"),
            "a build before it cannot read the key"
        );
        assert_eq!(none["worker"], 0, "every older phase stays, zeros included");

        let some = Seconds::of(&[(TimingPhase::DeepRound, 30)]);
        let value = serde_json::to_value(Saved(&some)).unwrap();
        assert_eq!(value["deep_round"], 30);
        let back: Seconds = serde_json::from_value(value).unwrap();
        assert_eq!(back, some);
        assert_eq!(
            serde_json::from_value::<Seconds>(none).unwrap(),
            Seconds::of(&[(TimingPhase::Ci, 7)])
        );
    }
}
