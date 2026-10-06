//! The issue writer's `gh` commands: filing, labelling, reading and linking issues

use super::ledger::Ledger;
use super::{IssueRules, ONLY, PLAIN, heredoc_body, is_heredoc, plain};
use crate::board::AGENT_LABEL;
use crate::guard::{Home, WRITE};
use crate::issues::has_criteria;

// The labels that say where an issue stands for kelpie. Only the one the
// rules name may go on.
const STATUS_LABELS: [&str; 4] = [
    crate::board::READY,
    crate::runner::HUMAN,
    crate::runner::IN_PROGRESS,
    crate::runner::SUMMON_LABEL,
];

// Why an edit or a link on an issue the session did not file is refused.
const NOT_FILED: &str = "the issue writer edits and links only the issues it filed in this \
    session";

/// What one `gh` command's words, after `gh`, may run, given what the
/// session filed and read so far
pub(super) struct Gh<'a> {
    pub(super) heredocs: &'a [String],
    pub(super) rules: &'a IssueRules,
    pub(super) home: &'a Home,
    pub(super) ledger: &'a Ledger,
}

impl Gh<'_> {
    pub(super) fn judge(&self, args: &[String]) -> Result<(), String> {
        let word = |at: usize| args.get(at).map(String::as_str);
        match (word(0), word(1)) {
            (Some("issue"), Some("create" | "new")) => {
                create(&args[2..], self.heredocs, self.rules, self.home)
            }
            (Some("issue"), Some("edit")) => edit(&args[2..], self.rules, self.ledger),
            (Some("issue"), Some("view")) => view(&args[2..]),
            (Some("issue"), Some("list")) => list(&args[2..]),
            (Some("api"), _) => api(&args[1..], self.ledger),
            _ => Err(format!("{ONLY}.")),
        }
    }
}

fn number(word: &str) -> Option<u64> {
    word.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| word.parse().ok())
        .flatten()
}

// A command's flags, each by its long name, and its other words.
#[derive(Debug, Default)]
struct Flags {
    values: Vec<(&'static str, String)>,
    bare: Vec<&'static str>,
    words: Vec<String>,
}

impl Flags {
    fn all(&self, long: &str) -> impl Iterator<Item = &str> {
        self.values
            .iter()
            .filter(move |(name, _)| *name == long)
            .map(|(_, v)| v.as_str())
    }
}

// `args` read against `known`, each flag's long name, its short one if any,
// and whether it takes a value. An unknown flag is refused by name.
fn flags(args: &[String], known: &[(&'static str, Option<&str>, bool)]) -> Result<Flags, String> {
    let mut flags = Flags::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if !arg.starts_with('-') || arg == "-" {
            flags.words.push(arg.clone());
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name, Some(value)),
            _ => (arg.as_str(), None),
        };
        let found = known
            .iter()
            .find(|(long, short, _)| *long == name || *short == Some(name));
        let Some(&(long, _, takes)) = found else {
            let shown: String = arg.chars().take(40).collect();
            return Err(format!(
                "the issue writer's gh does not take `{shown}`. {ONLY}."
            ));
        };
        match (takes, inline) {
            (true, Some(value)) => flags.values.push((long, value.to_owned())),
            (true, None) => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("`{name}` needs a value."))?;
                flags.values.push((long, value.clone()));
            }
            (false, _) => flags.bare.push(long),
        }
    }
    Ok(flags)
}

