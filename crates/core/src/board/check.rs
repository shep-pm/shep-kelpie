//! The issue check: whether an issue holds every part an implementer builds from
//!
//! It reads structure, not quality: each part is there and well formed. The
//! parts are what an issue body holds, as the triager writes one, the
//! blockers and harness reach `CLAUDE.md` asks for, and the labels the board
//! and the triager set. The issue skill, `skills/kelpie/issue/SKILL.md`, gives
//! the same format to the agent that writes an issue, and a test holds the
//! two together.

use std::fmt;

use super::is_priority_label;
use crate::guard::{CONVENTIONAL_TYPES, conventional_type};

/// The line naming the files an issue changes, each path in backticks
pub const TOUCHES: &str = "Touches:";

/// The heading of the section that says what to build
pub const WHAT_TO_BUILD: &str = "What to build";

/// The heading of the section of `- [ ]` items a pull request is checked against
pub const CRITERIA: &str = "Acceptance criteria";

/// The line saying which harnesses a change to one harness's adapter reaches
pub const HARNESS_REACH: &str = "Harness reach:";

/// The line naming the issues that must merge first
pub const BLOCKED_BY: &str = "Blocked by:";

/// What a kind label starts with, as `kind:feat`
pub const KIND: &str = "kind:";

/// The conventional types no issue's kind is: #1379 keeps nine of the gh
/// guard's eleven
const NOT_KINDS: [&str; 2] = ["build", "revert"];

/// The kinds an issue may have, #1379's nine conventional types
pub fn kinds() -> impl Iterator<Item = &'static str> {
    CONVENTIONAL_TYPES
        .into_iter()
        .filter(|t| !NOT_KINDS.contains(t))
}

/// The kind `title` starts with, as `feat` for `feat(scope)!: ...`
pub fn title_kind(title: &str) -> Option<&'static str> {
    let typed = conventional_type(title)?;
    kinds().find(|k| *k == typed)
}

/// The harnesses whose adapters are named for them, as in `adapters/pi/`
pub const HARNESSES: [&str; 4] = ["claude", "pi", "codex", "goose"];

/// A part of the format an issue lacks or holds malformed
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The title is empty
    NoTitle,
    /// No line starts `Touches:`, and no heading is `Touches`
    NoTouches,
    /// The `Touches:` line, the list under it or the `Touches` section names
    /// no path in backticks
    TouchesNoPath,
    /// `Touches:` names this path in one harness's adapter, and no
    /// `Harness reach:` line with text follows
    NoHarnessReach(String),
    /// No `What to build` heading, or nothing under it
    NoWhatToBuild,
    /// No `Acceptance criteria` heading
    NoCriteria,
    /// The `Acceptance criteria` section holds no `- [ ]` item
    NoCriteriaItems,
    /// A `Blocked by:` line names no issue
    BlockedByNoIssue,
    /// This acceptance criterion asks a worker to change another issue
    ChangesIssue(String),
    /// No priority label
    NoPriority,
    /// More than one priority label, these
    Priorities(Vec<String>),
    /// No `kind:` label, and no kind starting the title
    NoKind,
    /// This `kind:` label names no kind
    UnknownKind(String),
    /// More than one `kind:` label, these
    Kinds(Vec<String>),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoTitle => f.write_str("the title is empty"),
            Self::NoTouches => write!(
                f,
                "no `{TOUCHES}` line or `## Touches` section naming the files it changes"
            ),
            Self::TouchesNoPath => write!(f, "`{TOUCHES}` names no path in backticks"),
            Self::NoHarnessReach(path) => write!(
                f,
                "`{TOUCHES}` names `{path}`, one harness's adapter, and no `{HARNESS_REACH}` \
                 line says which harnesses the change reaches"
            ),
            Self::NoWhatToBuild => {
                write!(f, "no `## {WHAT_TO_BUILD}` section, or nothing under it")
            }
            Self::NoCriteria => write!(f, "no `## {CRITERIA}` section"),
            Self::NoCriteriaItems => write!(f, "the `## {CRITERIA}` section has no `- [ ]` item"),
            Self::BlockedByNoIssue => write!(f, "the `{BLOCKED_BY}` line names no issue as `#<n>`"),
            Self::ChangesIssue(item) => write!(
                f,
                "the criterion `{item}` asks a worker to change another issue, and a worker's \
                 credential cannot change issues: put it to the maintainer as a note, or take \
                 it out of the issue"
            ),
            Self::NoPriority => f.write_str("no priority label, `priority: P0` to `priority: P4`"),
            Self::Priorities(labels) => {
                write!(f, "priority labels {}: keep one", ticked(labels, " and "))
            }
            Self::NoKind => write!(
                f,
                "no kind: no `{KIND}<type>` label, and the title does not start with a \
                 kind such as `feat:` or `fix(board):`"
            ),
            Self::UnknownKind(label) => {
                write!(f, "`{label}` names no kind, as `{KIND}feat` does")
            }
            Self::Kinds(labels) => write!(f, "kind labels {}: keep one", ticked(labels, " and ")),
        }
    }
}

