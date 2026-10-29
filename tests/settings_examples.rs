//! The two example files document every setting kelpie accepts
//!
//! `settings.example.toml` holds the keys of a project's `[app.dogs.kelpie]`
//! table, and `kelpie-settings.example.toml` those of the `[kelpie]` section of
//! `dogs.toml`. The schema checked against is the one kelpie answers shep's
//! `--schema` probe with, which lookout edits from. A key is documented when it
//! appears, set or commented out, with a comment above it.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use shep_client::dogs::{SHEEP_SCHEMA_KEY, config_schema_with_sheep};
use shep_kelpie::settings::Settings;
use shep_kelpie::webhook::KelpieSettings;

const PROJECT_FILE: &str = "settings.example.toml";
const KELPIE_FILE: &str = "kelpie-settings.example.toml";

// Follows a `$ref` into the root's `$defs`, and an `Option`'s `anyOf`.
fn resolve<'a>(root: &'a Value, schema: &'a Value) -> &'a Value {
    if let Some(name) = schema["$ref"].as_str() {
        return resolve(root, &root["$defs"][name.trim_start_matches("#/$defs/")]);
    }
    if let Some(options) = schema["anyOf"].as_array()
        && let Some(some) = options.iter().find(|o| o["type"] != "null")
    {
        return resolve(root, some);
    }
    schema
}

// What a map of tables holds per entry, when it holds tables.
fn map_entry<'a>(root: &'a Value, node: &'a Value) -> Option<&'a Value> {
    let entry = &node["additionalProperties"];
    (entry.is_object() && resolve(root, entry)["properties"].is_object()).then_some(entry)
}

// What an array of tables holds per element, when it holds tables.
fn array_element<'a>(root: &'a Value, node: &'a Value) -> Option<&'a Value> {
    let element = &node["items"];
    (node["type"] == "array" && resolve(root, element)["properties"].is_object()).then_some(element)
}

// Every key the schema accepts, dotted. A map's entry name is `*`, an array of
// tables adds no segment, and a map of strings or a tagged enum is one key: its
// parts are written inline, in the value.
fn schema_keys(root: &Value, schema: &Value, prefix: &str, keys: &mut BTreeSet<String>) {
    let node = resolve(root, schema);
    if node["oneOf"].is_null() {
        if let Some(properties) = node["properties"].as_object() {
            for (key, property) in properties {
                schema_keys(root, property, &format!("{prefix}{key}."), keys);
            }
            return;
        }
        if let Some(entry) = map_entry(root, node) {
            return schema_keys(root, entry, &format!("{prefix}*."), keys);
        }
        if let Some(element) = array_element(root, node) {
            return schema_keys(root, element, prefix, keys);
        }
    }
    keys.insert(prefix.trim_end_matches('.').to_owned());
}

// The schema key a file's key path is, or `None` when the schema accepts no
// such key.
fn locate(root: &Value, schema: &Value, path: &[&str], prefix: &str) -> Option<String> {
    let node = resolve(root, schema);
    let Some((segment, rest)) = path.split_first() else {
        return Some(prefix.trim_end_matches('.').to_owned());
    };
    if node["oneOf"].is_null() {
        if let Some(properties) = node["properties"].as_object() {
            let property = properties.get(*segment)?;
            return locate(root, property, rest, &format!("{prefix}{segment}."));
        }
        if let Some(entry) = map_entry(root, node) {
            // `segment` was the entry's name, which `*` stands for.
            return locate(root, entry, rest, &format!("{prefix}*."));
        }
        if let Some(element) = array_element(root, node) {
            return locate(root, element, path, prefix);
        }
    }
    // A map of strings or a tagged enum: the rest of the path is inside its value.
    (node["oneOf"].is_array() || node["type"] == "object")
        .then(|| prefix.trim_end_matches('.').to_owned())
}

enum Line {
    Blank,
    Prose,
    Header(Vec<String>),
    Key(String),
    Other,
}

