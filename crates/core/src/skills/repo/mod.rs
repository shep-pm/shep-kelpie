//! The plugins a repo turns on
//!
//! A repo's `.claude/settings.json` names a marketplace in
//! `extraKnownMarketplaces` and the plugin in `enabledPlugins`. A headless run
//! installs none, so kelpie reads that file from the default branch (never a
//! worker's) and copies each enabled plugin, less the parts `repo_setup.parts`
//! leaves out, under the project's skills folder, given to every call with `--plugin-dir`.
//!
//! The repo's own `.claude` files load as one more plugin (`own`). MCP servers
//! go to each call's MCP config ([`mcp`]). A plugin that cannot load is skipped.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Component, Path, PathBuf};

use serde::de::IgnoredAny;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use super::is_kelpies_own;
use crate::ports::{Git, GitError};
use crate::settings::{Part, RepoSetup, SkillName};

/// The folder under the skills folder that holds the repo's plugins, so none
/// is ever kelpie's own
const FOLDER: &str = "repo";

/// Where the marketplaces kelpie clones are kept, under kelpie's home
pub const MARKETPLACES: &str = "marketplaces";

const SETTINGS: &str = ".claude/settings.json";

/// The parts of a plugin that are one folder each, which kelpie copies whole
/// where `repo_setup.parts` loads them and leaves out where it does not
const FOLDERS: [Part; 4] = [Part::Skills, Part::Agents, Part::Commands, Part::Hooks];

/// The folder a plugin's manifest sits in, which the copy writes trimmed of its
/// own rather than copying
const MANIFEST_FOLDER: &str = ".claude-plugin";

/// The manifest keys a copied plugin keeps: what names and describes it. The
/// rest of a manifest points at parts outside the folders kelpie copies.
const MANIFEST_KEYS: [&str; 6] = [
    "name",
    "version",
    "description",
    "author",
    "license",
    "homepage",
];

/// One plugin the repo turns on, as `status` shows it
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoPlugin {
    /// The plugin as the repo's settings name it, `name@marketplace`
    pub plugin: String,
    /// The skills it brought, where it loaded
    pub skills: Vec<String>,
    /// Why it was left out, where it was
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    /// Why it is an older copy, where its marketplace could not be fetched
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale: Option<String>,
    /// Each of its `SKILL.md` files a strict harness refuses, named with the
    /// parser's message. The plugin loads, and so does every skill of it the
    /// harness can read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreadable: Vec<String>,
    #[serde(skip)]
    dir: Option<PathBuf>,
    // Where it is read from, until `install` has copied it to `dir`.
    #[serde(skip)]
    found: Option<Found>,
    // The repo's own `.claude` files, which are no plugin of a marketplace, in place of `found`.
    #[serde(skip)]
    own: Option<own::Own>,
    // What its files held when it was read, so a change to them alone is a change.
    #[serde(skip)]
    digest: u64,
    // The MCP servers it brings, by the name their tools are allowed under.
    #[serde(skip)]
    mcp: Servers,
}

/// The plugins the repo turns on, as the runner loaded them
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoPlugins {
    plugins: Vec<RepoPlugin>,
}

impl RepoPlugins {
    /// Loads the plugins the checkout's default branch turns on, writing each
    /// under `skills/repo/<name>`
    ///
    /// Nothing here fails: a plugin that cannot load is skipped.
    pub fn load(
        checkout: &Path,
        base: &str,
        kelpie_home: &Path,
        skills: &Path,
        git: &dyn Git,
        setup: &RepoSetup,
    ) -> Self {
        Self::plan(checkout, base, kelpie_home, skills, git, setup).install(skills)
    }

    /// What [`Self::load`] would load, with the marketplaces fetched and
    /// nothing written under `skills`
    ///
    /// A reread plans first, so a reread that fails later leaves the folders
    /// the runner is using as they were.
    pub fn plan(
        checkout: &Path,
        base: &str,
        kelpie_home: &Path,
        skills: &Path,
        git: &dyn Git,
        setup: &RepoSetup,
    ) -> Self {
        let into = skills.join(FOLDER);
        let mut plugins: Vec<RepoPlugin> = resolve(checkout, base, kelpie_home, git, true, setup)
            .into_iter()
            .map(|resolved| plan_one(resolved, &into, setup))
            .collect();
        plugins.extend(own::plan(checkout, base, &into, setup));
        Self { plugins }
    }

