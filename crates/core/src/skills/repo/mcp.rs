//! The MCP servers a repo's plugins and its `.mcp.json` bring
//!
//! Claude Code reads a plugin's servers from its `.mcp.json` or manifest, and
//! the repo's from its root `.mcp.json`. Kelpie runs most calls with
//! `--strict-mcp-config`, which drops them all, so it hands them to the adapter as `mcpServers`.
//!
//! A plugin's server may run a file of it through `${CLAUDE_PLUGIN_ROOT}`, so
//! the whole plugin is copied to a folder inside it that Claude Code does not
//! read, which the variable names. A skill's text gets the copy's own folder.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::Value;

use super::{inside, is_file};

/// Servers by name, each as Claude Code's `mcpServers` entry
pub type Servers = BTreeMap<String, Value>;

/// The folder inside a plugin's copy that holds the whole plugin, for a server that runs one of its files
///
/// The copy beside it leaves `.mcp.json` out of its root, and this one holds
/// the whole plugin, so a server runs a file of it from a folder Claude Code does not read.
pub(super) const ROOT: &str = "mcp-root";

/// The file at a plugin's root that lists its servers, which the copy keeps out of its own root
/// and holds in `ROOT` instead
pub(super) const CONFIG_FILE: &str = ".mcp.json";

const ROOT_VARIABLE: &str = "${CLAUDE_PLUGIN_ROOT}";

/// `text` with each `${CLAUDE_PLUGIN_ROOT}` in it the folder `root`
pub(super) fn fill_in(text: &str, root: &str) -> String {
    text.replace(ROOT_VARIABLE, root)
}

// The servers `value` lists: its `mcpServers`, or itself where a file lists them bare.
// An entry that is no object is no server.
fn entries(value: &Value) -> Servers {
    let listed = match value.get("mcpServers") {
        Some(listed) => listed,
        None => value,
    };
    (listed.as_object().into_iter())
        .flatten()
        .filter(|(_, entry)| entry.is_object())
        .map(|(name, entry)| (name.clone(), entry.clone()))
        .collect()
}

// The JSON of the file at `path`, if it is a file itself and no link to one.
fn read(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok().filter(|_| is_file(path))?;
    serde_json::from_str(&text).ok()
}

/// The servers of the plugin at `folder`, by the name its files give them
pub(super) fn plugin_servers(folder: &Path) -> Servers {
    let mut servers = read(&folder.join(CONFIG_FILE))
        .map(|file| entries(&file))
        .unwrap_or_default();
    let manifest =
        read(&folder.join(super::MANIFEST_FOLDER).join("plugin.json")).unwrap_or(Value::Null);
    let named = match &manifest["mcpServers"] {
        Value::String(path) => vec![path.as_str()],
        Value::Array(paths) => paths.iter().filter_map(Value::as_str).collect(),
        Value::Object(_) => {
            servers.extend(entries(&manifest["mcpServers"]));
            Vec::new()
        }
        _ => Vec::new(),
    };
    for path in named {
        let file = inside(folder, Path::new(path)).ok().and_then(|p| read(&p));
        servers.extend(file.iter().flat_map(entries));
    }
    servers
}

/// The servers of the `.mcp.json` whose text is `text`
pub(super) fn own_servers(text: &str) -> Servers {
    let file: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    (entries(&file).into_iter())
        .map(|(name, entry)| (tool_name(&name), entry))
        .collect()
}

/// `servers` of plugin `plugin`, named as Claude Code names a plugin's server, and with
/// `${CLAUDE_PLUGIN_ROOT}` in them the plugin's folder at `root`
pub(super) fn of_plugin(plugin: &str, servers: Servers, root: &Path) -> Servers {
    (servers.into_iter())
        .map(|(name, mut entry)| {
            substitute(&mut entry, &root.display().to_string());
            (tool_name(&format!("plugin_{plugin}_{name}")), entry)
        })
        .collect()
}

// A tool's name holds letters, digits, `_` and `-`, so a server's name does too.
fn tool_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn substitute(value: &mut Value, root: &str) {
    match value {
        Value::String(text) => *text = fill_in(text, root),
        Value::Array(items) => items.iter_mut().for_each(|item| substitute(item, root)),
        Value::Object(map) => map.values_mut().for_each(|item| substitute(item, root)),
        _ => {}
    }
}