fn ticked<S: AsRef<str>>(labels: &[S], joint: &str) -> String {
    let ticked: Vec<String> = labels.iter().map(|l| format!("`{}`", l.as_ref())).collect();
    ticked.join(joint)
}

/// Every part of the format the issue of `title`, `body` and `labels` lacks
/// or holds malformed, none when it passes
pub fn check(title: &str, body: &str, labels: &[String]) -> Vec<Failure> {
    let mut failures = Vec::new();
    if title.trim().is_empty() {
        failures.push(Failure::NoTitle);
    }
    match touched(body) {
        None => failures.push(Failure::NoTouches),
        Some(paths) => {
            if paths.is_empty() {
                failures.push(Failure::TouchesNoPath);
            }
            let reach = labelled_line(body, HARNESS_REACH).is_some_and(|r| !bare(r).is_empty());
            let adapter = paths.into_iter().find(|p| is_adapter(p));
            if let Some(path) = adapter.filter(|_| !reach) {
                failures.push(Failure::NoHarnessReach(path.to_owned()));
            }
        }
    }
    let built = section(body, WHAT_TO_BUILD);
    if !built.is_some_and(|lines| lines.iter().any(|l| !l.trim().is_empty())) {
        failures.push(Failure::NoWhatToBuild);
    }
    match section(body, CRITERIA) {
        None => failures.push(Failure::NoCriteria),
        Some(lines) if !lines.iter().any(|l| is_item(l)) => {
            failures.push(Failure::NoCriteriaItems);
        }
        Some(lines) => {
            let asked = criteria(&lines).into_iter();
            let asked = asked.filter(|(_, whole)| changes_an_issue(whole));
            failures.extend(asked.map(|(first, _)| Failure::ChangesIssue(first.to_owned())));
        }
    }
    if labelled_line(body, BLOCKED_BY).is_some_and(|line| !names_an_issue(line)) {
        failures.push(Failure::BlockedByNoIssue);
    }
    let priorities: Vec<String> = (labels.iter())
        .filter(|l| is_priority_label(l))
        .cloned()
        .collect();
    match priorities.len() {
        0 => failures.push(Failure::NoPriority),
        1 => {}
        _ => failures.push(Failure::Priorities(priorities)),
    }
    let kind_labels: Vec<String> = (labels.iter())
        .filter(|l| kind_of(l).is_some())
        .cloned()
        .collect();
    match kind_labels.as_slice() {
        [] if title_kind(title).is_none() => {
            failures.push(Failure::NoKind);
        }
        [] => {}
        [label] => {
            let kind = kind_of(label).unwrap_or_default();
            if !kinds().any(|k| k.eq_ignore_ascii_case(kind)) {
                failures.push(Failure::UnknownKind(label.clone()));
            }
        }
        _ => failures.push(Failure::Kinds(kind_labels)),
    }
    failures
}

/// The version of the check's rules, which [`key`] takes in
///
/// Raise it with any change to what [`check`] refuses: every issue is then
/// checked once more, and each that now fails is owed a new triage pass.
pub const VERSION: u32 = 2;

/// A digest of what [`check`] reads of an issue, which an edit changes
///
/// Its title, its body, its priority and kind labels in any order, and the
/// check's [`VERSION`]. A label the check does not read, such as a triage
/// label, changes nothing.
pub fn key(title: &str, body: &str, labels: &[String]) -> u64 {
    let mut read: Vec<String> = (labels.iter())
        .filter(|l| is_priority_label(l) || kind_of(l).is_some())
        .map(|l| l.trim().to_ascii_lowercase())
        .collect();
    read.sort();
    read.dedup();
    let text = format!(
        "v{VERSION}\0{}\0{}\0{}",
        title.trim(),
        body.trim(),
        read.join("\0")
    );
    fnv1a(text.as_bytes())
}

