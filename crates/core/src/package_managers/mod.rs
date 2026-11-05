//! The package managers a project can keep, and the registries they fetch from
//!
//! One table for every reader: the sandbox of a call that works an item reaches
//! the registries of the managers its worktree keeps a file for (detection is
//! always on), the dev server's install runs the manager whose lockfile the
//! checkout keeps, and `doctor` names each manager it finds.

use std::path::{Path, PathBuf};

/// A package manager, known by the files a project keeps for it
#[derive(Debug, PartialEq, Eq)]
pub struct PackageManager {
    /// The name `doctor` says
    pub name: &'static str,
    /// The lockfiles it writes, any of which shows the project uses it
    pub lockfiles: &'static [&'static str],
    /// The manifests it reads, which show the project uses it where no lockfile is kept
    pub manifests: &'static [&'static str],
    /// The registries it fetches from
    pub hosts: &'static [&'static str],
    /// The variables that put its cache in the build folder, with the folder in it
    ///
    /// Each name is uppercase, since a `build_env` name is.
    pub caches: &'static [(&'static str, &'static str)],
    /// What installs the packages from the lockfile as it stands, with no package's install
    /// script, for the managers the dev server installs with
    pub install: Option<&'static str>,
}

const NPM_HOST: &str = "registry.npmjs.org";
const YARN_HOST: &str = "registry.yarnpkg.com";
const CARGO_HOSTS: &[&str] = &["crates.io", "index.crates.io", "static.crates.io"];
const GO_HOSTS: &[&str] = &["proxy.golang.org", "sum.golang.org"];
const PYTHON_HOSTS: &[&str] = &["pypi.org", "files.pythonhosted.org"];

/// Every package manager kelpie knows, in the order a lockfile is looked for
pub const MANAGERS: [PackageManager; 8] = [
    PackageManager {
        name: "npm",
        lockfiles: &["package-lock.json"],
        manifests: &["package.json"],
        hosts: &[NPM_HOST],
        caches: &[("NPM_CONFIG_CACHE", "npm-cache")],
        install: Some("npm ci --ignore-scripts --no-audit --no-fund"),
    },
    PackageManager {
        name: "pnpm",
        lockfiles: &["pnpm-lock.yaml"],
        manifests: &[],
        hosts: &[NPM_HOST],
        caches: &[
            // pnpm 12 reads `PNPM_CONFIG_*` and ignores `NPM_CONFIG_*`, which older pnpm reads.
            // Its store operation lock sits in `XDG_RUNTIME_DIR`, which it will not make.
            ("NPM_CONFIG_STORE_DIR", "pnpm-store"),
            ("PNPM_CONFIG_STORE_DIR", "pnpm-store"),
            ("PNPM_CONFIG_CACHE_DIR", "pnpm-cache"),
            ("PNPM_CONFIG_STATE_DIR", "pnpm-state"),
            ("XDG_RUNTIME_DIR", "pnpm-run"),
        ],
        install: Some("pnpm install --frozen-lockfile --ignore-scripts"),
    },
    PackageManager {
        name: "yarn",
        lockfiles: &["yarn.lock"],
        manifests: &[],
        hosts: &[YARN_HOST, NPM_HOST],
        caches: &[("YARN_CACHE_FOLDER", "yarn-cache")],
        install: Some("yarn install --frozen-lockfile --ignore-scripts"),
    },
    PackageManager {
        name: "bun",
        lockfiles: &["bun.lock", "bun.lockb"],
        manifests: &[],
        hosts: &[NPM_HOST],
        caches: &[("BUN_INSTALL_CACHE_DIR", "bun-cache")],
        install: Some("bun install --frozen-lockfile --ignore-scripts"),
    },
    PackageManager {
        name: "cargo",
        lockfiles: &["Cargo.lock"],
        manifests: &["Cargo.toml"],
        hosts: CARGO_HOSTS,
        caches: &[],
        install: None,
    },
    PackageManager {
        name: "go",
        lockfiles: &["go.sum"],
        manifests: &["go.mod"],
        hosts: GO_HOSTS,
        caches: &[("GOMODCACHE", "go-mod"), ("GOCACHE", "go-build")],
        install: None,
    },
    PackageManager {
        name: "uv",
        lockfiles: &["uv.lock"],
        manifests: &[],
        hosts: PYTHON_HOSTS,
        caches: &[("UV_CACHE_DIR", "uv-cache")],
        install: None,
    },
    PackageManager {
        name: "pip",
        lockfiles: &[],
        manifests: &["requirements.txt", "pyproject.toml"],
        hosts: PYTHON_HOSTS,
        caches: &[("PIP_CACHE_DIR", "pip-cache")],
        install: None,
    },
];

/// Folders of a worktree that never hold a project's own manifest
const SKIPPED: [&str; 3] = ["node_modules", "target", "vendor"];

impl PackageManager {
    /// Whether `folder` holds one of its lockfiles
    pub fn locks(&self, folder: &Path) -> bool {
        self.lockfiles.iter().any(|f| folder.join(f).is_file())
    }

    /// Whether `folder` holds one of its lockfiles or manifests
    pub fn kept_in(&self, folder: &Path) -> bool {
        self.locks(folder) || self.manifests.iter().any(|f| folder.join(f).is_file())
    }
}

/// `worktree` and the folders directly under it, where a project keeps its package files
///
/// A folder one level down is read since a project often keeps its web app in one. A hidden
/// folder and the ones in `SKIPPED` are not.
pub fn folders(worktree: &Path) -> Vec<PathBuf> {
    let below = std::fs::read_dir(worktree)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            !name.starts_with('.') && !SKIPPED.contains(&name.as_ref())
        })
        .map(|entry| entry.path())
        .filter(|path| path.is_dir());
    std::iter::once(worktree.to_owned()).chain(below).collect()
}

/// The package managers `worktree` keeps a lockfile or manifest for, in its root or in a
/// folder directly under it, in the table's order
pub fn found_in(worktree: &Path) -> Vec<&'static PackageManager> {
    let folders = folders(worktree);
    MANAGERS
        .iter()
        .filter(|m| folders.iter().any(|folder| m.kept_in(folder)))
        .collect()
}

/// The registries of `managers`, each once, in the order they are first named
pub fn registries(managers: &[&PackageManager]) -> Vec<&'static str> {
    let mut hosts: Vec<&'static str> = Vec::new();
    for host in managers.iter().flat_map(|m| m.hosts) {
        if !hosts.contains(host) {
            hosts.push(host);
        }
    }
    hosts
}

/// The cache variables of `managers`, each once, with the folder of the build folder it names
pub fn caches(managers: &[&PackageManager]) -> Vec<(&'static str, &'static str)> {
    let mut caches: Vec<(&'static str, &'static str)> = Vec::new();
    for cache in managers.iter().flat_map(|m| m.caches) {
        if !caches.iter().any(|(name, _)| *name == cache.0) {
            caches.push(*cache);
        }
    }
    caches
}

#[cfg(test)]
mod tests;
