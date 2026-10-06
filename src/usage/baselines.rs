//! Baselines to hold kelpie's numbers against, kept on this machine only
//!
//! `<kelpie home>/baselines/baselines.json`, written by hand, names each
//! baseline's units per merged pull request, such as the control room's
//! over a window it ran. `usage` prints each beside kelpie's median.

use std::path::Path;

use serde::Deserialize;

/// Where the baselines file is under kelpie's home
pub const FILE: &str = "baselines/baselines.json";

/// One baseline, as far as `usage` reads it
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Baseline {
    /// What it measured
    pub name: String,
    /// The stretch it measured over, as written
    #[serde(default)]
    pub window: Option<String>,
    /// Its units for each pull request it merged
    pub units_per_merged_pr: f64,
}

#[derive(Deserialize)]
struct File {
    baselines: Vec<Baseline>,
}

/// The baselines in `kelpie_home`'s file, or none when it is missing or
/// does not read as one
pub fn load(kelpie_home: &Path) -> Vec<Baseline> {
    let text = std::fs::read_to_string(kelpie_home.join(FILE)).unwrap_or_default();
    serde_json::from_str::<File>(&text).map_or_else(|_| Vec::new(), |file| file.baselines)
}
