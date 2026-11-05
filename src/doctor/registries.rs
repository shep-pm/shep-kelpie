//! The registries of a project's package managers
//!
//! A worker installs packages in its sandbox, which always reaches the
//! registry of each manager the checkout keeps a lockfile or manifest for.

use super::Line;
use crate::package_managers;
use crate::settings::Settings;

/// A line naming each package manager the checkout keeps files for: nothing where it keeps none
pub(super) fn line(subject: String, settings: &Settings) -> Option<Line> {
    let found = package_managers::found_in(&settings.git.checkout);
    if found.is_empty() {
        return None;
    }
    let names: Vec<&str> = found.iter().map(|manager| manager.name).collect();
    Some(Line::ok(subject, names.join(", ")))
}
