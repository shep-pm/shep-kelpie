//! The installed sources and docs of dependencies and toolchains, which every
//! call reads and none of them writes
//!
//! A reviewer reads the source of the crate a call uses, and a worker the
//! version its lockfile pins. Each folder comes from the tool's own variable,
//! or its default home where that is unset, and one not on this machine adds
//! nothing. A variable naming a folder relative to where the tool ran names
//! nothing: each call runs its tool from a different folder. Nothing here is
//! writable, and a credential file beside a readable folder stays in
//! [`crate::profile::CREDENTIALS`] or [`cargo_credentials`].

use std::ffi::OsString;
use std::path::{Component, MAIN_SEPARATOR_STR, Path, PathBuf};

use crate::ports::AgentCall;

/// What a cargo home holds of its dependencies' source, at any depth
const CARGO_SRC: [&str; 2] = ["registry/src", "git/checkouts"];

/// What a rustup toolchain holds of the standard library's source and its docs
const RUSTUP_SRC: [&str; 2] = ["lib/rustlib/src", "share/doc"];

/// The folders of a dependency or toolchain a call may read
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DepSources {
    folders: Vec<PathBuf>,
}

impl DepSources {
    /// Discovered from the toolchain homes under `home` and the variables in `env`.
    /// A tool that is not installed, or names a folder that is not there, adds nothing.
    pub fn discover(home: &Path, env: &dyn Env) -> Self {
        let mut found = Self::default();
        found.add_cargo(home, env);
        found.add_rustup(home, env);
        found
    }

    // The unpacked crates and the git checkouts, under `CARGO_HOME` and under
    // cargo's own default home.
    fn add_cargo(&mut self, home: &Path, env: &dyn Env) {
        for dir in tool_homes(env, "CARGO_HOME", home, ".cargo") {
            self.folders.extend(under(&dir, &CARGO_SRC));
        }
    }

    // Each installed toolchain's standard library source and its docs, under
    // `RUSTUP_HOME` and under rustup's own default home.
    fn add_rustup(&mut self, home: &Path, env: &dyn Env) {
        for dir in tool_homes(env, "RUSTUP_HOME", home, ".rustup") {
            self.folders.extend(rustup_srcs(&dir));
        }
    }

    /// The folders, in the order they were found
    pub fn folders(&self) -> &[PathBuf] {
        &self.folders
    }
}

/// One environment variable, or `None` when it is unset
pub trait Env {
    /// The variable `name`, or `None` when it is unset
    fn get(&self, name: &str) -> Option<String>;
}

/// The process's own environment, which is what each tool the call runs reads
#[derive(Debug)]
pub struct ProcessEnv;

impl Env for ProcessEnv {
    fn get(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    }
}

/// The source folders a cargo home is expected to hold, whether or not they are
/// there yet. A project's `build_env` puts its worker's cargo home in the work
/// item's build folder, and the call runs cargo itself: its crates appear while
/// it runs, and a grant that names a folder which is not there when the call
/// starts may be refused.
pub fn expected_cargo_srcs(dir: &Path) -> Vec<PathBuf> {
    CARGO_SRC.iter().map(|sub| dir.join(sub)).collect()
}

/// The source and docs a rustup home is expected to hold, whether or not they
/// are there yet: each installed toolchain's, in name order, or, where none is
/// installed yet, its `toolchains` folder whole. A toolchain's name is not known
/// until the call installs one, so the whole folder is the narrowest grant that
/// covers a toolchain made while the call runs.
pub fn expected_rustup_srcs(dir: &Path) -> Vec<PathBuf> {
    let installed = rustup_srcs(dir);
    match installed.is_empty() {
        true => vec![dir.join("toolchains")],
        false => installed,
    }
}

/// The source and docs of every toolchain under the rustup home at `dir`, each
/// one there, in toolchain name order.
fn rustup_srcs(dir: &Path) -> Vec<PathBuf> {
    toolchains(dir)
        .iter()
        .flat_map(|toolchain| under(toolchain, &RUSTUP_SRC))
        .collect()
}

// Each of `subs` under `root` that is a folder
fn under(root: &Path, subs: &[&str]) -> Vec<PathBuf> {
    subs.iter()
        .map(|sub| root.join(sub))
        .filter(|folder| folder.is_dir())
        .collect()
}

// The toolchains installed under the rustup home `dir`, in name order, so the
// folders come out the same way every run
fn toolchains(rustup: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(rustup.join("toolchains")) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .collect();
    found.sort();
    found
}