/// FNV-1a over `bytes`
///
/// A saved digest is read again after a restart and on another machine, so
/// it is a fixed function and not the standard library's, which may change.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The paths in backticks the issue says it touches: on its `Touches:` line,
/// or the list under that line when it holds no text, or the section under a
/// `Touches` heading. `None` without either.
pub fn touched(body: &str) -> Option<Vec<&str>> {
    let Some((at, text)) = labelled(body, TOUCHES) else {
        let name = TOUCHES.trim_end_matches(':');
        return section(body, name).map(|lines| lines.into_iter().flat_map(backticked).collect());
    };
    if !bare(text).is_empty() {
        return Some(backticked(text));
    }
    let below = (outside_fences(body).skip_while(|&(i, _)| i <= at)).map(|(_, l)| l);
    let below = below.skip_while(|l| l.trim().is_empty());
    let list = below.take_while(|l| is_list_item(l));
    Some(list.flat_map(backticked).collect())
}

// Whether `line` is an item of a `-` or `*` list.
fn is_list_item(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("- ") || line.starts_with("* ")
}

/// The type a `kind:` label names, in any case and with any spaces, or `None`
/// for a label that is no kind label
pub fn kind_of(label: &str) -> Option<&str> {
    let (head, kind) = label.split_at_checked(KIND.len())?;
    head.eq_ignore_ascii_case(KIND).then(|| kind.trim())
}

/// The kind an issue's one `kind:` label names, as [`kinds`] spells it, or
/// `None` where it has no such label, two of them, or one naming no kind
pub fn label_kind(labels: &[String]) -> Option<&'static str> {
    let mut named = labels.iter().filter_map(|l| kind_of(l));
    let kind = named.next()?;
    if named.next().is_some() {
        return None;
    }
    kinds().find(|k| k.eq_ignore_ascii_case(kind))
}

// What follows `label` on the first line it starts, past list and bold marks.
fn labelled_line<'a>(body: &'a str, label: &str) -> Option<&'a str> {
    labelled(body, label).map(|(_, text)| text)
}

// The first line `label` starts, in any case and past list and bold marks,
// its colon inside the bold or after it: its index, and what follows the label.
fn labelled<'a>(body: &'a str, label: &str) -> Option<(usize, &'a str)> {
    let name = label.trim_end_matches(':');
    outside_fences(body).find_map(|(at, l)| {
        let l = l.trim_start_matches(|c: char| c.is_whitespace() || c == '-' || c == '*');
        let (head, rest) = l.split_at_checked(name.len())?;
        let rest = rest.strip_prefix("**").unwrap_or(rest).strip_prefix(':')?;
        head.eq_ignore_ascii_case(name).then_some((at, rest))
    })
}

// A line's text without the bold marks and spaces around it.
fn bare(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || c == '*')
}

// The spans in backticks. Backticks pair up, so every second part is inside a pair.
fn backticked(line: &str) -> Vec<&str> {
    let spans = line.split('`').skip(1).step_by(2);
    spans.filter(|s| !s.trim().is_empty()).collect()
}

// Whether `path` sits in one harness's adapter: a part after an `adapters`
// folder named for a harness, as `adapters/pi/` or `adapters/codex_usage.rs`.
fn is_adapter(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    parts.windows(2).any(|pair| {
        let stem = pair[1].split(['.', '_']).next().unwrap_or_default();
        pair[0] == "adapters" && HARNESSES.contains(&stem)
    })
}

// Whether `line` names an issue: `#` then a digit, or a local ticket's `k12`,
// `k` then digits alone.
fn names_an_issue(line: &str) -> bool {
    let number = |s: &str| s.starts_with(|c: char| c.is_ascii_digit());
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    line.split('#').skip(1).any(number)
        || (line.split(|c: char| !c.is_alphanumeric()))
            .any(|w| w.strip_prefix('k').is_some_and(digits))
}

