//! The registry port over `curl`
//!
//! One GET on the host `package_managers` names for the registry: the crates.io
//! sparse index, `registry.npmjs.org` or `pypi.org`. A 404 says the package or
//! version is not there. Any other answer that is no success, and a private
//! registry the host does not serve, says nothing, so it is an error.

use super::curl::{render, run};
use crate::board::check::{Needed, Registry as Kind};
use crate::ports::{AlertError, Registry, RegistryError};

/// Seconds curl gets to connect, and to finish the read
const CONNECT_TIMEOUT: u32 = 5;
const MAX_TIME: u32 = 15;

/// Reads the public registries with the system's `curl`
#[derive(Debug, Clone, Copy, Default)]
pub struct CurlRegistry;

impl Registry for CurlRegistry {
    fn published(&self, needed: &Needed) -> Result<bool, RegistryError> {
        read(&get, needed)
    }
}

// What a read gives back: the body, then the HTTP status.
type Got = Result<(String, u16), RegistryError>;

// One GET of `url` through curl.
fn get(url: &str) -> Got {
    let config = render(&[
        ("url", url.to_owned()),
        ("proto", "=https".to_owned()),
        ("connect-timeout", CONNECT_TIMEOUT.to_string()),
        ("max-time", MAX_TIME.to_string()),
        // The status goes on a line of its own after the body.
        ("write-out", "\n%{http_code}".to_owned()),
    ]);
    run(&config).map_err(|error| match error {
        AlertError::Spawn(why) => RegistryError::Spawn(why),
        AlertError::Unreachable(code) => RegistryError::Unreachable(code),
        AlertError::Refused(status) => RegistryError::Refused(status),
        AlertError::Unreadable => RegistryError::Unreadable,
    })
}

// Whether `needed` is published, asking `get` for the one URL its registry keeps it at.
fn read(get: &dyn Fn(&str) -> Got, needed: &Needed) -> Result<bool, RegistryError> {
    let url = url(needed).ok_or(RegistryError::Unreadable)?;
    let (body, status) = get(&url)?;
    match (status, needed.registry) {
        (404, _) => Ok(false),
        (200, Kind::Crates) => in_index(&body, &needed.version),
        (200, Kind::Npm) => same_version(&body, &["version"], &needed.version),
        (200, Kind::Pypi) => same_version(&body, &["info", "version"], &needed.version),
        (status, _) => Err(RegistryError::Refused(status)),
    }
}

// The URL of `needed`, or none where its name or version could not be a package's, which
// keeps a path or a query out of the URL.
fn url(needed: &Needed) -> Option<String> {
    let Needed {
        registry,
        name,
        version,
    } = needed;
    let word = |s: &str, extra: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
    };
    if !word(version, "._+-") {
        return None;
    }
    match registry {
        Kind::Crates if word(name, "_-") => {
            let name = name.to_ascii_lowercase();
            // The index shards by the name's first characters, as Cargo does.
            let path = match name.len() {
                1 | 2 => format!("{}/{name}", name.len()),
                3 => format!("3/{}/{name}", &name[..1]),
                _ => format!("{}/{}/{name}", &name[..2], &name[2..4]),
            };
            Some(format!("https://index.crates.io/{path}"))
        }
        Kind::Npm => {
            // A scope's `/` is `%2f` in the one path segment the registry reads.
            let (scope, bare) = match name.strip_prefix('@') {
                Some(rest) => rest
                    .split_once('/')
                    .map_or((None, ""), |(s, b)| (Some(s), b)),
                None => (None, name.as_str()),
            };
            let named = word(bare, "._~-") && scope.is_none_or(|s| word(s, "._~-"));
            let name = match scope {
                Some(scope) => format!("@{scope}%2f{bare}"),
                None => bare.to_owned(),
            };
            named.then(|| format!("https://registry.npmjs.org/{name}/{version}"))
        }
        Kind::Pypi if word(name, "._-") => {
            Some(format!("https://pypi.org/pypi/{name}/{version}/json"))
        }
        Kind::Crates | Kind::Pypi => None,
    }
}

// Whether the sparse index's lines, one JSON object per version, hold `version`.
fn in_index(body: &str, version: &str) -> Result<bool, RegistryError> {
    let mut found = false;
    for line in body.lines().filter(|l| !l.trim().is_empty()) {
        let line: serde_json::Value =
            serde_json::from_str(line).map_err(|_| RegistryError::Unreadable)?;
        let vers = line.get("vers").and_then(|v| v.as_str());
        found |= vers.ok_or(RegistryError::Unreadable)? == version;
    }
    Ok(found)
}

