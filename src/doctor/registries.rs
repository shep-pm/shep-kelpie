//! The registries and caches of a project's package managers
//!
//! A worker installs packages in its sandbox, which always reaches the registry of each manager
//! the checkout keeps a lockfile or manifest for, and sets the caches of each in its build
//! folder.

use super::Line;
use crate::package_managers;
use crate::settings::Settings;

/// A line naming each package manager the checkout keeps files for, with the variables that put
/// its caches in the build folder: nothing where it keeps none
pub(super) fn line(subject: String, settings: &Settings) -> Option<Line> {
    let found = package_managers::found_in(&settings.git.checkout);
    if found.is_empty() {
        return None;
    }
    let names: Vec<String> = found
        .iter()
        .map(|manager| {
            let caches: Vec<&str> = manager.caches.iter().map(|(name, _)| *name).collect();
            if caches.is_empty() {
                manager.name.to_owned()
            } else {
                format!("{} ({})", manager.name, caches.join(", "))
            }
        })
        .collect();
    Some(Line::ok(subject, names.join(", ")))
}

/// A line for each `build_env` entry a found manager's cache already sets to the same folder,
/// which the project can delete
///
/// An entry with the same name and another folder overrides the cache, so it stays unnamed.
pub(super) fn covered(subject: &dyn Fn(&str) -> String, settings: &Settings) -> Vec<Line> {
    let found = package_managers::found_in(&settings.git.checkout);
    let mut lines = Vec::new();
    for (name, folder) in &settings.worker.build_env {
        let manager = found.iter().find(|manager| {
            (manager.caches.iter()).any(|(cache, dir)| {
                *cache == name.as_str() && std::path::Path::new(dir) == folder.as_path()
            })
        });
        if let Some(manager) = manager {
            lines.push(Line::ok(
                subject(&format!("build_env {}", name.as_str())),
                format!(
                    "no longer needed, since the sandbox sets it to `{}` for {} already: you can delete it",
                    folder.as_path().display(),
                    manager.name
                ),
            ));
        }
    }
    lines
}
