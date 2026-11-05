//! The retro's own earlier findings that are still open
//!
//! A report ends with a `## New findings` section of bullets, each
//! `- <theme>: <what>`. Reports stay in `retro-reports/unverified` until the
//! maintainer consolidates them, so the themes of the reports still there are
//! the open known gaps. The next retro is told them, asked for new findings
//! only, and asked to count a gap that recurs by its theme. The list holds the
//! newest [`LISTED`] of them and counts the older ones it left out.

use std::fs;
use std::path::Path;

/// The title of the section the report's findings sit under, as `## New findings`
pub(super) const HEADING: &str = "New findings";

/// The most themes the prompt lists
pub(super) const LISTED: usize = 40;

/// The most characters of a theme
const THEME_LEN: usize = 80;

/// The themes of `report`'s findings
pub(super) fn themes(report: &str) -> Vec<String> {
    let mut lines = report.lines().map(str::trim);
    if !lines.any(is_heading) {
        return Vec::new();
    }
    (lines.take_while(|l| !l.starts_with('#')))
        .filter_map(|l| l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")))
        .map(|bullet| bullet.replace(['*', '`'], ""))
        .map(|bullet| {
            let theme = bullet.split(':').next().unwrap_or_default();
            theme.trim().chars().take(THEME_LEN).collect::<String>()
        })
        .filter(|theme| !theme.is_empty())
        .collect()
}

// Whether `line` is the findings heading at any level, numbered or bold
fn is_heading(line: &str) -> bool {
    let words = line.trim_start_matches(|c: char| {
        c == '#' || c == '*' || c == '.' || c.is_ascii_digit() || c.is_whitespace()
    });
    let words = words.trim_end_matches(|c: char| c == '*' || c == ':' || c.is_whitespace());
    words.eq_ignore_ascii_case(HEADING)
}

/// The open known gaps the retro is told, and what its limit left out
#[derive(Debug, Default)]
pub(super) struct Open {
    /// The themes the prompt lists, oldest report first, each once
    pub listed: Vec<String>,
    /// How many themes older than these the limit dropped
    pub left_out: usize,
}

/// The open known gaps: the themes in `project`'s reports kept in `folder`,
/// oldest report first, each once, and the newest [`LISTED`] of them, with how
/// many older ones that left out. A folder or report that cannot be read adds
/// none.
pub(super) fn open(folder: &Path, project: &str) -> Open {
    let Ok(entries) = fs::read_dir(folder) else {
        return Open::default();
    };
    // The folder is every project's, and a report's name begins with its project's.
    let prefix = format!("{project}-batch-");
    let mut files: Vec<_> = (entries.filter_map(|e| Some(e.ok()?.path())))
        .filter(|f| {
            let name = f.file_name().and_then(|n| n.to_str());
            name.is_some_and(|n| n.starts_with(&prefix))
        })
        .collect();
    files.sort();
    let mut gaps: Vec<String> = Vec::new();
    for file in files {
        let text = fs::read_to_string(file).unwrap_or_default();
        for theme in themes(&text) {
            if !gaps.iter().any(|g| g.eq_ignore_ascii_case(&theme)) {
                gaps.push(theme);
            }
        }
    }
    let left_out = gaps.len().saturating_sub(LISTED);
    gaps.drain(..left_out);
    Open {
        listed: gaps,
        left_out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = "# Batch retro: koji #1\n\n---\n\n## Patterns\n\n- not a finding: x\n\n\
        ## New findings\n\n- **Sandbox cannot bind a socket**: tests fail there\n\
        * `state VERSION` conflicts: every item\n\n## Later\n\n- nor this: y\n";

    #[test]
    fn the_themes_are_the_bullets_under_the_findings_heading() {
        assert_eq!(
            themes(REPORT),
            ["Sandbox cannot bind a socket", "state VERSION conflicts"]
        );
    }

    #[test]
    fn a_report_with_no_findings_heading_has_no_themes() {
        assert_eq!(themes("## Patterns\n\n- a: b\n"), Vec::<String>::new());
    }

    #[test]
    fn the_open_gaps_are_the_reports_themes_once_each_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("koji-batch-1.md"), REPORT).unwrap();
        fs::write(
            dir.path().join("koji-batch-2.md"),
            "## New findings\n\n- sandbox cannot bind a socket: again\n- slow CI: new\n",
        )
        .unwrap();

        fs::write(
            dir.path().join("alpha-batch-1.md"),
            "## New findings\n\n- stale lockfile of alpha: x\n",
        )
        .unwrap();

        let gaps = open(dir.path(), "koji");
        assert_eq!(
            gaps.listed,
            [
                "Sandbox cannot bind a socket",
                "state VERSION conflicts",
                "slow CI"
            ]
        );
        assert_eq!(gaps.left_out, 0);
        assert_eq!(
            open(&dir.path().join("none"), "koji").listed,
            Vec::<String>::new()
        );
    }

    #[test]
    fn the_findings_heading_is_found_at_any_level_numbered_or_bold() {
        for heading in [
            "## New findings",
            "### New findings",
            "## 4. New findings",
            "**New findings**",
            "## New Findings:",
        ] {
            let report = format!("{heading}\n\n- a gap: x\n");
            assert_eq!(themes(&report), ["a gap"], "{heading}");
        }
    }

    #[test]
    fn gaps_past_the_limit_are_counted() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..LISTED + 5 {
            let file = dir.path().join(format!("koji-batch-{n:03}.md"));
            fs::write(file, format!("## New findings\n\n- gap {n}: x\n")).unwrap();
        }

        let gaps = open(dir.path(), "koji");

        assert_eq!(gaps.listed.len(), LISTED);
        assert_eq!(gaps.left_out, 5);
        assert_eq!(gaps.listed.first().unwrap(), "gap 5");
        assert_eq!(gaps.listed.last().unwrap(), &format!("gap {}", LISTED + 4));
    }
}