    /// Writes each planned plugin under `skills/repo/<name>`, and takes out
    /// what the repo no longer turns on, so no call keeps it
    #[must_use]
    pub fn install(self, skills: &Path) -> Self {
        let into = skills.join(FOLDER);
        let plugins: Vec<RepoPlugin> = (self.plugins.into_iter())
            .map(|mut plugin| {
                let failed = match (&plugin.found, &plugin.own, &plugin.dir) {
                    (Some(found), _, Some(dir)) => install(found, dir).err(),
                    (None, Some(files), Some(dir)) => own::install(files, dir).err(),
                    _ => None,
                };
                if let Some(reason) = failed {
                    plugin = RepoPlugin::skipped(plugin.plugin, reason);
                }
                plugin
            })
            .collect();
        let loaded: Vec<&PathBuf> = plugins.iter().filter_map(|p| p.dir.as_ref()).collect();
        if let Ok(entries) = fs::read_dir(&into) {
            for entry in entries.filter_map(Result::ok) {
                if !loaded.contains(&&entry.path()) {
                    let _ = fs::remove_dir_all(entry.path());
                }
            }
        }
        Self { plugins }
    }

    /// Every plugin folder that loaded, for `claude --plugin-dir`, the one
    /// that holds the repo's own `.claude` files left out where the call
    /// already loads them from a worktree that matches `main`
    pub fn dirs(&self, own_loaded: bool) -> impl Iterator<Item = &Path> {
        (self.plugins.iter())
            .filter(move |p| !(own_loaded && p.own.is_some()))
            .filter_map(|p| p.dir.as_deref())
    }

    /// The MCP servers of the plugins that loaded and of the repo's own `.mcp.json`, by the
    /// name their tools are allowed under
    ///
    /// The repo's own are not the worktree's `.mcp.json`, which a `--strict-mcp-config` call
    /// never reads, so a call gets them whether or not its worktree matches `main`.
    pub fn mcp_servers(&self) -> Servers {
        (self.plugins.iter())
            .filter(|p| p.skipped.is_none())
            .flat_map(|p| p.mcp.clone())
            .collect()
    }

    /// Each plugin the repo turns on, with its skills or why it was skipped
    pub fn status(&self) -> &[RepoPlugin] {
        &self.plugins
    }

    /// A log line for each plugin that was skipped or is an older copy, and
    /// for each of its skill files a strict harness refuses
    pub fn notices(&self) -> impl Iterator<Item = String> + '_ {
        self.plugins.iter().flat_map(|p| {
            let skipped = p
                .skipped
                .as_ref()
                .map(|reason| format!("the repo's plugin {} is skipped: {reason}", p.plugin));
            let stale = p.stale.as_ref().map(|reason| {
                format!(
                    "the repo's plugin {} is the copy from the last good fetch: {reason}",
                    p.plugin
                )
            });
            let unreadable = (p.unreadable.iter()).map(move |reason| {
                format!(
                    "the repo's plugin {} has a skill whose frontmatter is not valid YAML: {reason}",
                    p.plugin
                )
            });
            skipped.into_iter().chain(stale).chain(unreadable)
        })
    }
}

impl RepoPlugin {
    fn skipped(plugin: String, reason: String) -> Self {
        Self {
            plugin,
            skills: Vec::new(),
            skipped: Some(reason),
            stale: None,
            unreadable: Vec::new(),
            dir: None,
            found: None,
            own: None,
            digest: 0,
            mcp: Servers::new(),
        }
    }

    /// The MCP servers it brings, by the name their tools are allowed under
    pub fn mcp_servers(&self) -> &Servers {
        &self.mcp
    }
}

