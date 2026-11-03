//! `shep kelpie settings migrate`: writes the keys each project's table is missing
//!
//! `rulings.pm_alone` becomes `rulings.pm_decides`, each kind the map lacks written
//! at its default. The keys the agent board replaced ([`GONE_KEYS`]), the gone roles
//! ([`GONE_ROLES`]) and the `unlabelled` kind are deleted, each named with its value.
//! `agents.reviewers` goes too: its names join a listed `agents.allow`, and
//! `contributions.reviewers` where the project names none. An agent kelpie once
//! named for a project that left it unset gets that name ([`DEFAULTS`]). Each table
//! is checked as `set` checks it.

use std::process::ExitCode;

use serde_json::{Map, Value};

use super::{Homes, Place, checked, lookup, write};
use crate::home::{ProjectName, ProjectPaths};
use crate::settings::{PM_ALONE, PM_DECIDES, RulingName};
use crate::shepherd::{self, Connection, Socket};
use crate::webhook::KelpieSettings;

/// The keys migrate deletes: the loader refuses the first six by name and
/// ignores the rest, which doctor names
const GONE_KEYS: [&str; 9] = [
    "rulings.decide",
    "rulings.full_panel_paths",
    "agents.advisor",
    "agents.researcher",
    "researcher",
    "agents.issue_writer",
    "agents.implementers",
    "agents.implementer_choice",
    "agents.fallback_after",
];

/// The key whose names join a listed `agents.allow` before it is deleted
const REVIEWERS: &str = "agents.reviewers";

/// Where they join them
const ALLOW: &str = "agents.allow";

/// Where they go when the project names no reviewers of its own for a contribution
const CONTRIBUTION_REVIEWERS: &str = "contributions.reviewers";

/// The list of roles that may still name a role that is gone
const ROLES: &str = "repo_setup.roles";

/// The roles migrate deletes from `repo_setup.roles`, which refuses each by name
const GONE_ROLES: [&str; 3] = ["advisor", "researcher", "issue-writer"];

/// The kind of ruling migrate deletes from `rulings.pm_decides` and the old list
const WRITER_KIND: &str = "unlabelled";

/// The agents kelpie named when a project left the setting unset, which no code
/// names now: migrate writes each into a table that lacks it, so the project keeps
/// running on the agent it ran on
const DEFAULTS: [(&str, &str); 3] = [
    ("agents.representative", "representative"),
    ("triage.agent", "triager"),
    ("contributions.security_reviewer", "security-reviewer"),
];

/// What a contribution's pass ran when `contributions.reviewers` was unset: the
/// first where kelpie's settings held a gateway named [`PADDOCK`], then the second
const DEFAULT_REVIEWERS: [&str; 2] = ["iq3-xxs-review", "defect-hunter-lite"];

/// The gateway the first of [`DEFAULT_REVIEWERS`] ran on
const PADDOCK: &str = "paddock";