// The homes a tool keeps its cache in: the one its variable names, then the
// tool's `default` under `home`, each spelled once. Both are kept, since a
// moved cache may leave crates the default folder fetched earlier. A relative
// variable names nothing, as each call runs its tool from its own folder.
fn tool_homes(env: &dyn Env, name: &str, home: &Path, default: &str) -> Vec<PathBuf> {
    let default = normalize(&home.join(default));
    let mut homes = Vec::new();
    homes.extend(named(env, name).filter(|named| *named != default));
    homes.push(default);
    homes
}

// The absolute folder the variable `name` names, normalized, or `None`
fn named(env: &dyn Env, name: &str) -> Option<PathBuf> {
    env.get(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|named| named.is_absolute())
        .map(|named| normalize(&named))
}

// `path` with its `.` and `..` resolved and its trailing separator gone, by
// spelling alone, its Windows prefix kept ahead of the root. Not
// `fs::canonicalize`, which needs the folder to exist and follows links: a tool
// variable can name a folder that is not there.
fn normalize(path: &Path) -> PathBuf {
    let mut parts: Vec<OsString> = Vec::new();
    let mut prefix = None;
    let mut root = false;
    for part in path.components() {
        match part {
            Component::Prefix(found) => prefix = Some(found.as_os_str().to_os_string()),
            Component::RootDir => root = true,
            Component::CurDir => {}
            Component::ParentDir => match parts.last() {
                Some(last) if last != ".." => {
                    parts.pop();
                }
                _ => parts.push(OsString::from("..")),
            },
            Component::Normal(name) => parts.push(name.to_os_string()),
        }
    }
    let mut out = prefix.map(PathBuf::from).unwrap_or_default();
    if root {
        out.push(MAIN_SEPARATOR_STR);
    }
    for part in parts {
        out.push(part);
    }
    out
}

/// The tool homes themselves, where they are on this machine: what each tool
/// reads to run at all, its binaries, its config and its cache
///
/// Cargo's, rustup's and bun's homes are read whole, and Go's `GOPATH` gives its
/// `bin` and module cache. None that holds `home` is read back, or it would undo
/// the home's deny. A credential inside one stays denied by name, in
/// [`cargo_credentials`]; Go's and bun's homes hold none of their own.
pub fn toolchain_homes(home: &Path, env: &dyn Env) -> Vec<PathBuf> {
    let gopaths = tool_homes(env, "GOPATH", home, "go");
    let found = [("CARGO_HOME", ".cargo"), ("RUSTUP_HOME", ".rustup")]
        .into_iter()
        .flat_map(|(name, default)| tool_homes(env, name, home, default))
        .chain(gopaths.iter().map(|gopath| gopath.join("bin")))
        .chain(named(env, "GOMODCACHE"))
        .chain(gopaths.iter().map(|gopath| gopath.join("pkg/mod")))
        .chain(tool_homes(env, "BUN_INSTALL", home, ".bun"));
    let mut homes: Vec<PathBuf> = Vec::new();
    for dir in found {
        if dir.is_dir() && !home.starts_with(&dir) && !homes.contains(&dir) {
            homes.push(dir);
        }
    }
    homes
}

/// The credential files of each cargo home in `homes`, spelled for a deny rule
///
/// [`crate::profile::CREDENTIALS`] names the default home's alone, so a grant of
/// the whole home `CARGO_HOME` moved elsewhere would open the registry token
/// beside it. Each is named whether or not the file is there: a call that runs
/// `cargo login` writes one while it runs, and the deny is made before it starts.
pub fn cargo_credentials(homes: &[PathBuf]) -> Vec<String> {
    homes
        .iter()
        .flat_map(|home| {
            ["credentials", "credentials.toml"].map(|name| format!("{}/{}", home.display(), name))
        })
        .collect()
}

/// The dependency and toolchain sources `home`'s machine holds, as it holds them
/// now, for a call that has no build folder of its own
pub fn machine_sources(home: &Path) -> Vec<PathBuf> {
    let found = DepSources::discover(home, &ProcessEnv);
    found.folders().to_vec()
}