// A plugin that was found, read for the parts `setup` loads and aimed at its
// folder under `into`, or the reason it cannot load.
fn plan_one(resolved: Result<Found, Skipped>, into: &Path, setup: &RepoSetup) -> RepoPlugin {
    let mut found = match resolved {
        Ok(found) => found,
        Err(skipped) => return RepoPlugin::skipped(skipped.id, skipped.reason),
    };
    let skills = if setup.loads(Part::Skills) {
        skills_in(&found.folder)
    } else {
        Vec::new()
    };
    found.parts = FOLDERS
        .into_iter()
        .filter(|part| setup.loads(*part))
        .filter(|part| match part {
            Part::Skills => !skills.is_empty(),
            _ => is_folder(&found.folder.join(part.as_str())),
        })
        .collect();
    let dir = into.join(&found.name);
    // The plugin's files go whole beside its parts, for a server that runs one of them.
    let servers = if setup.loads(Part::Mcp) {
        mcp::plugin_servers(&found.folder)
    } else {
        Servers::new()
    };
    let mcp = mcp::of_plugin(&found.name, servers, &dir.join(mcp::ROOT));
    if !mcp.is_empty() {
        found.parts.push(Part::Mcp);
    }
    if found.parts.is_empty() {
        let asked: Vec<&str> = setup.parts.iter().map(|part| part.as_str()).collect();
        let reason = format!(
            "it has none of the parts repo_setup.parts loads ({})",
            asked.join(", ")
        );
        return RepoPlugin::skipped(found.id, reason);
    }
    let unreadable: Vec<String> = skills
        .iter()
        .filter_map(|skill| {
            let file = format!("{}/{skill}/SKILL.md", Part::Skills.as_str());
            let reason = frontmatter_error(&found.folder.join(&file))?;
            Some(format!("{file}: {reason}"))
        })
        .collect();
    RepoPlugin {
        plugin: found.id.clone(),
        skills,
        skipped: None,
        stale: found.stale.clone(),
        unreadable,
        dir: Some(dir),
        digest: fingerprint(&found),
        found: Some(found),
        own: None,
        mcp,
    }
}

/// A plugin the repo turns on whose folder was found
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Found {
    /// `name@marketplace`
    pub(crate) id: String,
    /// The plugin's name
    pub(crate) name: String,
    /// Its folder in the marketplace
    pub(crate) folder: PathBuf,
    /// Why the marketplace is the last good copy, where it could not be fetched
    pub(crate) stale: Option<String>,
    /// The parts that load: those `repo_setup.parts` names that the plugin has
    pub(crate) parts: Vec<Part>,
}

/// A plugin the repo turns on that cannot load
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Skipped {
    /// `name@marketplace`, or what was read in its place
    pub(crate) id: String,
    pub(crate) reason: String,
}