// Whether the JSON `body` names `version` at `path`, which a registry's answer for a
// version it holds does.
fn same_version(body: &str, path: &[&str], version: &str) -> Result<bool, RegistryError> {
    let mut at: serde_json::Value =
        serde_json::from_str(body).map_err(|_| RegistryError::Unreadable)?;
    for key in path {
        at = at
            .get_mut(*key)
            .map(serde_json::Value::take)
            .unwrap_or_default();
    }
    match at.as_str() {
        Some(named) if named == version => Ok(true),
        _ => Err(RegistryError::Unreadable),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    const CRATES_INDEX: &str =
        include_str!("../../../../fixtures/registry-crates-command-fds.jsonl");
    const CRATES_NO_KEY: &str =
        include_str!("../../../../fixtures/registry-crates-no-such-key.xml");
    const NPM_MS: &str = include_str!("../../../../fixtures/registry-npm-ms-2.1.3.json");
    const NPM_TYPES_MS: &str =
        include_str!("../../../../fixtures/registry-npm-types-ms-2.1.0.json");
    const NPM_NOT_FOUND: &str =
        include_str!("../../../../fixtures/registry-npm-version-not-found.json");
    const PYPI_SIX: &str = include_str!("../../../../fixtures/registry-pypi-six-1.16.0.json");
    const PYPI_NOT_FOUND: &str = include_str!("../../../../fixtures/registry-pypi-not-found.json");

    fn needing(registry: Kind, name: &str, version: &str) -> Needed {
        Needed {
            registry,
            name: name.to_owned(),
            version: version.to_owned(),
        }
    }

    // Reads `needed` from a registry that answers `status` and `body`, and returns the
    // answer with the one URL it was asked.
    fn answered(needed: &Needed, status: u16, body: &str) -> (Result<bool, RegistryError>, String) {
        let asked = RefCell::new(Vec::new());
        let get = |url: &str| -> Got {
            asked.borrow_mut().push(url.to_owned());
            Ok((body.to_owned(), status))
        };
        let read = read(&get, needed);
        let [url] = asked.into_inner().try_into().expect("one GET");
        (read, url)
    }

    #[test]
    fn a_published_crate_reads_as_published() {
        let needed = needing(Kind::Crates, "command-fds", "0.3.2");

        let (read, url) = answered(&needed, 200, CRATES_INDEX);

        assert_eq!(read, Ok(true));
        assert_eq!(url, "https://index.crates.io/co/mm/command-fds");
    }

    #[test]
    fn a_version_not_in_the_index_reads_as_unpublished() {
        let (newer, _) = answered(
            &needing(Kind::Crates, "command-fds", "9.9.9"),
            200,
            CRATES_INDEX,
        );
        assert_eq!(newer, Ok(false));

        // A crate the index has no file for answers 404
        let (missing, url) = answered(
            &needing(Kind::Crates, "Kelpie-No-Such-Crate", "0.1.0"),
            404,
            CRATES_NO_KEY,
        );
        assert_eq!(missing, Ok(false));
        assert_eq!(url, "https://index.crates.io/ke/lp/kelpie-no-such-crate");
    }

    #[test]
    fn a_crate_is_found_at_the_path_cargo_shards_its_name_to() {
        for (name, path) in [
            ("a", "1/a"),
            ("ab", "2/ab"),
            ("abc", "3/a/abc"),
            ("abcd", "ab/cd/abcd"),
        ] {
            let (_, url) = answered(&needing(Kind::Crates, name, "1.0.0"), 404, "");
            assert_eq!(url, format!("https://index.crates.io/{path}"));
        }
    }

    #[test]
    fn an_npm_and_a_pypi_package_read_from_their_hosts() {
        let cases = [
            (
                needing(Kind::Npm, "ms", "2.1.3"),
                NPM_MS,
                NPM_NOT_FOUND,
                "https://registry.npmjs.org/ms/2.1.3",
            ),
            (
                needing(Kind::Npm, "@types/ms", "2.1.0"),
                NPM_TYPES_MS,
                NPM_NOT_FOUND,
                "https://registry.npmjs.org/@types%2fms/2.1.0",
            ),
            (
                needing(Kind::Pypi, "six", "1.16.0"),
                PYPI_SIX,
                PYPI_NOT_FOUND,
                "https://pypi.org/pypi/six/1.16.0/json",
            ),
        ];
        for (needed, found, not_found, want) in cases {
            let (read, url) = answered(&needed, 200, found);
            assert_eq!((read, url.as_str()), (Ok(true), want), "{needed}");

            let (read, url) = answered(&needed, 404, not_found);
            assert_eq!((read, url.as_str()), (Ok(false), want), "{needed}");
        }
    }

    #[test]
    fn a_refused_read_is_an_error() {
        // A private registry answers 401 or 403 where the public one has no say
        for status in [401, 403, 429, 500] {
            let (got, _) = answered(&needing(Kind::Npm, "@acme/private", "1.0.0"), status, "");
            assert_eq!(got, Err(RegistryError::Refused(status)), "{status}");
        }
        // An answer that is not the registry's shape says nothing either
        let (got, _) = answered(
            &needing(Kind::Crates, "command-fds", "0.3.2"),
            200,
            "<html>",
        );
        assert_eq!(got, Err(RegistryError::Unreadable));
        let (got, _) = answered(&needing(Kind::Pypi, "six", "1.16.0"), 200, NPM_MS);
        assert_eq!(got, Err(RegistryError::Unreadable));
        // A read that never reached the registry
        let down = |_: &str| -> Got { Err(RegistryError::Unreachable(7)) };
        let unreached = read(&down, &needing(Kind::Pypi, "six", "1.16.0"));
        assert_eq!(unreached, Err(RegistryError::Unreachable(7)));
    }

    #[test]
    fn a_name_that_could_not_be_a_package_is_never_put_in_a_url() {
        let never = |url: &str| -> Got { panic!("asked {url}") };
        for needed in [
            needing(Kind::Crates, "../etc", "1.0.0"),
            needing(Kind::Npm, "ms/../../x", "1.0.0"),
            needing(Kind::Npm, "@scope", "1.0.0"),
            needing(Kind::Pypi, "six?x=1", "1.0.0"),
            needing(Kind::Pypi, "six", "1.0.0/../x"),
        ] {
            assert_eq!(
                read(&never, &needed),
                Err(RegistryError::Unreadable),
                "{needed}"
            );
        }
    }
}
