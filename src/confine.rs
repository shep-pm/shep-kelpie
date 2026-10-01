//! `kelpie confine <folder>...`: a PreToolUse hook that holds a worker's
//! file tools to the folders named
//!
//! Claude Code's sandbox covers Bash only, and `bypassPermissions` lets the
//! file tools write anywhere. Codex writes files with `apply_patch`, and
//! runs the same hook on it. The hook reads the tool call on stdin and
//! refuses a write outside the folders, following symlinks, so a worker's
//! Edit and Write stop where its Bash does. Every harness's own files inside
//! the folders are refused too. Anything unreadable is refused.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::fence;

/// What the hook tells Claude Code
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The call goes ahead
    Allow,
    /// The call is refused, and Claude is told why
    Refuse(String),
}

/// Codex's file tool, whose one call can add, change, move and delete files
const APPLY_PATCH: &str = "apply_patch";

/// The lines of an `apply_patch` patch that name a file it writes
const PATCH_PATHS: [&str; 4] = [
    "*** Add File:",
    "*** Update File:",
    "*** Delete File:",
    "*** Move to:",
];

/// Judges the tool call in `input` against `folders`
///
/// Claude Code's file tools name one path. Codex's `apply_patch` names
/// each file in its patch, and every one must pass.
pub fn judge(input: impl Read, folders: &[PathBuf]) -> Verdict {
    #[derive(Deserialize)]
    struct Call {
        cwd: PathBuf,
        #[serde(default)]
        tool_name: String,
        tool_input: serde_json::Value,
    }
    #[derive(Deserialize)]
    struct ToolInput {
        file_path: Option<PathBuf>,
        notebook_path: Option<PathBuf>,
    }
    let call: Call = match serde_json::from_reader(input) {
        Ok(call) => call,
        Err(e) => return Verdict::Refuse(format!("kelpie cannot read this tool call: {e}")),
    };
    if call.tool_name == APPLY_PATCH {
        let targets = patch_paths(&call.tool_input);
        if targets.is_empty() {
            return Verdict::Refuse("kelpie cannot find the files this patch writes".into());
        }
        return targets
            .iter()
            .map(|target| judge_path(&call.cwd.join(target), folders))
            .find(|verdict| *verdict != Verdict::Allow)
            .unwrap_or(Verdict::Allow);
    }
    let input: ToolInput = match serde_json::from_value(call.tool_input) {
        Ok(input) => input,
        Err(e) => return Verdict::Refuse(format!("kelpie cannot read this tool call: {e}")),
    };
    let Some(target) = input.file_path.or(input.notebook_path) else {
        return Verdict::Allow;
    };
    judge_path(&call.cwd.join(target), folders)
}

// Every file a patch anywhere in `input` names, however its lines are
// indented, since Codex reads them trimmed.
fn patch_paths(input: &serde_json::Value) -> Vec<PathBuf> {
    let mut texts = Vec::new();
    strings(input, &mut texts);
    texts
        .iter()
        .flat_map(|text| text.lines())
        .filter_map(|line| {
            let line = line.trim();
            PATCH_PATHS
                .iter()
                .find_map(|mark| line.strip_prefix(mark))
                .map(|path| PathBuf::from(path.trim()))
        })
        .collect()
}

// Every string in `value`, at any depth.
fn strings<'a>(value: &'a serde_json::Value, out: &mut Vec<&'a str>) {
    match value {
        serde_json::Value::String(s) => out.push(s),
        serde_json::Value::Array(items) => items.iter().for_each(|v| strings(v, out)),
        serde_json::Value::Object(fields) => fields.values().for_each(|v| strings(v, out)),
        _ => {}
    }
}

// Whether a write to `target` stays in `folders` and off every harness's own files.
fn judge_path(target: &Path, folders: &[PathBuf]) -> Verdict {
    let canonical: Vec<PathBuf> = folders
        .iter()
        .map(|f| f.canonicalize().unwrap_or_else(|_| f.clone()))
        .collect();
    let within = |path: &Path| -> Vec<PathBuf> {
        folders
            .iter()
            .chain(&canonical)
            .filter_map(|f| path.strip_prefix(f).ok().map(Path::to_owned))
            .collect()
    };
    let Some(resolved) = resolve(target).filter(|r| canonical.iter().any(|f| r.starts_with(f)))
    else {
        let allowed: Vec<_> = folders.iter().map(|f| f.display().to_string()).collect();
        return Verdict::Refuse(format!(
            "{} is outside the folders this worker may write: {}",
            target.display(),
            allowed.join(", ")
        ));
    };
    // As written and as resolved, so a link to or from `.claude` changes nothing.
    if let Some(harness) = within(target)
        .iter()
        .chain(&within(&resolved))
        .find_map(|p| fence::owner(p))
    {
        return Verdict::Refuse(format!(
            "{} is {harness}'s own configuration, which only the maintainer changes",
            target.display()
        ));
    }
    Verdict::Allow
}