fn is_name(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

// A comment is `# ` and then a table header or a key, or it is prose. A
// deeper indent is prose, such as the agent files' frontmatter.
fn classify(line: &str) -> Line {
    let line = line.trim_end();
    if line.is_empty() {
        return Line::Blank;
    }
    let commented = line.starts_with('#');
    let body = if commented {
        match line[1..].strip_prefix(' ') {
            Some(body) if !body.starts_with(' ') => body,
            _ => return Line::Prose,
        }
    } else {
        line
    };
    if let Some(inner) = body.strip_prefix('[').and_then(|b| b.strip_suffix(']')) {
        let inner = inner
            .strip_prefix('[')
            .and_then(|b| b.strip_suffix(']'))
            .unwrap_or(inner);
        if inner.split('.').all(is_name) {
            return Line::Header(inner.split('.').map(str::to_owned).collect());
        }
    }
    match body.split_once(" = ") {
        Some((name, _)) if is_name(name) => Line::Key(name.to_owned()),
        _ if commented => Line::Prose,
        _ => Line::Other,
    }
}

struct Entry {
    line: usize,
    path: Vec<String>,
    commented_above: bool,
}

// A key has a comment above it when prose sits directly over it, or when it is
// in the run of keys that opens its table and prose sits directly over the
// table's header. A blank line, or a key further down the table, ends the walk.
fn commented_above(lines: &[Line], key: usize) -> bool {
    let mut past_header = false;
    for row in (0..key).rev() {
        match &lines[row] {
            Line::Prose => return past_header || row + 1 == key,
            Line::Header(_) if !past_header => past_header = true,
            Line::Key(_) | Line::Other if !past_header => {}
            _ => return false,
        }
    }
    false
}

// Every key line of a file, set or commented out, under the table header
// before it, or in the root table when none came.
fn entries(text: &str) -> Vec<Entry> {
    let lines: Vec<Line> = text.lines().map(classify).collect();
    let mut table: &[String] = &[];
    let mut found = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        match line {
            Line::Header(path) => table = path,
            Line::Key(name) => {
                let commented_above = commented_above(&lines, index);
                let mut path = table.to_vec();
                path.push(name.clone());
                found.push(Entry {
                    line: index + 1,
                    path,
                    commented_above,
                });
            }
            _ => {}
        }
    }
    found
}

// What is wrong between a file and the schema, one line each, naming the key
// and the file. `section` is the table the schema's keys sit in. The table of
// the Flockfile entry that holds the section is not part of the schema.
fn problems(file: &str, text: &str, root: &Value, schema: &Value, section: &[&str]) -> Vec<String> {
    let section: Vec<String> = section.iter().map(|s| (*s).to_owned()).collect();
    let mut problems = Vec::new();
    let mut documented: BTreeMap<String, bool> = BTreeMap::new();
    for entry in entries(text) {
        let table = &entry.path[..entry.path.len() - 1];
        if !table.is_empty() && section.starts_with(table) && table.len() < section.len() {
            continue;
        }
        let shown = entry.path.join(".");
        let Some(inside) = entry.path.strip_prefix(section.as_slice()) else {
            problems.push(format!(
                "{file}:{} holds `{shown}`, outside `{}`",
                entry.line,
                section.join(".")
            ));
            continue;
        };
        let inside: Vec<&str> = inside.iter().map(String::as_str).collect();
        match locate(root, schema, &inside, "") {
            Some(key) => {
                *documented.entry(key).or_default() |= entry.commented_above;
            }
            None => problems.push(format!(
                "{file}:{} holds `{}`, which the schema does not accept",
                entry.line,
                inside.join(".")
            )),
        }
    }
    let mut accepted = BTreeSet::new();
    schema_keys(root, schema, "", &mut accepted);
    for key in accepted.iter().filter(|k| !documented.contains_key(*k)) {
        problems.push(format!("{file} lacks `{key}`"));
    }
    for (key, _) in documented.iter().filter(|(_, commented)| !**commented) {
        problems.push(format!("{file} has no comment above `{key}`"));
    }
    problems
}

fn assert_no_problems(problems: &[String]) {
    assert!(problems.is_empty(), "\n{}\n", problems.join("\n"));
}

fn schema() -> Value {
    config_schema_with_sheep::<KelpieSettings, Settings>().to_value()
}

#[test]
fn every_project_setting_is_documented_in_the_settings_example() {
    let root = schema();
    assert_no_problems(&problems(
        PROJECT_FILE,
        include_str!("../settings.example.toml"),
        &root,
        &root[SHEEP_SCHEMA_KEY],
        &["app", "dogs", "kelpie"],
    ));
}

#[test]
fn every_kelpie_setting_is_documented_in_the_kelpie_settings_example() {
    let root = schema();
    assert_no_problems(&problems(
        KELPIE_FILE,
        include_str!("../kelpie-settings.example.toml"),
        &root,
        &root,
        &["kelpie"],
    ));
}

