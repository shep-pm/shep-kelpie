//! Kelpie's copy of mattpocock/skills, built into the binary
//!
//! `scripts/vendor-skills.sh` fetches it into `skills/` at the
//! pinned commit, with its licence and the pin's `git ls-tree` in
//! `blobs.txt`. The runner writes it out as a Claude Code plugin when it
//! starts, so a project installs nothing.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The upstream repo
pub const UPSTREAM: &str = "https://github.com/mattpocock/skills";

/// The upstream commit every file here was taken from
pub const PIN: &str = "d81f3a183412e71a5b1e84ca21bc1a35eea03a60";

/// The plugin's name, which each skill's slash command starts with
pub const PLUGIN: &str = "mattpocock";

// Each vendored file, by its path in the upstream repo.
macro_rules! vendored {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_bytes!(concat!("../../skills/", $path)))),*]
    };
}

/// Every vendored file, by its path upstream, and its bytes
pub(super) const FILES: &[(&str, &[u8])] = vendored![
    "LICENSE",
    "skills/engineering/code-review/SKILL.md",
    "skills/engineering/code-review/agents/openai.yaml",
    "skills/engineering/diagnosing-bugs/SKILL.md",
    "skills/engineering/diagnosing-bugs/agents/openai.yaml",
    "skills/engineering/diagnosing-bugs/scripts/hitl-loop.template.sh",
    "skills/engineering/implement/SKILL.md",
    "skills/engineering/implement/agents/openai.yaml",
    "skills/engineering/pr/CREDITS.md",
    "skills/engineering/pr/SKILL.md",
    "skills/engineering/pr/agents/openai.yaml",
    "skills/engineering/retro/SKILL.md",
    "skills/engineering/retro/agents/openai.yaml",
    "skills/engineering/tdd/SKILL.md",
    "skills/engineering/tdd/agents/openai.yaml",
    "skills/engineering/tdd/mocking.md",
    "skills/engineering/tdd/tests.md",
    "skills/engineering/to-spec/SKILL.md",
    "skills/engineering/to-spec/agents/openai.yaml",
    "skills/engineering/to-tickets/SKILL.md",
    "skills/engineering/to-tickets/agents/openai.yaml",
    "skills/engineering/triage/AGENT-BRIEF.md",
    "skills/engineering/triage/OUT-OF-SCOPE.md",
    "skills/engineering/triage/SKILL.md",
    "skills/engineering/triage/agents/openai.yaml",
    "skills/productivity/handoff/SKILL.md",
    "skills/productivity/handoff/agents/openai.yaml",
];

/// Writes the plugin into `dir`, replacing whatever was there
///
/// A skill lands at `skills/<name>/`, dropping upstream's grouping folder,
/// where Claude Code looks for a plugin's skills.
///
/// # Errors
///
/// The first file or folder that could not be removed or written.
pub(super) fn write(dir: &Path) -> io::Result<()> {
    match fs::remove_dir_all(dir) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    for (path, bytes) in FILES {
        let to = dir.join(plugin_path(path));
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(to, bytes)?;
    }
    let manifest = serde_json::json!({
        "name": PLUGIN,
        "description": format!("mattpocock/skills at {PIN}, vendored by kelpie"),
        "repository": UPSTREAM,
        "license": "MIT",
    });
    let manifest = serde_json::to_string_pretty(&manifest).expect("the manifest is JSON");
    fs::create_dir_all(dir.join(".claude-plugin"))?;
    fs::write(dir.join(".claude-plugin/plugin.json"), manifest)
}

// `skills/<group>/<name>/<rest>` upstream is `skills/<name>/<rest>` in the plugin.
fn plugin_path(upstream: &str) -> PathBuf {
    match upstream
        .strip_prefix("skills/")
        .and_then(|p| p.split_once('/'))
    {
        Some((_group, rest)) => Path::new("skills").join(rest),
        None => PathBuf::from(upstream),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use sha1::{Digest, Sha1};

    use super::*;

    // The folder `scripts/vendor-skills.sh` fills
    fn vendor_folder() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("skills")
    }

    // The id git gives a file's contents, as `git ls-tree` prints it
    fn blob_id(bytes: &[u8]) -> String {
        let mut hash = Sha1::new();
        hash.update(format!("blob {}\0", bytes.len()).as_bytes());
        hash.update(bytes);
        hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    fn files_under(dir: &Path, root: &Path, found: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files_under(&path, root, found);
            } else {
                let relative = path.strip_prefix(root).unwrap();
                found.push(relative.to_str().unwrap().to_owned());
            }
        }
    }

    #[test]
    fn the_vendored_copies_match_the_pinned_version() {
        let folder = vendor_folder();
        let upstream = fs::read_to_string(folder.join("UPSTREAM")).unwrap();
        assert_eq!(upstream, format!("{UPSTREAM}\n{PIN}\n"));
        let pinned: BTreeMap<&str, &str> = include_str!("../../skills/blobs.txt")
            .lines()
            .map(|line| {
                let (id, path) = line.split_once("  ").unwrap();
                (path, id)
            })
            .collect();
        let built: BTreeMap<&str, String> = FILES
            .iter()
            .map(|(path, bytes)| (*path, blob_id(bytes)))
            .collect();
        let built: BTreeMap<&str, &str> = built.iter().map(|(p, id)| (*p, id.as_str())).collect();
        assert_eq!(built, pinned);
    }

    #[test]
    fn every_vendored_file_is_built_in() {
        let folder = vendor_folder();
        let mut found = Vec::new();
        files_under(&folder, &folder, &mut found);
        found.retain(|path| path != "UPSTREAM" && path != "blobs.txt");
        found.sort();
        let mut built: Vec<&str> = FILES.iter().map(|(path, _)| *path).collect();
        built.sort_unstable();
        assert_eq!(found, built);
    }

    #[test]
    fn the_plugin_holds_each_skill_under_its_name_with_the_licence() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = dir.path().join("plugin");
        fs::create_dir_all(plugin.join("stale")).unwrap();
        write(&plugin).unwrap();
        assert!(!plugin.join("stale").exists());
        let tests = fs::read(plugin.join("skills/tdd/tests.md")).unwrap();
        assert_eq!(
            tests,
            fs::read(vendor_folder().join("skills/engineering/tdd/tests.md")).unwrap()
        );
        assert!(plugin.join("skills/handoff/SKILL.md").is_file());
        let licence = fs::read_to_string(plugin.join("LICENSE")).unwrap();
        assert!(licence.starts_with("MIT License"), "{licence}");
        let manifest = fs::read_to_string(plugin.join(".claude-plugin/plugin.json")).unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
        assert_eq!(manifest["name"], PLUGIN);
    }
}