/// Finds the folder of each plugin the checkout's default branch turns on,
/// fetching the marketplaces that need it, in the order of the repo's list
/// and `origin`'s default branch first where `refresh` is set
///
/// What `setup` leaves out of the repo's plugins is not listed, and what it
/// adds comes after them.
pub(crate) fn resolve(
    checkout: &Path,
    base: &str,
    kelpie_home: &Path,
    git: &dyn Git,
    refresh: bool,
    setup: &RepoSetup,
) -> Vec<Result<Found, Skipped>> {
    let mut resolved = Vec::new();
    // The branch is read even where no plugin of the repo's loads, so the
    // checkout is as fresh for the plugins `setup` adds.
    let read = settings_on_default_branch(checkout, base, refresh);
    let (settings, mut enabled) = match read {
        Ok(Some(read)) if setup.plugins => read,
        Err(reason) if setup.plugins => {
            let id = SETTINGS.to_owned();
            resolved.push(Err(Skipped { id, reason }));
            (Value::Null, Vec::new())
        }
        _ => (Value::Null, Vec::new()),
    };
    enabled.retain(|(id, _)| !setup.skips(id));
    // A marketplace the project gives takes the place of the repo's of that name.
    let mut known = settings["extraKnownMarketplaces"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    let mut given: BTreeMap<&str, (&str, Value)> = BTreeMap::new();
    let mut disagreeing: Vec<(String, String)> = Vec::new();
    for (plugin, source) in &setup.extra {
        let (id, market, entry) = (plugin.as_str(), plugin.marketplace(), source.entry());
        match given.get(market) {
            Some((first, was)) if was != &entry => {
                disagreeing.push((id.to_owned(), (*first).to_owned()));
            }
            Some(_) => {}
            None => {
                known.insert(market.to_owned(), entry.clone());
                given.insert(market, (id, entry));
            }
        }
        if !enabled
            .iter()
            .any(|(on, flag)| on == id && flag == &Value::Bool(true))
        {
            enabled.push((id.to_owned(), Value::Bool(true)));
        }
    }
    let known = Some(&known);
    let mut markets: BTreeMap<String, Result<(PathBuf, Option<String>), String>> = BTreeMap::new();
    let mut supplied: BTreeMap<String, String> = BTreeMap::new();
    for (id, on) in &enabled {
        if on != &Value::Bool(true) {
            continue;
        }
        let skip = |reason: String| Skipped {
            id: id.clone(),
            reason,
        };
        if let Some((_, first)) = disagreeing.iter().find(|(extra, _)| extra == id) {
            let market = id.split_once('@').map_or("", |(_, market)| market);
            resolved.push(Err(skip(format!(
                "{first} already gives the {market} marketplace another source"
            ))));
            continue;
        }
        let Some((name, market)) = id.split_once('@') else {
            resolved.push(Err(skip("it is not written name@marketplace".into())));
            continue;
        };
        if !plain(name) || is_kelpies_own(name) {
            let what = if plain(name) {
                "which is kelpie's own"
            } else {
                "which is not a plugin's name"
            };
            resolved.push(Err(skip(format!("it is named {name}, {what}"))));
            continue;
        }
        let root = markets
            .entry(market.to_owned())
            .or_insert_with(|| marketplace(market, known, checkout, base, kelpie_home, git));
        let found = match root {
            Ok((root, stale)) => plugin_in(root, name).map(|folder| Found {
                id: id.clone(),
                name: name.to_owned(),
                folder,
                stale: stale.clone(),
                parts: Vec::new(),
            }),
            Err(reason) => Err(reason.clone()),
        };
        // Two plugins of one name would share a folder, the last overwriting the first.
        let found = found.and_then(|found| match supplied.get(name) {
            Some(first) => Err(format!("{first} already supplies a plugin named {name}")),
            None => {
                supplied.insert(name.to_owned(), id.clone());
                Ok(found)
            }
        });
        resolved.push(found.map_err(skip));
    }
    resolved
}

// What `.claude/settings.json` holds on the default branch, and its
// `enabledPlugins` in the order the file lists them.
type Read = (Value, Vec<(String, Value)>);

// The keys of a JSON object in the order they are written, which a
// `serde_json::Map` does not keep.
struct InOrder(Vec<(String, Value)>);

impl<'de> Deserialize<'de> for InOrder {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = InOrder;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<InOrder, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry::<String, Value>()? {
                    entries.push(entry);
                }
                Ok(InOrder(entries))
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}

#[derive(Deserialize)]
struct Listed {
    #[serde(rename = "enabledPlugins")]
    enabled: Option<InOrder>,
}

// `.claude/settings.json` as the default branch has it, read from the
// checkout's `origin` so a worker's branch never reaches it.
fn settings_on_default_branch(
    checkout: &Path,
    base: &str,
    refresh: bool,
) -> Result<Option<Read>, String> {
    use crate::worktree::git;
    // The checkout may be behind a merge that turned a plugin on; failing
    // to fetch leaves what it has.
    if refresh {
        let _ = git(checkout, ["fetch", "--quiet", "origin", base]);
    }
    let spec = format!("origin/{base}:{SETTINGS}");
    let Some(bytes) = blobs::read(checkout, &[&spec])?.pop().flatten() else {
        return Ok(None);
    };
    let text = String::from_utf8_lossy(&bytes);
    match serde_json::from_str::<Value>(&text) {
        Ok(settings) if settings.is_object() => {
            // A file whose `enabledPlugins` is no object turns on nothing.
            let enabled = serde_json::from_str::<Listed>(&text)
                .ok()
                .and_then(|listed| listed.enabled)
                .map_or_else(Vec::new, |in_order| in_order.0);
            Ok(Some((settings, enabled)))
        }
        Ok(_) => Err(format!("{SETTINGS} on {base} is not a JSON object")),
        Err(e) => Err(format!("{SETTINGS} on {base} is not valid JSON: {e}")),
    }
}

/// The default branch's commit as the checkout last fetched it, which a
/// runner compares to know the plugins may have changed
pub fn default_head(checkout: &Path, base: &str) -> Option<String> {
    use crate::worktree::{git, loose_commit_in};
    let tracking = format!("refs/remotes/origin/{base}");
    let spec = format!("origin/{base}");
    loose_commit_in(checkout, &tracking)
        .or_else(|| git(checkout, ["rev-parse", "--verify", "--quiet", &spec]).ok())
}

// Where marketplace `name`'s repo is on disk, fetched first where it is a
// git repo, and why the copy is old if the fetch failed.
fn marketplace(
    name: &str,
    known: Option<&Map<String, Value>>,
    checkout: &Path,
    base: &str,
    kelpie_home: &Path,
    git: &dyn Git,
) -> Result<(PathBuf, Option<String>), String> {
    let entry = known
        .and_then(|known| known.get(name))
        .ok_or_else(|| format!("{SETTINGS} has no extraKnownMarketplaces entry for {name}"))?;
    let source = &entry["source"];
    let text = |key: &str| source[key].as_str().filter(|s| !s.is_empty());
    let (url, directory) = match source["source"].as_str() {
        Some("github") => {
            let repo = text("repo").ok_or("its github source names no repo")?;
            (Some(format!("https://github.com/{repo}.git")), None)
        }
        Some("git") => (
            Some(text("url").ok_or("its git source has no url")?.to_owned()),
            None,
        ),
        Some("directory") => (
            None,
            Some(text("path").ok_or("its directory source has no path")?),
        ),
        Some(other) => return Err(format!("its {other} source is not one kelpie loads")),
        None => return Err(format!("the {name} marketplace names no source")),
    };
    if let Some(path) = directory {
        if !plain(name) {
            return Err(format!(
                "{name} is not a name kelpie keeps a marketplace under"
            ));
        }
        // From the default branch, not the working tree, which kelpie never moves.
        let into = kelpie_home.join(MARKETPLACES).join(name);
        return export(checkout, base, Path::new(path), &into)
            .map(|()| (into, None))
            .map_err(|e| {
                format!("its directory {path} cannot be read from the default branch: {e}")
            });
    }
    let url = url.unwrap_or_default();
    // A leading `-` would be taken for an option, and `::` names a transport
    // that runs a program.
    if url.starts_with('-') || url.contains("::") {
        return Err(format!("{url} is not a repo address"));
    }
    if !plain(name) {
        return Err(format!(
            "{name} is not a name kelpie keeps a marketplace under"
        ));
    }
    let into = kelpie_home.join(MARKETPLACES).join(name);
    let reference = text("ref");
    match git.sync(&url, reference, &into) {
        Ok(()) => Ok((into, None)),
        Err(e) if into.join(".claude-plugin/marketplace.json").is_file() => Ok((
            into,
            Some(format!("fetching the {name} marketplace failed: {e}")),
        )),
        Err(e) => Err(format!("fetching the {name} marketplace failed: {e}")),
    }
}

// Writes the folder `relative` as `origin`'s default branch holds it into
// `into`, replacing what was there. Only regular files come across: a link in
// the tree is left out, so nothing a marketplace holds leads out of it.
fn export(checkout: &Path, base: &str, relative: &Path, into: &Path) -> Result<(), String> {
    let plain = relative
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    let folder: PathBuf = relative.components().collect();
    if !plain || folder.as_os_str().is_empty() {
        return Err("it is not a folder inside the checkout".into());
    }
    let files = blobs::files_under(checkout, base, &[&folder])?;
    let _ = fs::remove_dir_all(into);
    if files.is_empty() {
        return Err("it holds no files there".into());
    }
    for (path, blob) in files {
        let Ok(rest) = path.strip_prefix(&folder) else {
            continue;
        };
        let target = into.join(rest);
        let parent = target.parent().unwrap_or(into);
        fs::create_dir_all(parent)
            .and_then(|()| fs::write(&target, blob))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

// What the files of the plugin at `folder` hold, to tell a change to them.
// Every folder of the plugin counts, since the copy holds every one of them.
fn fingerprint(found: &Found) -> u64 {
    fn walk(dir: &Path, hasher: &mut DefaultHasher) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            // A fetch rewrites files here, and `copy_plain` leaves it out.
            if entry.file_name() == ".git" {
                continue;
            }
            entry.file_name().hash(hasher);
            if kind.is_dir() {
                walk(&entry.path(), hasher);
            } else if kind.is_file() {
                fs::read(entry.path()).unwrap_or_default().hash(hasher);
            }
        }
    }
    let mut hasher = DefaultHasher::new();
    for part in &found.parts {
        part.as_str().hash(&mut hasher);
    }
    walk(&found.folder, &mut hasher);
    manifest(&found.folder, "").hash(&mut hasher);
    hasher.finish()
}

// The folder of plugin `name` in the marketplace at `root`.
fn plugin_in(root: &Path, name: &str) -> Result<PathBuf, String> {
    let file = root.join(".claude-plugin/marketplace.json");
    let listed: Value = fs::read_to_string(&file)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .ok_or_else(|| format!("{} is not a readable marketplace", file.display()))?;
    let entry = listed["plugins"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["name"].as_str() == Some(name))
        .ok_or_else(|| format!("the marketplace lists no plugin {name}"))?;
    let Some(source) = entry["source"].as_str() else {
        return Err("it comes from outside its marketplace, which kelpie does not load yet".into());
    };
    inside(root, Path::new(source))
        .map_err(|()| format!("its source {source} leaves the marketplace"))
}

// `relative` under `root`, if it names a place there: no `..`, no absolute
// path, and no link that leads out. A place that does not exist yet is
// returned for the caller to find missing.
fn inside(root: &Path, relative: &Path) -> Result<PathBuf, ()> {
    let plain = relative
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    if !plain {
        return Err(());
    }
    let place = root.join(relative);
    match (place.canonicalize(), root.canonicalize()) {
        (Ok(place_now), Ok(root_now)) if !place_now.starts_with(&root_now) => Err(()),
        _ => Ok(place),
    }
}

// What the `SKILL.md` at `file` gets wrong about its frontmatter, where it
// gets any of it wrong. A harness that refuses a frontmatter lists no skill of
// the file and puts its warning where kelpie keeps no log, so the reason is
// told here. A file that opens none is the harness's own to take or leave.
fn frontmatter_error(file: &Path) -> Option<String> {
    let Ok(text) = fs::read_to_string(file) else {
        return None;
    };
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    let Some(yaml) = frontmatter(rest) else {
        return Some("it opens with `---` and never closes it".to_owned());
    };
    serde_saphyr::from_str::<IgnoredAny>(yaml)
        .err()
        .map(|e| e.without_snippet().to_string())
}

// The frontmatter of a file whose text after the opening `---` line is `rest`:
// the text up to the next line that is `---`, where one closes it.
fn frontmatter(rest: &str) -> Option<&str> {
    let mut at = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\n', '\r']) == "---" {
            return Some(&rest[..at]);
        }
        at += line.len();
    }
    None
}