/// The host a server's `url` names, where it has one that is written out
///
/// A `stdio` server names none, since what a command reaches cannot be told
/// from how it is started, and a host left to an environment variable is not
/// known until the call runs.
pub fn host_of(server: &Value) -> Option<String> {
    let url = server["url"].as_str()?;
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    let host = match authority.strip_prefix('[') {
        Some(inner) => inner.split_once(']')?.0,
        None => authority.split(':').next()?,
    };
    (!host.is_empty() && !host.contains('$')).then(|| host.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_file_lists_servers_under_mcp_servers_or_bare() {
        let wrapped = entries(&json!({ "mcpServers": { "a": { "command": "x" } } }));
        let bare = entries(&json!({ "a": { "command": "x" }, "b": "not a server" }));
        assert_eq!(wrapped, bare);
        assert_eq!(wrapped.keys().collect::<Vec<_>>(), ["a"]);
    }

    #[test]
    fn a_plugins_servers_come_from_its_file_and_its_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let at = dir.path();
        fs::create_dir_all(at.join(".claude-plugin")).unwrap();
        fs::write(
            at.join(".mcp.json"),
            r#"{"mcpServers":{"a":{"command":"a"}}}"#,
        )
        .unwrap();
        fs::write(at.join("more.json"), r#"{"b":{"command":"b"}}"#).unwrap();
        let manifest = json!({ "mcpServers": "./more.json" });
        fs::write(at.join(".claude-plugin/plugin.json"), manifest.to_string()).unwrap();
        assert_eq!(plugin_servers(at).keys().collect::<Vec<_>>(), ["a", "b"]);
        let inline = json!({ "mcpServers": { "c": { "command": "c" } } });
        fs::write(at.join(".claude-plugin/plugin.json"), inline.to_string()).unwrap();
        assert_eq!(plugin_servers(at).keys().collect::<Vec<_>>(), ["a", "c"]);
    }

    #[test]
    fn a_manifest_cannot_name_a_file_outside_the_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let at = dir.path().join("plugin");
        fs::create_dir_all(at.join(".claude-plugin")).unwrap();
        fs::write(dir.path().join("outside.json"), r#"{"a":{"command":"a"}}"#).unwrap();
        let manifest = json!({ "mcpServers": "../outside.json" });
        fs::write(at.join(".claude-plugin/plugin.json"), manifest.to_string()).unwrap();
        assert!(plugin_servers(&at).is_empty());
    }

    #[test]
    fn a_plugins_server_is_named_as_claude_code_names_it_and_runs_from_the_copy() {
        let servers = Servers::from([(
            "search.v2".to_owned(),
            json!({ "command": "${CLAUDE_PLUGIN_ROOT}/bin/s", "args": ["${CLAUDE_PLUGIN_ROOT}"] }),
        )]);
        let named = of_plugin("style", servers, Path::new("/k/repo/style/mcp-root"));
        assert_eq!(
            named["plugin_style_search_v2"],
            json!({ "command": "/k/repo/style/mcp-root/bin/s", "args": ["/k/repo/style/mcp-root"] })
        );
    }

    #[test]
    fn a_marketplaces_git_folder_does_not_change_the_plugins_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".mcp.json"), r#"{"a":{"command":"a"}}"#).unwrap();
        let found = super::super::Found {
            id: "p@m".to_owned(),
            name: "p".to_owned(),
            folder: dir.path().to_owned(),
            stale: None,
            parts: vec![crate::settings::Part::Mcp],
        };
        let before = super::super::fingerprint(&found);
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git/FETCH_HEAD"), "abc\n").unwrap();
        assert_eq!(super::super::fingerprint(&found), before);
        fs::write(dir.path().join(".mcp.json"), r#"{"b":{"command":"b"}}"#).unwrap();
        assert_ne!(super::super::fingerprint(&found), before);
    }

    #[test]
    fn the_host_of_a_server_is_its_urls_and_none_for_a_command() {
        let host = |url: &str| host_of(&json!({ "type": "http", "url": url }));
        assert_eq!(
            host("https://Docs.Example.com/mcp").as_deref(),
            Some("docs.example.com")
        );
        assert_eq!(
            host("http://user@h.example:8080/x?y").as_deref(),
            Some("h.example")
        );
        assert_eq!(
            host("https://[2001:db8::1]:9/mcp").as_deref(),
            Some("2001:db8::1")
        );
        assert_eq!(host("https://${HOST}/mcp"), None);
        assert_eq!(host_of(&json!({ "command": "npx" })), None);
    }
}