// A small schema with each shape the real one has: tables, a map of tables, an
// array of tables, a map of strings, a list and a tagged enum.
fn small_schema() -> Value {
    json!({
        "$defs": {
            "Gateway": {
                "type": "object",
                "properties": { "url": { "type": "string" } },
            },
            "Hook": {
                "type": "object",
                "properties": { "point": { "type": "string" } },
            },
            "Choice": {
                "oneOf": [
                    { "type": "object", "properties": { "kind": { "const": "none" } } },
                    { "type": "object", "properties": { "path": { "type": "string" } } },
                ],
            },
        },
        "type": "object",
        "properties": {
            "git": {
                "type": "object",
                "properties": {
                    "checkout": { "type": "string" },
                    "remote": { "anyOf": [{ "type": "string" }, { "type": "null" }] },
                },
            },
            "gateways": { "type": "object", "additionalProperties": { "$ref": "#/$defs/Gateway" } },
            "hooks": { "type": "array", "items": { "$ref": "#/$defs/Hook" } },
            "build_env": { "type": "object", "additionalProperties": { "type": "string" } },
            "roles": { "type": "array", "items": { "type": "string" } },
            "implement": { "anyOf": [{ "$ref": "#/$defs/Choice" }, { "type": "null" }] },
        },
    })
}

fn small_problems(text: &str) -> Vec<String> {
    let schema = small_schema();
    problems("small.toml", text, &schema, &schema, &["app"])
}

const SMALL_EXAMPLE: &str = r#"[app]
# Tool caches.
build_env = {}
# The roles.
# roles = ["worker"]
# A choice.
# implement = { kind = "none" }

# Where the project lives.
[app.git]
checkout = "~/shep"
# remote = "owner/name"

# A model gateway.
# [app.gateways.paddock]
# url = "http://gpu-box:8700"

# Its hooks.
# [[app.hooks]]
# point = "publish"
"#;

#[test]
fn a_file_that_holds_every_key_with_a_comment_has_no_problem() {
    assert_no_problems(&small_problems(SMALL_EXAMPLE));
}

#[test]
fn a_key_the_file_lacks_is_named() {
    let text = SMALL_EXAMPLE.replace("# [[app.hooks]]\n# point = \"publish\"\n", "");
    assert_eq!(small_problems(&text), ["small.toml lacks `hooks.point`"]);
}

#[test]
fn a_key_the_schema_does_not_accept_is_named_with_its_file_and_line() {
    let text = SMALL_EXAMPLE.replace("# remote = \"owner/name\"", "# origin = \"owner/name\"");
    assert_eq!(
        small_problems(&text),
        [
            "small.toml:12 holds `git.origin`, which the schema does not accept",
            "small.toml lacks `git.remote`",
        ]
    );
}

#[test]
fn a_key_with_no_comment_above_it_is_named() {
    let text = SMALL_EXAMPLE.replace("# Tool caches.\n", "");
    assert_eq!(
        small_problems(&text),
        ["small.toml has no comment above `build_env`"]
    );
}

#[test]
fn a_comment_over_a_table_covers_the_keys_that_follow_it() {
    let text = SMALL_EXAMPLE.replace("# Where the project lives.\n", "");
    assert_eq!(
        small_problems(&text),
        [
            "small.toml has no comment above `git.checkout`",
            "small.toml has no comment above `git.remote`",
        ]
    );
}

#[test]
fn a_table_outside_the_section_is_named() {
    let text = format!("{SMALL_EXAMPLE}\n# Elsewhere.\n[other]\nkey = 1\n");
    assert_eq!(
        small_problems(&text),
        ["small.toml:24 holds `other.key`, outside `app`"]
    );
}

#[test]
fn a_key_in_a_map_of_tables_is_located_under_the_entry_name() {
    let text = SMALL_EXAMPLE.replace("# url = ", "# uri = ");
    assert_eq!(
        small_problems(&text),
        [
            "small.toml:16 holds `gateways.paddock.uri`, which the schema does not accept",
            "small.toml lacks `gateways.*.url`",
        ]
    );
}

#[test]
fn a_key_before_any_table_is_outside_the_section() {
    let text = format!("stray = 1\n\n{SMALL_EXAMPLE}");
    assert_eq!(
        small_problems(&text),
        ["small.toml:1 holds `stray`, outside `app`"]
    );
}

#[test]
fn a_key_under_another_key_does_not_share_the_comment_over_that_key() {
    let text = SMALL_EXAMPLE.replace("# The roles.\n", "");
    assert_eq!(
        small_problems(&text),
        ["small.toml has no comment above `roles`"]
    );
}