// The names of the skills in the plugin at `folder`: the folders of its
// `skills/` that `copy_plain` copies whole, with a `SKILL.md` in them.
fn skills_in(folder: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(folder.join("skills")) else {
        return Vec::new();
    };
    let mut skills: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| is_folder(&entry.path()) && is_file(&entry.path().join("SKILL.md")))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    skills.sort();
    skills
}

/// What a runner starting now would load of the checkout's plugins, from the
/// marketplaces already fetched, fetching nothing and writing only the copy of a
/// `directory` marketplace under kelpie's home
pub fn inspect(
    checkout: &Path,
    base: &str,
    kelpie_home: &Path,
    setup: &RepoSetup,
) -> Vec<RepoPlugin> {
    let mut plugins: Vec<RepoPlugin> = resolve(checkout, base, kelpie_home, &Fetched, false, setup)
        .into_iter()
        .map(|resolved| plan_one(resolved, Path::new(""), setup))
        .collect();
    plugins.extend(own::plan(checkout, base, Path::new(""), setup));
    plugins
}

// A git that fetches nothing: a marketplace is as the last runner left it.
struct Fetched;

impl Git for Fetched {
    fn sync(&self, _: &str, _: Option<&str>, into: &Path) -> Result<(), GitError> {
        if into.join(".claude-plugin/marketplace.json").is_file() {
            return Ok(());
        }
        Err(GitError(
            "it is not fetched yet, which a runner does as it starts".into(),
        ))
    }
}