// The deepest part of `path` that exists, with symlinks followed, and the
// rest joined on. `None` when the rest climbs with `..`.
fn resolve(path: &Path) -> Option<PathBuf> {
    let mut existing = path;
    let mut rest = Vec::new();
    loop {
        if let Ok(real) = existing.canonicalize() {
            let mut resolved = real;
            for part in rest.iter().rev() {
                resolved.push(part);
            }
            return Some(resolved);
        }
        let name = existing.file_name()?;
        rest.push(name.to_owned());
        existing = existing.parent()?;
        if matches!(
            existing.components().next_back(),
            Some(Component::ParentDir)
        ) {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use serde_json::json;

    use super::*;

    struct World {
        _dir: tempfile::TempDir,
        root: PathBuf,
    }

    // Canonical from the start, since macOS's temporary folder is a symlink.
    fn world() -> World {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for folder in ["wt/src", "target", "home"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        World { _dir: dir, root }
    }

    impl World {
        fn folders(&self) -> Vec<PathBuf> {
            vec![self.root.join("wt"), self.root.join("target")]
        }

        fn judge(&self, tool_input: serde_json::Value) -> Verdict {
            self.judge_tool("Write", tool_input)
        }

        fn judge_tool(&self, tool: &str, tool_input: serde_json::Value) -> Verdict {
            let call = json!({
                "session_id": "s",
                "cwd": self.root.join("wt"),
                "hook_event_name": "PreToolUse",
                "tool_name": tool,
                "tool_input": tool_input,
            });
            judge(call.to_string().as_bytes(), &self.folders())
        }

        fn path(&self, p: &str) -> String {
            self.root.join(p).display().to_string()
        }
    }

    #[test]
    fn a_write_inside_a_folder_goes_ahead() {
        let w = world();
        for p in ["wt/src/lib.rs", "wt/new/deep/file.rs", "target/x"] {
            assert_eq!(
                w.judge(json!({ "file_path": w.path(p) })),
                Verdict::Allow,
                "{p}"
            );
        }
        assert_eq!(
            w.judge(json!({ "file_path": "src/relative.rs" })),
            Verdict::Allow
        );
    }

    #[test]
    fn a_write_outside_is_refused_naming_the_folders() {
        let w = world();
        let Verdict::Refuse(why) = w.judge(json!({ "file_path": w.path("home/.zshrc") })) else {
            panic!("a write to the home folder went ahead");
        };
        assert!(why.contains("home/.zshrc is outside the folders"), "{why}");
        assert!(
            why.ends_with(&format!("{}, {}", w.path("wt"), w.path("target"))),
            "{why}"
        );
    }

    #[test]
    fn climbing_out_with_dots_is_refused() {
        let w = world();
        for p in ["wt/../home/me", "wt/missing/../../home/me"] {
            let verdict = w.judge(json!({ "file_path": w.path(p) }));
            assert!(matches!(verdict, Verdict::Refuse(_)), "{p}");
        }
    }

    #[test]
    fn a_symlink_out_of_the_worktree_is_followed_and_refused() {
        let w = world();
        symlink(w.root.join("home"), w.root.join("wt/escape")).unwrap();
        let verdict = w.judge(json!({ "file_path": w.path("wt/escape/.zshrc") }));
        assert!(matches!(verdict, Verdict::Refuse(_)), "{verdict:?}");
    }

    #[test]
    fn claude_codes_own_files_are_refused_to_write_and_edit() {
        let w = world();
        std::fs::create_dir_all(w.root.join("wt/.claude")).unwrap();
        for p in [
            "wt/.claude/settings.local.json",
            "wt/.claude/settings.json",
            "wt/.claude/agents/escape.md",
            "wt/.CLAUDE/settings.json",
            "wt/src/.claude/skills/x/SKILL.md",
            "wt/.mcp.json",
            "wt/.MCP.json",
            "wt/.mcp.j\u{17f}on",
            "target/.claude/settings.json",
        ] {
            for tool in ["Write", "Edit"] {
                let Verdict::Refuse(why) = w.judge_tool(tool, json!({ "file_path": w.path(p) }))
                else {
                    panic!("{tool} of {p} went ahead");
                };
                assert!(why.contains("Claude Code's own configuration"), "{why}");
            }
        }
        assert_eq!(
            w.judge(json!({ "file_path": ".claude/settings.local.json" })),
            w.judge(json!({ "file_path": w.path("wt/.claude/settings.local.json") })),
        );
        for p in ["wt/src/mcp.json", "wt/src/.mcp.json", "wt/claude/notes.md"] {
            assert_eq!(
                w.judge(json!({ "file_path": w.path(p) })),
                Verdict::Allow,
                "{p}"
            );
        }
    }

    #[test]
    fn codexs_own_files_are_refused_to_write_and_edit() {
        let w = world();
        for p in [
            "wt/.codex/config.toml",
            "wt/.CODEX/config.toml",
            "wt/src/.Codex/config.toml",
            "wt/.co\u{301}dex/config.toml",
        ] {
            for tool in ["Write", "Edit"] {
                let Verdict::Refuse(why) = w.judge_tool(tool, json!({ "file_path": w.path(p) }))
                else {
                    panic!("{tool} of {p} went ahead");
                };
                assert!(why.contains("Codex's own configuration"), "{why}");
            }
        }
        assert_eq!(
            w.judge(json!({ "file_path": w.path("wt/src/codex.rs") })),
            Verdict::Allow
        );
    }

    #[test]
    fn a_link_to_or_from_claudes_own_folder_is_refused() {
        let w = world();
        std::fs::create_dir_all(w.root.join("wt/.claude")).unwrap();
        symlink(w.root.join("wt/.claude"), w.root.join("wt/cfg")).unwrap();
        let verdict = w.judge(json!({ "file_path": w.path("wt/cfg/settings.local.json") }));
        assert!(matches!(verdict, Verdict::Refuse(_)), "{verdict:?}");

        let w = world();
        symlink(w.root.join("wt/src"), w.root.join("wt/.claude")).unwrap();
        let verdict = w.judge(json!({ "file_path": w.path("wt/.claude/settings.json") }));
        assert!(matches!(verdict, Verdict::Refuse(_)), "{verdict:?}");
    }

    #[test]
    fn a_notebook_path_is_judged_too() {
        let w = world();
        let verdict = w.judge(json!({ "notebook_path": w.path("home/n.ipynb") }));
        assert!(matches!(verdict, Verdict::Refuse(_)), "{verdict:?}");
    }

    #[test]
    fn a_call_without_a_path_goes_ahead() {
        assert_eq!(world().judge(json!({ "command": "ls" })), Verdict::Allow);
    }

    fn patch(body: &str) -> serde_json::Value {
        json!({ "command": format!("*** Begin Patch\n{body}\n*** End Patch\n") })
    }

    #[test]
    fn a_codex_patch_inside_the_folders_goes_ahead() {
        let w = world();
        let body = format!(
            "*** Add File: src/new.rs\n+fn main() {{}}\n*** Update File: {}\n@@\n-a\n+b\n\
             *** Delete File: old.rs",
            w.path("target/x")
        );
        assert_eq!(w.judge_tool("apply_patch", patch(&body)), Verdict::Allow);
    }

    #[test]
    fn a_codex_patch_with_any_file_outside_or_fenced_is_refused() {
        let w = world();
        for body in [
            format!(
                "*** Add File: src/ok.rs\n+x\n*** Add File: {}\n+x",
                w.path("home/.zshrc")
            ),
            "*** Update File: src/lib.rs\n*** Move to: ../outside/lib.rs\n@@\n-a\n+b".into(),
            "*** Delete File: ../outside/.zshrc".into(),
            "*** Add File: .codex/config.toml\n+x".into(),
            "  *** Add File: .claude/settings.json\n+{}".into(),
            "*** Update File: .mcp.json\n@@\n-a\n+b".into(),
        ] {
            let verdict = w.judge_tool("apply_patch", patch(&body));
            assert!(matches!(verdict, Verdict::Refuse(_)), "{body}: {verdict:?}");
        }
    }

    #[test]
    fn a_codex_patch_is_read_wherever_its_call_carries_it() {
        let w = world();
        let text = "*** Begin Patch\n*** Add File: ../outside/x\n+x\n*** End Patch";
        for input in [
            json!({ "input": text }),
            json!({ "command": ["apply_patch", text] }),
        ] {
            let verdict = w.judge_tool("apply_patch", input.clone());
            assert!(matches!(verdict, Verdict::Refuse(_)), "{input}");
        }
    }

    #[test]
    fn a_codex_patch_naming_no_file_is_refused() {
        let w = world();
        for input in [json!({}), json!({ "command": "rm -rf ~" }), patch("")] {
            let verdict = w.judge_tool("apply_patch", input.clone());
            assert!(matches!(verdict, Verdict::Refuse(_)), "{input}");
        }
    }

    #[test]
    fn an_unreadable_call_is_refused() {
        let verdict = judge(&b"not json"[..], &[PathBuf::from("/")]);
        assert!(matches!(verdict, Verdict::Refuse(_)), "{verdict:?}");
    }
}