// The checklist items among `lines`, each as its first line without the box
// and its whole text with the lines that wrap under it.
fn criteria<'a>(lines: &[&'a str]) -> Vec<(&'a str, String)> {
    let mut items: Vec<(&str, String)> = Vec::new();
    for line in lines {
        if is_item(line) {
            let first = line.trim_start()[2..].trim_start()[3..].trim();
            items.push((first, first.to_owned()));
        } else if let Some((_, whole)) = items.last_mut() {
            whole.push('\n');
            whole.push_str(line);
        }
    }
    items
}

// The verbs that change the issue they stand before, as "close #12".
const CHANGES: [&str; 9] = [
    "edit", "update", "comment", "label", "close", "reopen", "retitle", "assign", "unassign",
];

// The verbs that change an issue only when a preposition makes its name their
// target, as "add a row to #12" and not "add a test for #12".
const CHANGES_TO: [&str; 9] = [
    "add", "append", "tick", "untick", "uncheck", "check", "post", "remove", "delete",
];

// The prepositions that make an issue the target of a verb in `CHANGES_TO`.
const TARGET: [&str; 5] = ["to", "on", "in", "from", "at"];

// The words a verb may follow and still start what is asked of the worker.
const LEADS: [&str; 9] = [
    "to", "must", "should", "will", "can", "also", "then", "and", "or",
];

// A word of a clause, and whether it names an issue as `#12` or `owner/repo#12`.
type Word = (bool, String);

// Whether a criterion asks for an issue to be edited, updated, commented on,
// labelled, ticked, closed or reopened, one clause at a time. Words in
// backticks are code unless they name an issue, and a verb whose object is
// the pull request's own body changes no issue.
fn changes_an_issue(text: &str) -> bool {
    clauses(text).iter().any(|clause| {
        let words = clause_words(clause);
        (0..words.len()).any(|at| asks_to_change(&words, at))
    })
}

// Whether the verb at `at` leads a request, is one of the issue-changing
// verbs, and has an issue after it as its target.
fn asks_to_change(words: &[Word], at: usize) -> bool {
    let (is_issue, word) = &words[at];
    let leads = at == 0 || LEADS.contains(&words[at - 1].1.as_str());
    let rest = &words[at + 1..];
    let direct = CHANGES.iter().any(|stem| is_form(word, stem));
    let aimed = CHANGES_TO.iter().any(|stem| is_form(word, stem))
        && (word != "check" || rest.first().is_some_and(|(_, w)| w == "off"));
    if *is_issue || !leads || !(direct || aimed) {
        return false;
    }
    let object: Vec<&str> = (rest.iter())
        .map(|(_, w)| w.as_str())
        .take_while(|w| !["and", "then", "or"].contains(w))
        .collect();
    if names_own_body(&object) {
        return false;
    }
    (rest.iter().enumerate())
        .any(|(i, (is_issue, _))| *is_issue && (direct || aims_at(words, at + 1 + i)))
}

// Whether the word before the issue at `issue`, past "the" and "issue", is a
// preposition that makes it a target.
fn aims_at(words: &[Word], issue: usize) -> bool {
    let before = words[..issue].iter().rev();
    let mut before =
        before.skip_while(|(_, w)| ["the", "issue", "ticket", "tracker"].contains(&w.as_str()));
    before
        .next()
        .is_some_and(|(_, w)| TARGET.contains(&w.as_str()))
}

// Whether `words` hold "pull request" or "PR" with "body" or "description"
// within three words of it.
fn names_own_body(words: &[&str]) -> bool {
    words.iter().enumerate().any(|(i, w)| {
        let pull_request = *w == "request" && i > 0 && words[i - 1] == "pull";
        let near = &words[i.saturating_sub(3)..(i + 4).min(words.len())];
        (pull_request || *w == "pr") && near.iter().any(|w| ["body", "description"].contains(w))
    })
}

// Whether `word` is `stem` or one of its endings: `closes`, `closed`,
// `closing`, `labelled`.
fn is_form(word: &str, stem: &str) -> bool {
    let ending = word.strip_prefix(stem).map_or_else(
        || {
            let dropped = stem.strip_suffix('e')?;
            word.strip_prefix(dropped).filter(|r| *r == "ing")
        },
        Some,
    );
    ending.is_some_and(|e| ["", "s", "es", "d", "ed", "ing", "led", "ling"].contains(&e))
}