// Writes the plugin's parts, every other folder of it, and a trimmed manifest
// into `dir`, replacing what was there.
fn install(found: &Found, dir: &Path) -> Result<(), String> {
    let manifest = manifest(&found.folder, &found.name);
    let written = (|| {
        match fs::remove_dir_all(dir) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        fs::create_dir_all(dir.join(MANIFEST_FOLDER))?;
        fs::write(dir.join(MANIFEST_FOLDER).join("plugin.json"), manifest)?;
        for part in &found.parts {
            match part {
                Part::Mcp => copy_plain(&found.folder, &dir.join(mcp::ROOT))?,
                _ => {
                    let folder = part.as_str();
                    copy_plain(&found.folder.join(folder), &dir.join(folder))?;
                }
            }
        }
        for name in rest(&found.folder) {
            let from = found.folder.join(&name);
            let to = dir.join(&name);
            // A skill may name a file at the plugin's own root, not only one
            // under a folder of it.
            if is_folder(&from) {
                copy_plain(&from, &to)?;
            } else {
                fs::copy(&from, &to)?;
            }
        }
        fill_in_skills(&dir.join(Part::Skills.as_str()), &dir.display().to_string())?;
        Ok(())
    })();
    written.map_err(|e| format!("cannot copy it into {}: {e}", dir.display()))
}

