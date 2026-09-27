//! `kelpie confine <folder>...`: a PreToolUse hook that holds Claude's file
//! tools to the folders named
//!
//! Claude Code's sandbox covers Bash only, and `bypassPermissions` lets the
//! file tools write anywhere. The hook reads the tool call on stdin and
//! refuses a write outside the folders, following symlinks, so a worker's
//! Edit and Write stop where its Bash does. Anything unreadable is refused.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

/// What the hook tells Claude Code
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The call goes ahead
    Allow,
    /// The call is refused, and Claude is told why
    Refuse(String),
}

/// Judges the tool call in `input` against `folders`
pub fn judge(input: impl Read, folders: &[PathBuf]) -> Verdict {
    #[derive(Deserialize)]
    struct Call {
        cwd: PathBuf,
        tool_input: ToolInput,
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
    let Some(target) = call.tool_input.file_path.or(call.tool_input.notebook_path) else {
        return Verdict::Allow;
    };
    let target = call.cwd.join(target);
    let inside = resolve(&target).is_some_and(|resolved| {
        folders
            .iter()
            .any(|f| resolved.starts_with(f.canonicalize().unwrap_or_else(|_| f.clone())))
    });
    if inside {
        return Verdict::Allow;
    }
    let allowed: Vec<_> = folders.iter().map(|f| f.display().to_string()).collect();
    Verdict::Refuse(format!(
        "{} is outside the folders this worker may write: {}",
        target.display(),
        allowed.join(", ")
    ))
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
            let call = json!({
                "session_id": "s",
                "cwd": self.root.join("wt"),
                "hook_event_name": "PreToolUse",
                "tool_name": "Write",
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
        for p in ["wt/../home/x", "wt/missing/../../home/x"] {
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
    fn a_notebook_path_is_judged_too() {
        let w = world();
        let verdict = w.judge(json!({ "notebook_path": w.path("home/n.ipynb") }));
        assert!(matches!(verdict, Verdict::Refuse(_)), "{verdict:?}");
    }

    #[test]
    fn a_call_without_a_path_goes_ahead() {
        assert_eq!(world().judge(json!({ "command": "ls" })), Verdict::Allow);
    }

    #[test]
    fn an_unreadable_call_is_refused() {
        let verdict = judge(&b"not json"[..], &[PathBuf::from("/")]);
        assert!(matches!(verdict, Verdict::Refuse(_)), "{verdict:?}");
    }
}