// The clauses of `text`, split at commas, colons, semicolons, brackets and
// sentence ends. Code in backticks that names no issue is one word, `code`.
fn clauses(text: &str) -> Vec<String> {
    let mut plain = String::new();
    for (i, part) in text.split('`').enumerate() {
        let names = part
            .match_indices('#')
            .any(|(at, _)| part[at + 1..].starts_with(|c: char| c.is_ascii_digit()));
        plain.push(' ');
        plain.push_str(if i % 2 == 0 || names { part } else { "code" });
        plain.push(' ');
    }
    let mut clauses = vec![String::new()];
    let mut chars = plain.chars().peekable();
    while let Some(c) = chars.next() {
        let ends = ",;:()".contains(c)
            || (".!?".contains(c) && chars.peek().is_none_or(|n| n.is_whitespace()));
        if ends {
            clauses.push(String::new());
        } else if let Some(last) = clauses.last_mut() {
            last.push(c);
        }
    }
    clauses
}

// The words of a clause in lower case, each flagged when it names an issue.
fn clause_words(clause: &str) -> Vec<Word> {
    let names = |t: &str| {
        t.match_indices('#')
            .any(|(at, _)| t[at + 1..].starts_with(|c: char| c.is_ascii_digit()))
    };
    (clause.split_whitespace())
        .map(|token| {
            let word = token
                .strip_suffix("'s")
                .or(token.strip_suffix("\u{2019}s"))
                .unwrap_or(token);
            let word = word.trim_matches(|c: char| !(c.is_alphanumeric() || c == '_' || c == '#'));
            (names(token), word.to_lowercase())
        })
        .filter(|(_, w)| !w.is_empty())
        .collect()
}

// Whether `line` is a checklist item, `- [ ]` or `- [x]`.
fn is_item(line: &str) -> bool {
    let line = line.trim_start();
    let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) else {
        return false;
    };
    ["[ ]", "[x]", "[X]"].iter().any(|b| rest.starts_with(b))
}

// The heading a line is, as its level and text, without closing `#`s or the
// bold around it.
fn heading(line: &str) -> Option<(usize, &str)> {
    let line = line.trim();
    let level = line.chars().take_while(|&c| c == '#').count();
    let text = line[level..].strip_prefix(' ')?.trim();
    let open = text.trim_end_matches('#');
    let closed = open.is_empty() || open.ends_with(' ');
    let text = if closed { open.trim() } else { text };
    let bold = ["**", "__"]
        .iter()
        .find_map(|m| text.strip_prefix(m)?.strip_suffix(m));
    (1..=6)
        .contains(&level)
        .then(|| (level, bold.unwrap_or(text).trim()))
}

// The lines under the heading `name`, at any level and in any case, up to the
// next heading of its level or above, fenced lines left out. `None` without
// that heading.
fn section<'a>(body: &'a str, name: &str) -> Option<Vec<&'a str>> {
    let mut lines = outside_fences(body).map(|(_, line)| (line, heading(line)));
    let (level, _) =
        lines.find_map(|(_, h)| h.filter(|(_, text)| text.eq_ignore_ascii_case(name)))?;
    let under = lines
        .take_while(|(_, h)| !h.is_some_and(|(l, _)| l <= level))
        .map(|(line, _)| line);
    Some(under.collect())
}

// The lines of `body` outside its code fences, each with its index. The lines
// that open and close a fence are left out too, and an unclosed one runs to
// the end.
fn outside_fences(body: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut open: Option<(char, usize)> = None;
    body.lines().enumerate().filter(move |(_, line)| {
        match (open, fence(line)) {
            (None, Some((mark, length, info))) if !(mark == '`' && info.contains('`')) => {
                open = Some((mark, length));
            }
            (Some((mark, length)), Some((closing, at_least, rest)))
                if closing == mark && at_least >= length && rest.trim().is_empty() =>
            {
                open = None;
            }
            (open, _) => return open.is_none(),
        }
        false
    })
}

// The mark a line starts a fence with, `` ` `` or `~`, its length and what
// follows it, for three or more of the mark behind at most three spaces.
fn fence(line: &str) -> Option<(char, usize, &str)> {
    let rest = line.trim_start_matches(' ');
    let mark = rest.chars().next().filter(|&c| c == '`' || c == '~')?;
    let length = rest.chars().take_while(|&c| c == mark).count();
    let fenced = line.len() - rest.len() <= 3 && length >= 3;
    fenced.then(|| (mark, length, &rest[length..]))
}