fn create(
    args: &[String],
    heredocs: &[String],
    rules: &IssueRules,
    home: &Home,
) -> Result<(), String> {
    let flags = flags(
        args,
        &[
            ("--title", Some("-t"), true),
            ("--body", Some("-b"), true),
            ("--body-file", Some("-F"), true),
            ("--label", Some("-l"), true),
        ],
    )?;
    // `<<` is the heredoc's own word.
    if flags
        .words
        .iter()
        .any(|w| !is_heredoc(std::slice::from_ref(w)))
    {
        return Err(format!("{ONLY}."));
    }
    let titles: Vec<&str> = flags.all("--title").collect();
    let [title] = titles.as_slice() else {
        return Err("give the issue one `--title`.".into());
    };
    if flags.all("--body").chain(flags.all("--body-file")).count() != 1 {
        return Err("give the issue one body, as `--body-file - <<'EOF'`.".into());
    }
    let body = match (flags.all("--body").next(), flags.all("--body-file").next()) {
        (Some(word), None) if heredoc_body(word) => heredocs.join("\n"),
        (Some(word), None) if plain(word) => word.to_owned(),
        (None, Some("-")) if !heredocs.is_empty() => heredocs.join("\n"),
        _ => return Err(format!("{PLAIN}.")),
    };
    if !plain(title) {
        return Err(format!("{PLAIN}."));
    }
    let labels = labels(flags.all("--label"))?;
    let agents: Vec<&str> = labels.iter().filter_map(|l| agent_of(l)).collect();
    let [agent] = agents.as_slice() else {
        return Err(format!(
            "an issue carries exactly one `{AGENT_LABEL}<name>` label, naming {}.",
            listed(rules)
        ));
    };
    named_agent(agent, rules)?;
    if !labels.iter().any(|l| l.eq_ignore_ascii_case(&rules.status)) {
        return Err(format!("file the issue with the label `{}`.", rules.status));
    }
    other_status(&labels, rules)?;
    if !has_criteria(&body) {
        return Err(
            "an issue's body has a `## Acceptance criteria` section, with a `- [ ]` \
                    line for each criterion."
                .into(),
        );
    }
    if let Some(leak) = home.find_in_prose([title.to_string(), body]) {
        return Err(home.refusal("this issue", leak, WRITE));
    }
    Ok(())
}

fn edit(args: &[String], rules: &IssueRules, ledger: &Ledger) -> Result<(), String> {
    let flags = flags(
        args,
        &[("--add-label", None, true), ("--remove-label", None, true)],
    )?;
    let numbers: Option<Vec<u64>> = flags.words.iter().map(|w| number(w)).collect();
    let numbers = numbers.filter(|n| !n.is_empty() && !flags.values.is_empty());
    let Some(numbers) = numbers else {
        return Err(
            "`gh issue edit` takes the issue's number and only `--add-label` or \
                    `--remove-label`."
                .into(),
        );
    };
    if let Some(n) = numbers.iter().find(|n| !ledger.filed(**n)) {
        return Err(format!("{NOT_FILED}, and #{n} is not one."));
    }
    let added = labels(flags.all("--add-label"))?;
    let agents: Vec<&str> = added.iter().filter_map(|l| agent_of(l)).collect();
    if agents.len() > 1 {
        return Err(format!(
            "an issue carries exactly one `{AGENT_LABEL}<name>` label."
        ));
    }
    for agent in agents {
        named_agent(agent, rules)?;
    }
    other_status(&added, rules)?;
    let removed = labels(flags.all("--remove-label"))?;
    if let Some(label) = removed.iter().find(|l| agent_of(l).is_none()) {
        return Err(format!(
            "the issue writer takes off only `{AGENT_LABEL}` labels, not `{label}`."
        ));
    }
    Ok(())
}

// What a read's flags and words may be: plain, and none naming another repo.
fn this_repo(flags: &Flags) -> Result<(), String> {
    let words = flags.values.iter().map(|(_, v)| v).chain(&flags.words);
    match words.into_iter().any(|w| !plain(w)) {
        true => Err(format!("{PLAIN}.")),
        false => Ok(()),
    }
}

// The flags a read takes, besides its own. `--repo`, `--web` and a URL are
// none of them, so an issue elsewhere is never read.
const READ_FLAGS: [(&str, Option<&str>, bool); 3] = [
    ("--json", None, true),
    ("--jq", Some("-q"), true),
    ("--template", Some("-t"), true),
];

// `gh issue view <number>`, on this repo.
fn view(args: &[String]) -> Result<(), String> {
    let mut known = READ_FLAGS.to_vec();
    known.push(("--comments", Some("-c"), false));
    let flags = flags(args, &known)?;
    this_repo(&flags)?;
    match flags.words.as_slice() {
        [n] if number(n).is_some() => Ok(()),
        _ => Err("`gh issue view` takes one issue of this repo, by its number.".into()),
    }
}

// `gh issue list`, on this repo.
fn list(args: &[String]) -> Result<(), String> {
    let mut known = READ_FLAGS.to_vec();
    known.extend([
        ("--search", Some("-S"), true),
        ("--state", Some("-s"), true),
        ("--label", Some("-l"), true),
        ("--limit", Some("-L"), true),
        ("--author", Some("-A"), true),
    ]);
    let flags = flags(args, &known)?;
    this_repo(&flags)?;
    match flags.words.is_empty() {
        true => Ok(()),
        false => Err("`gh issue list` takes only its flags, on this repo.".into()),
    }
}