/// Add each of `folders` to `call`'s read list and its fence's read carve-outs,
/// skipping one the call already holds. The write list is untouched, so a call
/// reads these and writes none of them.
pub fn grant(call: &mut AgentCall, folders: &[PathBuf]) {
    let held = |folders: &[PathBuf], folder: &Path| {
        folders
            .iter()
            .any(|h| folder == h.as_path() || folder.starts_with(h))
    };
    for folder in folders {
        if held(&call.reach.read, folder) {
            continue;
        }
        call.reach.read.push(folder.clone());
        if let Some(fence) = call
            .reach
            .fence
            .as_mut()
            .filter(|fence| !held(&fence.read, folder))
        {
            fence.read.push(folder.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[cfg(windows)]
    #[test]
    fn normalize_keeps_a_windows_path_s_prefix_ahead_of_its_root() {
        for (spelled, kept) in [
            (r"C:\Users\R\.cargo", r"C:\Users\R\.cargo"),
            (r"C:\Users\R\a\..\.cargo", r"C:\Users\R\.cargo"),
            (r"C:\Users\R\.cargo\", r"C:\Users\R\.cargo"),
            (r"C:\..\..\x", r"C:\..\..\x"),
            (r"\\?\C:\Users\R\.cargo", r"\\?\C:\Users\R\.cargo"),
            (r"\\?\C:\Users\R\a\..\.cargo", r"\\?\C:\Users\R\.cargo"),
        ] {
            assert_eq!(normalize(Path::new(spelled)), Path::new(kept), "{spelled}");
        }
    }

    /// The variables in `vars`, as an [`Env`]
    #[derive(Default)]
    struct Fixed(BTreeMap<&'static str, String>);

    impl Env for Fixed {
        fn get(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
    }

    // A file at `rel` under `root`, with its folders made
    fn file_at(root: &Path, rel: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "x\n").unwrap();
    }

    #[test]
    fn the_cargo_home_named_by_its_variable_is_the_one_it_reads() {
        let dir = tempfile::tempdir().unwrap();
        let (home, named) = (dir.path(), dir.path().join("builds/7/cargo"));
        file_at(&named, "registry/src/serde-1.0.2/src/lib.rs");
        file_at(&home.join(".cargo"), "registry/src/rand-0.9.0/src/lib.rs");

        let found = DepSources::discover(
            home,
            &Fixed(BTreeMap::from([(
                "CARGO_HOME",
                named.to_string_lossy().into_owned(),
            )])),
        );

        assert_eq!(
            found.folders(),
            [named.join("registry/src"), home.join(".cargo/registry/src"),]
        );
    }

    #[test]
    fn a_rustup_home_gives_each_toolchains_library_source_and_docs() {
        let dir = tempfile::tempdir().unwrap();
        let rustup = dir.path().join(".rustup");
        for rel in [
            "toolchains/nightly-aarch64/lib/rustlib/src/rust/library/std/src/lib.rs",
            "toolchains/stable-aarch64/share/doc/rust/index.html",
            "toolchains/1.88-aarch64/lib/libstd.dylib",
            "toolchains/1.88-aarch64/lib/rustlib/components",
            "downloads/crates",
            "settings.toml",
        ] {
            file_at(&rustup, rel);
        }

        let found = DepSources::discover(dir.path(), &Fixed::default());

        assert_eq!(
            found.folders(),
            [
                rustup.join("toolchains/nightly-aarch64/lib/rustlib/src"),
                rustup.join("toolchains/stable-aarch64/share/doc"),
            ]
        );
    }

    #[test]
    fn a_relative_cargo_home_names_no_folder_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        file_at(&home.join(".cargo"), "registry/src/rand-0.9.0/src/lib.rs");
        file_at(
            &home.join("elsewhere"),
            "registry/src/serde-1.0.2/src/lib.rs",
        );

        // A relative tool variable means a folder under wherever the tool was
        // run, which is a different one for every call, so it names nothing.
        let found = DepSources::discover(
            home,
            &Fixed(BTreeMap::from([("CARGO_HOME", "elsewhere".to_owned())])),
        );

        assert_eq!(found.folders(), [home.join(".cargo/registry/src")]);
    }

    #[test]
    fn a_home_named_twice_over_is_read_once() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        file_at(&home.join(".cargo"), "registry/src/rand-0.9.0/src/lib.rs");
        // The named home is the default one, spelled through a folder that is
        // there, so both spellings reach the same crates.
        file_at(home, "tools/keep.txt");
        let named = home.join("tools/../.cargo/");

        let found = DepSources::discover(
            home,
            &Fixed(BTreeMap::from([(
                "CARGO_HOME",
                named.to_string_lossy().into_owned(),
            )])),
        );

        assert_eq!(found.folders(), [home.join(".cargo/registry/src")]);
    }

    #[test]
    fn a_cargo_home_moved_by_its_variable_keeps_its_credentials_named() {
        let dir = tempfile::tempdir().unwrap();
        let (home, moved) = (dir.path(), dir.path().join("shep/cargo"));
        file_at(&moved, "registry/src/serde-1.0.2/src/lib.rs");
        file_at(&moved, "credentials.toml");
        let env = Fixed(BTreeMap::from([(
            "CARGO_HOME",
            moved.to_string_lossy().into_owned(),
        )]));

        let homes = toolchain_homes(home, &env);
        assert!(homes.contains(&moved), "{homes:?}");
        // The home whole is what a call reads back, so each credential file it
        // could hold is named beside it, the one not on disk included.
        let named = cargo_credentials(&homes);
        for name in ["credentials", "credentials.toml"] {
            let own = moved.join(name).to_string_lossy().into_owned();
            assert!(named.contains(&own), "{own}: {named:?}");
        }
    }

    #[test]
    fn go_s_bin_and_module_cache_and_bun_s_home_are_read_back_and_no_other_folder() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        for rel in [
            "go/bin/golangci-lint",
            "go/pkg/mod/x/go.mod",
            "go/pkg/sumdb/sum.golang.org/latest",
            "go/src/other/main.go",
            ".bun/bin/bun",
            "other-repo/.env",
        ] {
            file_at(home, rel);
        }

        let homes = toolchain_homes(home, &Fixed::default());

        assert_eq!(
            homes,
            [
                home.join("go/bin"),
                home.join("go/pkg/mod"),
                home.join(".bun")
            ]
        );
    }

    #[test]
    fn a_gopath_and_a_module_cache_set_elsewhere_are_the_ones_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let (gopath, modcache) = (home.join("work/gopath"), home.join("caches/gomod"));
        file_at(&gopath, "bin/golangci-lint");
        file_at(&gopath, "pkg/mod/y/go.mod");
        file_at(&modcache, "z/go.mod");
        let env = Fixed(BTreeMap::from([
            ("GOPATH", gopath.to_string_lossy().into_owned()),
            ("GOMODCACHE", modcache.to_string_lossy().into_owned()),
        ]));

        let homes = toolchain_homes(home, &env);

        assert_eq!(
            homes,
            [gopath.join("bin"), modcache, gopath.join("pkg/mod")]
        );
    }

    #[test]
    fn a_toolchain_not_on_the_machine_adds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        file_at(dir.path(), "other-repo/src/main.go");
        let env = Fixed(BTreeMap::from([
            (
                "GOPATH",
                dir.path().join("nowhere").to_string_lossy().into_owned(),
            ),
            (
                "BUN_INSTALL",
                dir.path().join("none").to_string_lossy().into_owned(),
            ),
        ]));

        assert!(toolchain_homes(dir.path(), &env).is_empty());
    }

    // A tool home read back whole that holds the home would undo its deny.
    #[test]
    fn a_tool_variable_naming_the_home_or_above_it_reads_none_of_it_back() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("me");
        file_at(&home, "bin/tool");
        file_at(&home, "other-repo/.env");
        let env = Fixed(BTreeMap::from([
            ("BUN_INSTALL", home.to_string_lossy().into_owned()),
            ("CARGO_HOME", dir.path().to_string_lossy().into_owned()),
            ("GOPATH", home.to_string_lossy().into_owned()),
        ]));

        assert_eq!(toolchain_homes(&home, &env), [home.join("bin")]);
    }

    #[test]
    fn a_cargo_home_gives_its_unpacked_sources_and_none_of_its_cache() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let cargo = home.join(".cargo");
        for rel in [
            "registry/src/index.crates.io-6a1f/shep-core-0.10.0/src/lib.rs",
            "git/checkouts/shep-core-1a2b3c/HEAD/src/lib.rs",
            "registry/cache/index.crates.io-6a1f/shep-core-0.10.0.crate",
            "registry/index/index.crates.io-6a1f/.cache",
            "credentials.toml",
        ] {
            file_at(&cargo, rel);
        }

        let found = DepSources::discover(home, &Fixed(BTreeMap::new()));

        assert_eq!(
            found.folders(),
            [
                home.join(".cargo/registry/src"),
                home.join(".cargo/git/checkouts"),
            ]
        );
    }
}