/// Runs `shep kelpie settings migrate`, printing a line for each project it changed
pub fn main() -> ExitCode {
    let ran = crate::shep_home::required(crate::shep_home::FLOCK_FIX).and_then(|shep_home| {
        let home = crate::home::user_home().ok_or(crate::home::HOME_UNSET)?;
        let kelpie_home = crate::home::kelpie_home_of(&shep_home);
        let homes = Homes {
            home: &home,
            kelpie_home: &kelpie_home,
            shep_home: &shep_home,
        };
        shepherd::block_on(async {
            let client = shepherd::connect(&Socket, &shep_home)
                .await
                .map_err(|e| e.describe(&shep_home))?;
            migrate(&client, homes).await
        })
    });
    match ran {
        Ok(lines) => {
            lines.iter().for_each(|line| println!("{line}"));
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("shep kelpie settings migrate: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Writes the keys every project's table is missing, a line for each project changed
///
/// `nothing to migrate` when no table misses one.
///
/// # Errors
///
/// A message when the shepherd cannot be read, or when a project's changed
/// table is refused or not written: it names each such project and why,
/// after the lines of the projects that were written.
pub async fn migrate(client: &dyn Connection, homes: Homes<'_>) -> Result<Vec<String>, String> {
    let tables = super::super::tables(client).await?;
    let mut said = Vec::new();
    let mut refused = Vec::new();
    for sheep in tables.keys() {
        let Ok(name) = ProjectName::try_from(sheep.as_str()) else {
            continue;
        };
        let paths = ProjectPaths::under(homes.kelpie_home, homes.shep_home, &name);
        let place = Place {
            home: homes.home,
            paths: &paths,
        };
        match project(client, &name, place).await {
            Ok(Some(line)) => said.push(line),
            Ok(None) => {}
            Err(why) => refused.push(format!("project {name}: not migrated: {why}")),
        }
    }
    if !refused.is_empty() {
        return Err(said
            .into_iter()
            .chain(refused)
            .collect::<Vec<_>>()
            .join("\n"));
    }
    if said.is_empty() {
        said.push("nothing to migrate".to_owned());
    }
    Ok(said)
}

// Migrates one project's table, and says what it wrote, if anything.
async fn project(
    client: &dyn Connection,
    name: &ProjectName,
    place: Place<'_>,
) -> Result<Option<String>, String> {
    let read = shepherd::read_tables_with(client, name.as_str()).await?;
    let Some(mut table) = read.project.clone() else {
        return Ok(None);
    };
    let listed = lookup(&table, PM_ALONE).is_some();
    let gone_kinds = writer_kind(&mut table);
    let mut wrote = pm_decides(&mut table);
    let (joined, gone_reviewers) = reviewers(&mut table);
    wrote.extend(joined);
    wrote.extend(defaults(&mut table, &read.kelpie));
    let mut removed: Vec<String> = (listed && lookup(&table, PM_ALONE).is_none())
        .then(|| PM_ALONE.to_owned())
        .into_iter()
        .collect();
    removed.extend(gone_keys(&mut table));
    removed.extend(gone_reviewers);
    removed.extend(gone_roles(&mut table));
    removed.extend(gone_kinds);
    let said: Vec<String> = [("wrote", wrote.join(", ")), ("removed", removed.join(", "))]
        .into_iter()
        .filter(|(_, keys)| !keys.is_empty())
        .map(|(did, keys)| format!("{did} {keys}"))
        .collect();
    if said.is_empty() {
        return Ok(None);
    }
    let (request, _) = checked(client, name, place, read, table).await?;
    write(client, &request).await?;
    Ok(Some(format!("project {name}: {}", said.join("; "))))
}

// Deletes the keys that are gone from `table`, and names each it deleted with
// the value it held, so a rollback can write it back.
fn gone_keys(table: &mut Map<String, Value>) -> Vec<String> {
    let mut removed = Vec::new();
    for setting in GONE_KEYS {
        let held = match setting.split_once('.') {
            Some((outer, _)) => table.get_mut(outer).and_then(Value::as_object_mut),
            None => Some(&mut *table),
        };
        let key = leaves(setting).1;
        if let Some(value) = held.and_then(|held| held.remove(key)) {
            removed.push(format!("{setting} = {}", inline(&value)));
        }
    }
    removed
}

// A value as the TOML a `key = ` takes on one line, its JSON when TOML has no form for it.
fn inline(value: &Value) -> String {
    toml::Value::try_from(value).map_or_else(|_| value.to_string(), |value| value.to_string())
}

// Moves the names `agents.reviewers` lists into `agents.allow` when that is
// already a list, each name once, and into `contributions.reviewers` when the
// project names none there, so a contribution keeps the reviewers its project
// named. Deletes the key either way. Says what it changed.
fn reviewers(table: &mut Map<String, Value>) -> (Vec<String>, Option<String>) {
    let (outer, key) = leaves(REVIEWERS);
    let Some(agents) = table.get_mut(outer).and_then(Value::as_object_mut) else {
        return (Vec::new(), None);
    };
    let Some(Value::Array(held)) = agents.get(key) else {
        return (Vec::new(), None);
    };
    let named = each_once(held);
    let joined = agents
        .get_mut(leaves(ALLOW).1)
        .and_then(Value::as_array_mut)
        .filter(|allow| !allow.is_empty())
        .and_then(|allow| {
            let missing: Vec<Value> = (named.iter())
                .filter(|name| !allow.contains(name))
                .cloned()
                .collect();
            let before = allow.len();
            allow.extend(missing);
            (allow.len() > before)
                .then(|| format!("{ALLOW} = {}", inline(&Value::Array(allow.clone()))))
        });
    let removed = agents
        .remove(key)
        .map(|value| format!("{REVIEWERS} = {}", inline(&value)));
    let carried = carried(table, &named);
    (joined.into_iter().chain(carried).collect(), removed)
}

// Writes the agents kelpie named for a project that left them unset, each where
// it is absent, and says what it wrote. A value already there is kept, and so
// is a section that is no table at all: the check refuses that one. The
// reviewers come after `reviewers`, which has carried the project's own.
fn defaults(table: &mut Map<String, Value>, kelpie: &str) -> Vec<String> {
    let paddock = holds_paddock(kelpie);
    let reviewers = DEFAULT_REVIEWERS
        .iter()
        .skip(usize::from(!paddock))
        .map(|name| Value::String((*name).to_owned()))
        .collect();
    let writes = DEFAULTS
        .iter()
        .map(|(setting, name)| (*setting, Value::String((*name).to_owned())))
        .chain([(CONTRIBUTION_REVIEWERS, Value::Array(reviewers))]);
    let mut wrote = Vec::new();
    for (setting, value) in writes {
        let (outer, key) = leaves(setting);
        let section = table
            .entry(outer)
            .or_insert_with(|| Value::Object(Map::new()));
        let Some(section) = section.as_object_mut() else {
            continue;
        };
        if !section.contains_key(key) {
            wrote.push(format!("{setting} = {}", inline(&value)));
            section.insert(key.to_owned(), value);
        }
    }
    wrote
}

// Whether kelpie's `[kelpie]` section holds a gateway named `paddock`. A section
// that does not read holds none: the settings check says why it does not.
fn holds_paddock(kelpie: &str) -> bool {
    if kelpie.trim().is_empty() {
        return false;
    }
    KelpieSettings::from_section(kelpie)
        .is_ok_and(|kelpie| kelpie.gateways().iter().any(|(n, _)| n.as_str() == PADDOCK))
}

// `names` with each name once, in the order the list held them
fn each_once(names: &[Value]) -> Vec<Value> {
    let mut seen: Vec<Value> = Vec::new();
    for name in names {
        if !seen.contains(name) {
            seen.push(name.clone());
        }
    }
    seen
}

// Writes `named` where a contribution's pass reads its reviewers, when the
// project names none of its own. A list already there is left as it is, and so
// is a `contributions` that is no table at all: the check refuses that one.
fn carried(table: &mut Map<String, Value>, named: &[Value]) -> Option<String> {
    let (outer, key) = leaves(CONTRIBUTION_REVIEWERS);
    let contributions = table
        .entry(outer)
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()?;
    if contributions.contains_key(key) {
        return None;
    }
    let list = Value::Array(named.to_vec());
    contributions.insert(key.to_owned(), list.clone());
    Some(format!("{CONTRIBUTION_REVIEWERS} = {}", inline(&list)))
}

// Deletes the roles that are gone from `repo_setup.roles`, and names each it deleted.
fn gone_roles(table: &mut Map<String, Value>) -> Vec<String> {
    let (outer, key) = leaves(ROLES);
    let roles = table.get_mut(outer).and_then(|setup| setup.get_mut(key));
    let Some(Value::Array(roles)) = roles else {
        return Vec::new();
    };
    let gone = |role: &Value| role.as_str().is_some_and(|role| GONE_ROLES.contains(&role));
    let removed = (roles.iter().filter_map(Value::as_str))
        .filter(|role| GONE_ROLES.contains(role))
        .map(|role| format!("{role} from {ROLES}"))
        .collect();
    roles.retain(|role| !gone(role));
    removed
}

// Deletes `unlabelled` from `rulings.pm_decides` and the old list, and names each it deleted.
fn writer_kind(table: &mut Map<String, Value>) -> Vec<String> {
    let ((outer, map_key), (_, list_key)) = (leaves(PM_DECIDES), leaves(PM_ALONE));
    let Some(rulings) = table.get_mut(outer).and_then(Value::as_object_mut) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    let map = rulings.get_mut(map_key).and_then(Value::as_object_mut);
    if map.and_then(|map| map.remove(WRITER_KIND)).is_some() {
        removed.push(format!("{PM_DECIDES}.{WRITER_KIND}"));
    }
    if let Some(Value::Array(list)) = rulings.get_mut(list_key) {
        let before = list.len();
        list.retain(|kind| kind.as_str() != Some(WRITER_KIND));
        if list.len() < before {
            removed.push(format!("{WRITER_KIND} from {PM_ALONE}"));
        }
    }
    removed
}

/// Writes into `table` the kinds `rulings.pm_decides` is missing, at their
/// defaults, after the old `rulings.pm_alone` list's kinds as `true`
///
/// Names each key it wrote. A key already there is left as it is. So is a
/// list beside a map, or one with an entry that is not a kind's name: the
/// check refuses either, naming it.
pub fn pm_decides(table: &mut Map<String, Value>) -> Vec<String> {
    let ((rulings, map_key), (_, list_key)) = (leaves(PM_DECIDES), leaves(PM_ALONE));
    let rulings = table
        .entry(rulings)
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(rulings) = rulings.as_object_mut() else {
        return Vec::new();
    };
    let names_only = |list: &Value| {
        list.as_array()
            .is_some_and(|list| list.iter().all(Value::is_string))
    };
    let moves = !rulings.contains_key(map_key) && rulings.get(list_key).is_some_and(names_only);
    let listed = match moves.then(|| rulings.remove(list_key)).flatten() {
        Some(Value::Array(list)) => list,
        _ => Vec::new(),
    };
    let map = rulings
        .entry(map_key)
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(map) = map.as_object_mut() else {
        return Vec::new();
    };
    let mut wrote = Vec::new();
    let listed = listed
        .iter()
        .filter_map(Value::as_str)
        .map(|kind| (kind, true));
    let defaults =
        (RulingName::ALL.into_iter()).filter_map(|kind| Some((kind.as_str(), kind.pm_default()?)));
    for (kind, value) in listed.chain(defaults) {
        if !map.contains_key(kind) {
            map.insert(kind.to_owned(), Value::Bool(value));
            wrote.push(format!("{PM_DECIDES}.{kind} = {value}"));
        }
    }
    wrote
}

// The table and the key in it that a dotted `<table>.<key>` setting names.
fn leaves(setting: &'static str) -> (&'static str, &'static str) {
    setting.split_once('.').unwrap_or(("", setting))
}