// The issue's id, and its sub-issue and blocked-by links, on this repo, for
// the issues the session filed.
fn api(args: &[String], ledger: &Ledger) -> Result<(), String> {
    let flags = flags(
        args,
        &[
            ("--method", Some("-X"), true),
            ("--field", Some("-F"), true),
            ("--raw-field", Some("-f"), true),
            ("--jq", Some("-q"), true),
            ("--silent", None, false),
        ],
    )?;
    if flags.values.iter().any(|(_, v)| !plain(v)) {
        return Err(format!("{PLAIN}."));
    }
    let [endpoint] = flags.words.as_slice() else {
        return Err(format!("{ONLY}."));
    };
    let fields: Vec<&str> = flags
        .all("--field")
        .chain(flags.all("--raw-field"))
        .collect();
    let method = match flags.all("--method").next() {
        Some(method) => method.to_ascii_uppercase(),
        None if fields.is_empty() => "GET".to_owned(),
        None => "POST".to_owned(),
    };
    let rest = endpoint.trim_start_matches('/');
    let rest =
        (rest.strip_prefix("repos/{owner}/{repo}/issues/")).ok_or_else(|| format!("{ONLY}."))?;
    let digits = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let issue = number(&rest[..digits]).ok_or_else(|| format!("{ONLY}."))?;
    let link = match &rest[digits..] {
        "" => None,
        "/sub_issues" => Some("sub_issue_id"),
        "/dependencies/blocked_by" => Some("issue_id"),
        _ => return Err(format!("{ONLY}.")),
    };
    let id = |f: &str, key: &str| {
        f.strip_prefix(key)
            .and_then(|v| v.strip_prefix('='))
            .and_then(number)
    };
    let (key, field) = match (method.as_str(), link, fields.as_slice()) {
        ("GET", _, []) => return Ok(()),
        ("POST", Some(key), [f]) if id(f, key).is_some() => (key, id(f, key)),
        ("POST", Some(key), _) => {
            return Err(format!("a link takes one field, `{key}=<the issue's id>`."));
        }
        _ => return Err(format!("{ONLY}.")),
    };
    if !ledger.filed(issue) {
        return Err(format!("{NOT_FILED}, and #{issue} is not one."));
    }
    // A sub-issue is one the session filed; a blocker is any issue of this
    // repo whose id it read.
    let known = field.is_some_and(|id| match key {
        "sub_issue_id" => ledger.filed_id(id),
        _ => ledger.read_id(id),
    });
    match known {
        true => Ok(()),
        false => Err(format!(
            "{NOT_FILED}: `{key}` must be an id it read with \
             `gh api 'repos/{{owner}}/{{repo}}/issues/<number>' --jq .id`."
        )),
    }
}

// Each label named, split at commas, refused when one is not plain.
fn labels<'a>(values: impl Iterator<Item = &'a str>) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for value in values {
        if !plain(value) {
            return Err(format!("{PLAIN}."));
        }
        out.extend(
            value
                .split(',')
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_owned),
        );
    }
    Ok(out)
}

// The name an `agent:` label gives, its prefix in any case, as the board reads it.
fn agent_of(label: &str) -> Option<&str> {
    let prefix = label.get(..AGENT_LABEL.len())?;
    prefix
        .eq_ignore_ascii_case(AGENT_LABEL)
        .then(|| &label[AGENT_LABEL.len()..])
}

fn named_agent(agent: &str, rules: &IssueRules) -> Result<(), String> {
    match rules.agents.iter().any(|a| a == agent) {
        true => Ok(()),
        false => Err(format!(
            "`{AGENT_LABEL}{agent}` names no implementer this project lists: name {}.",
            listed(rules)
        )),
    }
}

fn other_status(labels: &[String], rules: &IssueRules) -> Result<(), String> {
    let other = labels.iter().find(|l| {
        !l.eq_ignore_ascii_case(&rules.status)
            && STATUS_LABELS.iter().any(|s| l.eq_ignore_ascii_case(s))
    });
    match other {
        Some(label) => Err(format!(
            "the issue writer files with `{}`, and puts no `{label}` on.",
            rules.status
        )),
        None => Ok(()),
    }
}

fn listed(rules: &IssueRules) -> String {
    let names: Vec<String> = rules.agents.iter().map(|a| format!("`{a}`")).collect();
    match names.as_slice() {
        [] => "an implementer the project lists".to_owned(),
        [one] => one.clone(),
        _ => format!("one of {}", names.join(", ")),
    }
}
