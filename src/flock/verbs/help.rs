//! The table of kelpie's top-level verbs, which `USAGE` is drawn from and the
//! manifest declares, so the help and the completion cannot drift apart

/// One form of a verb: `shep kelpie <name> <usage>`, and what it does
#[derive(Debug, Clone, Copy)]
pub struct Row {
    /// The word typed after `shep kelpie`
    pub name: &'static str,
    /// The words after the name, empty when the verb takes none
    pub usage: &'static str,
    /// One line, at most [`ABOUT_WIDTH`] columns
    pub about: &'static str,
}

const fn row(name: &'static str, usage: &'static str, about: &'static str) -> Row {
    Row { name, usage, about }
}

/// The most columns a row's `about` takes
pub const ABOUT_WIDTH: usize = 60;

/// Where each row's `about` starts
const ABOUT_COLUMN: usize = 41;

/// Every top-level verb, a row for each form it takes
///
/// A verb with several forms has a row for each, in the order the help lists
/// them. A form may name other verbs after a `|`, as `board top` does.
pub const TABLE: &[Row] = &[
    row(
        "add",
        "[<project>] [--base <branch>]",
        "register this checkout as a project",
    ),
    row("add", "<ticket>", "put a ticket on the project's board"),
    row("start", "", "run the project"),
    row("pause", "", "stop the project once its calls end"),
    row(
        "finish",
        "[--all]",
        "end the open work items, then stop (--all: every project)",
    ),
    row("status", "[--json]", "show every project, or one with -p"),
    row(
        "rule",
        "[<id> <answer>]",
        "answer a ruling, or list those waiting",
    ),
    row("rework", "<pr>", "rework a pull request"),
    row("adopt", "<pr>", "adopt a pull request"),
    row(
        "gate",
        "[<ticket>]",
        "send a work item into the review gate",
    ),
    row("drop", "[<ticket>]", "end a work item without merging it"),
    row(
        "triage",
        "<issue>",
        "triage an issue next, past the pick rule",
    ),
    row("board", "", "list the ready issues in the board's order"),
    row(
        "board",
        "move <issue> <position> | board top <issue>",
        "put an issue at a place in that list, or first",
    ),
    row(
        "timings",
        "[<n>]",
        "show where the last n finished items' time went",
    ),
    row(
        "issue",
        "check <ticket>",
        "check an issue holds every part an implementer needs",
    ),
    row(
        "issue",
        "check --draft <file> [--label <label>]...",
        "check an issue not yet filed, with no shepherd",
    ),
    row("attach", "<ticket>", "steer a work item's worker here"),
    row(
        "tell",
        "\"<note>\"",
        "leave a note for the project manager's next wake",
    ),
    row("pm", "", "steer the project manager's session here"),
    row("drain", "", "hold back every new call"),
    row("undrain", "", "let new calls start"),
    row(
        "get",
        "[<key>]",
        "show the project's settings, or one by its dotted key",
    ),
    row(
        "set",
        "<key> <value>",
        "change one setting, such as git.merging auto",
    ),
    row(
        "get",
        "-p '<glob>' <key>",
        "read a setting of every project a glob matches",
    ),
    row(
        "set",
        "-p '<glob>' <key> <value>",
        "write a setting of every match, all or none",
    ),
    row("ticket", "", "file and read a local project's tickets"),
    row(
        "ticket",
        "new \"<title>\" [--body <text>] [--label <label>]... [--parent <id>] [--blocked-by <id>]...",
        "file a local ticket and print its id",
    ),
    row(
        "ticket",
        "list [--all] | ticket show <id> | ticket close <id> [--not-planned] [--duplicate]",
        "read a local ticket, or close it",
    ),
    row(
        "ticket",
        "edit <id> [--title <text>] [--body <text>] [--label <label>]... [--unlabel <label>]... [--parent <id>] [--no-parent] [--blocked-by <id>]... [--unblock <id>]...",
        "change a local ticket",
    ),
    row(
        "watch",
        "[<issue> | pm] [--back <n>] [--full]",
        "follow the runner and the sessions live",
    ),
    row(
        "usage",
        "[<project>] [--since <date>]",
        "show units and dollars per merged pull request",
    ),
    row(
        "usage",
        "<project> --import <steps file>",
        "add a runner's logged calls to its ledger",
    ),
    row(
        "usage",
        "<project> --item <issue> [--json]",
        "show one work item's calls and transcripts",
    ),
    row(
        "usage",
        "<project> --context [--since <date>] [--json]",
        "show request sizes and cache shares per role",
    ),
    row(
        "usage",
        "<project> --reads [--since <date>] [--json]",
        "show what workers read before their first edit",
    ),
    row(
        "usage",
        "[<project>] --openmetrics",
        "print the ledger as OpenMetrics counters",
    ),
    row(
        "usage",
        "--forge [--since <date>]",
        "show GitHub calls per minute and used-up answers",
    ),
];

/// The verbs `main` runs itself, which the manifest declares beside [`TABLE`]
///
/// Their usage is `main`'s own, so none is drawn into [`USAGE`]. The verbs no
/// person types (`runner`, `guard`, `confine`, `mcp-connect`) are left out.
pub const ELSEWHERE: &[Row] = &[
    row("lease", "", "ask for, run under and read kelpie's leases"),
    row("doctor", "[<project>] [--test-alert]", "check the setup"),
    row("agents", "", "write and list the agent files"),
    row("settings", "migrate", "migrate the settings file"),
    row("upgrade", "", "install a new build and restart the runners"),
    row("github", "setup", "register kelpie's GitHub App"),
    row("version", "[--json]", "print the build's version"),
    row(
        "totp",
        "[--rotate | --unlock]",
        "set up or unlock the second factor",
    ),
    row("tools", "install", "install the tools workers run with"),
];