// Writes each `${CLAUDE_PLUGIN_ROOT}` in a `SKILL.md` under `dir` as the
// plugin's copy at `root`, the way [`mcp`] writes it for a server. A harness
// passes a skill's text as written, so the variable is a name only kelpie can
// answer, and a worker is left to guess which folder the skill means.
fn fill_in_skills(dir: &Path, root: &str) -> std::io::Result<()> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries.filter_map(Result::ok) {
        let kind = entry.file_type()?;
        let path = entry.path();
        if kind.is_dir() {
            fill_in_skills(&path, root)?;
        } else if kind.is_file() && entry.file_name() == "SKILL.md" {
            // A file that is not text is left as `copy_plain` wrote it. Filling
            // in is a convenience, and a failed install skips the whole plugin:
            // one skill file no harness reads cannot take its siblings' skills,
            // agents, hooks and MCP servers with it.
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            let Ok(text) = std::str::from_utf8(&bytes) else {
                continue;
            };
            let filled = mcp::fill_in(text, root);
            if filled != text {
                fs::write(&path, filled)?;
            }
        }
    }
    Ok(())
}

// The entries of the plugin at `folder` that are none of the parts, in name
// order: its folders and the files at its own root, which a skill names a file
// from. `.claude-plugin`, `mcp::ROOT` and `mcp::CONFIG_FILE` are aside: the
// copy writes its own manifest, and a server's files reach it through `mcp-root`.
fn rest(folder: &Path) -> Vec<OsString> {
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    let is_part = |name: &OsStr| FOLDERS.iter().any(|part| part.as_str() == name);
    let mut names: Vec<OsString> = entries
        .filter_map(Result::ok)
        // A link is no entry of a plugin kelpie copies: it could lead out.
        .filter(|entry| {
            entry
                .file_type()
                .is_ok_and(|kind| kind.is_dir() || kind.is_file())
        })
        .map(|entry| entry.file_name())
        .filter(|name| {
            let name = name.as_os_str();
            !is_part(name)
                && name != MANIFEST_FOLDER
                && name != ".git"
                && name != OsStr::new(mcp::ROOT)
                && name != OsStr::new(mcp::CONFIG_FILE)
        })
        .collect();
    names.sort();
    names
}

// Copies `from` into `to`, files and folders only. A link is left out, since
// a marketplace is not kelpie's and one could point at any file kelpie can read.
fn copy_plain(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        // A marketplace's own history is no part of a plugin.
        if entry.file_name() == ".git" {
            continue;
        }
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_plain(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

// Whether `path` is a folder itself and no link to one.
fn is_folder(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

// Whether `path` is a file itself and no link to one.
fn is_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
}

// The plugin's manifest with its name forced and its parts left out, or a
// bare one where the plugin has none.
fn manifest(plugin: &Path, name: &str) -> String {
    let file = plugin.join(MANIFEST_FOLDER).join("plugin.json");
    let read: Value = fs::read_to_string(&file)
        .ok()
        .filter(|_| is_file(&file))
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null);
    let mut kept = Map::new();
    for key in MANIFEST_KEYS {
        if let Some(value) = read.get(key) {
            kept.insert(key.to_owned(), value.clone());
        }
    }
    kept.insert("name".to_owned(), Value::String(name.to_owned()));
    Value::Object(kept).to_string()
}

// Whether `name` is one plain path component.
fn plain(name: &str) -> bool {
    SkillName::try_from(name.to_owned()).is_ok()
}

mod blobs;
mod mcp;
mod own;

pub use mcp::{Servers, host_of};
