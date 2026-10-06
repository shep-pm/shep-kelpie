//! `shep kelpie usage [<project>] [--since <date>]` and
//! `shep kelpie usage <project> --import <log file>`: the usage ledger,
//! read here with no shepherd asked

use std::path::Path;

use crate::runner::ProjectName;
use crate::usage::{self, baselines};

/// What `usage` was asked for
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ask<'a> {
    Report { since: Option<&'a str> },
    Import { log: &'a str },
}

/// Runs `usage` with `args`, the project `named` or taken first from them,
/// over the ledgers in `kelpie_home`
///
/// # Errors
///
/// A message when the arguments are not `usage`'s, a date does not read,
/// or a file cannot be read or written.
pub(super) fn usage(
    kelpie_home: &Path,
    named: Option<ProjectName>,
    args: &[&str],
) -> Result<Vec<String>, String> {
    let (project, ask) = read_args(named, args)?;
    match ask {
        Ask::Import { log } => {
            let project = project.ok_or("`usage --import` takes the project to import into")?;
            let text = std::fs::read(log).map_err(|e| format!("cannot read {log}: {e}"))?;
            // A bad byte costs only its own line, which then reads as no call.
            let text = String::from_utf8_lossy(&text);
            let ledger = kelpie_home.join(project.as_str()).join(usage::FILE);
            let imported = usage::import(&text, &ledger)?;
            Ok(vec![format!(
                "{project}: {} calls added from {log}, {} already in the ledger",
                imported.added, imported.known
            )])
        }
        Ask::Report { since } => {
            let since = since.map(since_date).transpose()?;
            let projects = match project {
                Some(project) => vec![project.as_str().to_owned()],
                None => with_ledgers(kelpie_home)?,
            };
            if projects.is_empty() {
                return Ok(vec!["no project has a usage ledger yet".to_owned()]);
            }
            let baselines = baselines::load(kelpie_home);
            let mut out = Vec::new();
            for project in projects {
                let path = kelpie_home.join(&project).join(usage::FILE);
                let lines = usage::read(&path)
                    .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
                if !out.is_empty() {
                    out.push(String::new());
                }
                out.extend(usage::report(&project, &lines, since, &baselines));
            }
            Ok(out)
        }
    }
}

fn read_args<'a>(
    named: Option<ProjectName>,
    args: &[&'a str],
) -> Result<(Option<ProjectName>, Ask<'a>), String> {
    let (positional, ask) = match args {
        [] => (None, Ask::Report { since: None }),
        ["--since", date] => (None, Ask::Report { since: Some(date) }),
        ["--import", log] => (None, Ask::Import { log }),
        [name] if !name.starts_with('-') => (Some(*name), Ask::Report { since: None }),
        [name, "--since", date] if !name.starts_with('-') => {
            (Some(*name), Ask::Report { since: Some(date) })
        }
        [name, "--import", log] if !name.starts_with('-') => (Some(*name), Ask::Import { log }),
        _ => return Err(super::USAGE.to_owned()),
    };
    let project = match (positional, named) {
        (Some(_), Some(_)) => return Err("name the project once".to_owned()),
        (Some(name), None) => Some(ProjectName::try_from(name).map_err(|e| e.to_string())?),
        (None, named) => named,
    };
    Ok((project, ask))
}

// The start of `date`, `YYYY-MM-DD`, in this machine's time zone
fn since_date(date: &str) -> Result<crate::ports::Timestamp, String> {
    let bad = || format!("`--since` takes a date as 2026-10-01, not {date:?}");
    let day: jiff::civil::Date = date.parse().map_err(|_| bad())?;
    let zoned = day
        .to_zoned(jiff::tz::TimeZone::system())
        .map_err(|_| bad())?;
    let seconds = u64::try_from(zoned.timestamp().as_second()).map_err(|_| bad())?;
    Ok(crate::ports::Timestamp(seconds))
}

// The projects under `kelpie_home` that have a ledger, by name
fn with_ledgers(kelpie_home: &Path) -> Result<Vec<String>, String> {
    let entries = match std::fs::read_dir(kelpie_home) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read {}: {e}", kelpie_home.display())),
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().join(usage::FILE).is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| ProjectName::try_from(name.as_str()).is_ok())
        .collect();
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::{Line, append_to};

    #[test]
    fn the_report_reads_each_project_s_ledger_and_an_import_names_its_project() {
        let home = tempfile::tempdir().unwrap();
        let koji = home.path().join("koji");
        std::fs::create_dir_all(&koji).unwrap();
        let line = r#"{"line":"call","at":5,"issue":null,"role":"pm","kind":"wake","usage":{"input":1,"cache_write":0,"cache_read":0,"output":1},"units":6,"cost_usd":0.25,"ended":"answered"}"#;
        let call: Line = serde_json::from_str(line).unwrap();
        append_to(&koji.join(usage::FILE), &call).unwrap();

        let out = usage(home.path(), None, &[]).unwrap();
        assert_eq!(out[0], "koji: 0 merged pull requests");
        assert!(
            out.contains(&"project manager: 1 call, 6 units, $0.25".to_owned()),
            "{out:#?}"
        );

        let err = usage(home.path(), None, &["--import", "shep.log"]).unwrap_err();
        assert!(err.contains("takes the project"), "{err}");
        let err = usage(home.path(), None, &["--since", "yesterday"]).unwrap_err();
        assert!(err.contains("2026-10-01"), "{err}");
        let named = ProjectName::try_from("koji").ok();
        assert!(usage(home.path(), named, &["koji"]).is_err(), "named twice");
    }

    #[test]
    fn a_bad_byte_in_a_log_costs_only_its_line() {
        let home = tempfile::tempdir().unwrap();
        let ended = |at: &str, session: &str| {
            format!(
                "{at} {{\"step\":\"ended\",\"issue\":7,\"session\":\"{session}\",\"usage\":\
                 {{\"input\":1,\"cache_write\":0,\"cache_read\":0,\"output\":1}}}}\n"
            )
        };
        let mut log = ended("2026-10-01T17:00:00Z", "a").into_bytes();
        log.extend(b"2026-10-01T17:01:00Z {\"step\":\"ended\",\"session\":\"\xff\"}\n");
        log.extend(ended("2026-10-01T17:02:00Z", "b").into_bytes());
        let path = home.path().join("koji-0-out.log");
        std::fs::write(&path, log).unwrap();
        let named = ProjectName::try_from("koji").ok();
        let out = usage(home.path(), named, &["--import", path.to_str().unwrap()]).unwrap();
        assert!(out[0].starts_with("koji: 2 calls added"), "{out:?}");
    }

    #[test]
    fn a_kelpie_home_that_cannot_be_listed_is_an_error_not_an_empty_report() {
        let home = tempfile::tempdir().unwrap();
        let not_a_folder = home.path().join("file");
        std::fs::write(&not_a_folder, "").unwrap();
        assert!(usage(&not_a_folder, None, &[]).is_err());
        let missing = home.path().join("missing");
        assert_eq!(
            usage(&missing, None, &[]).unwrap(),
            ["no project has a usage ledger yet"]
        );
    }

    #[test]
    fn a_date_starts_at_midnight_where_the_machine_is() {
        let since = since_date("2026-10-01").unwrap();
        let utc = 1_790_812_800;
        assert!(since.0.abs_diff(utc) <= 14 * 3600, "{since:?}");
    }
}