/// What follows the verbs in the help: the flags that go with any of them
const NOTES: &str = "
`--json` goes anywhere in the line of `add <issue>`, `finish`, `status`, `gate`,
`drop`, `timings`, `tell`, `drain`, `undrain` and `board`, and prints the runner's
JSON object in place of plain text.

`-p <project>` or `--project <project>` goes anywhere in the line, before a
ruling's answer. Without it, the project is the one whose repo holds this
folder, or whose GitHub repo is this clone's `origin`. Only `set` and `get` take a
glob of project names (`*`, `?`, `[...]`) in `-p`: quote it, or the shell expands
it against files.";

/// Each verb once, [`TABLE`]'s then [`ELSEWHERE`]'s, with the `about` of its first row
pub fn declared() -> Vec<(&'static str, &'static str)> {
    let mut verbs: Vec<(&str, &str)> = Vec::new();
    for row in TABLE.iter().chain(ELSEWHERE) {
        if !verbs.iter().any(|(name, _)| *name == row.name) {
            verbs.push((row.name, row.about));
        }
    }
    verbs
}

/// The help text, drawn from [`TABLE`] at build time
///
/// A write the buffer has no room for is counted and dropped, so the first
/// pass with an empty buffer measures what the second writes.
struct Page<const N: usize> {
    bytes: [u8; N],
    len: usize,
}

impl<const N: usize> Page<N> {
    const fn push(&mut self, text: &str) {
        let text = text.as_bytes();
        let mut at = 0;
        while at < text.len() {
            if self.len < N {
                self.bytes[self.len] = text[at];
            }
            self.len += 1;
            at += 1;
        }
    }
}

const fn build<const N: usize>() -> Page<N> {
    let mut page = Page {
        bytes: [b' '; N],
        len: 0,
    };
    let mut at = 0;
    while at < TABLE.len() {
        let row = &TABLE[at];
        let start = page.len;
        page.push(if at == 0 { "usage: " } else { "       " });
        page.push("shep kelpie ");
        page.push(row.name);
        if !row.usage.is_empty() {
            page.push(" ");
            page.push(row.usage);
        }
        let width = page.len - start;
        if width + 2 > ABOUT_COLUMN {
            page.push("\n");
            let mut spaces = 0;
            while spaces < ABOUT_COLUMN {
                page.push(" ");
                spaces += 1;
            }
        } else {
            let mut spaces = width;
            while spaces < ABOUT_COLUMN {
                page.push(" ");
                spaces += 1;
            }
        }
        page.push(row.about);
        page.push("\n");
        at += 1;
    }
    page.push(NOTES);
    page
}

const MEASURED: usize = build::<0>().len;
const PAGE: Page<MEASURED> = build::<MEASURED>();

/// What the verbs take, as their help says, one line for each row of [`TABLE`]
pub const USAGE: &str = match core::str::from_utf8(&PAGE.bytes) {
    Ok(text) => text,
    Err(_) => panic!("the help is not text"),
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_about_fits_sixty_columns() {
        let long: Vec<_> = TABLE
            .iter()
            .chain(ELSEWHERE)
            .filter(|row| row.about.chars().count() > 60)
            .map(|row| (row.name, row.about))
            .collect();
        assert!(long.is_empty(), "past 60 columns: {long:?}");
    }

    #[test]
    fn usage_lists_every_verb_in_the_table() {
        for row in TABLE {
            let form = format!("shep kelpie {} {}", row.name, row.usage);
            assert!(
                USAGE.contains(form.trim_end()),
                "`{}` is not in the usage",
                form.trim_end()
            );
            assert!(
                USAGE.contains(row.about),
                "`{}` is not in the usage",
                row.about
            );
        }
    }

    #[test]
    fn the_table_holds_every_verb_the_command_line_runs() {
        let named: Vec<_> = TABLE.iter().map(|row| row.name).collect();
        let missing: Vec<_> = crate::flock::VERBS
            .into_iter()
            .filter(|verb| !named.contains(verb))
            .collect();
        assert!(missing.is_empty(), "not in the table: {missing:?}");
    }

    // Every `command == "<verb>"` arm of `main`'s dispatch, bar the one no
    // person types, is a verb the manifest must declare.
    #[test]
    fn declared_names_every_verb_main_runs() {
        let main = include_str!("../../main.rs");
        let declared: Vec<_> = declared().into_iter().map(|(name, _)| name).collect();
        let mut missing = Vec::new();
        for arm in main.split("command == \"").skip(1) {
            let verb = arm.split('"').next().unwrap_or_default();
            if verb != "mcp-connect" && !declared.contains(&verb) {
                missing.push(verb);
            }
        }
        assert!(
            missing.is_empty(),
            "main runs, the manifest lacks: {missing:?}"
        );
    }

    #[test]
    fn declared_names_each_verb_once() {
        let names: Vec<_> = declared().into_iter().map(|(name, _)| name).collect();
        let mut once = names.clone();
        once.sort_unstable();
        once.dedup();
        assert_eq!(names.len(), once.len(), "{names:?}");
        assert_eq!(names.first(), Some(&"add"));
    }
}
