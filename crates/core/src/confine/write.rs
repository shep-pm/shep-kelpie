//! The project's `write` hooks, run on a file write or edit `kelpie confine`
//! let through
//!
//! The folders are held first, so a hook never sees a write that was refused
//! already. Each file the call writes is judged on its own, with its path
//! from the worktree's root and what the harness gave of the change: the
//! `content` of a write or an edit's new text, or the `patch` Codex's
//! `apply_patch` carries.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{Verdict, strings};
use crate::hooks::{Hooks, Role};
use crate::settings::HookPoint;

// The keys a file tool gives the text it writes under, by harness: Claude
// Code's `content`, `new_string` and a notebook's `new_source`, pi's `newText`.
const CONTENT: [&str; 4] = ["content", "new_string", "new_source", "newText"];

/// Runs the `write` hooks on each of `targets`, the files `tool` writes
/// with `input` as its call, from the worktree `root`
pub(super) fn check(
    hooks: &Hooks,
    tool: &str,
    input: &Value,
    root: Option<&Path>,
    targets: &[PathBuf],
) -> Verdict {
    if !hooks.has(HookPoint::Write) {
        return Verdict::Allow;
    }
    let mut details = json!({ "tool": tool });
    match content(input) {
        Some(_) if tool == super::APPLY_PATCH => {}
        Some(text) => details["content"] = text.into(),
        None => {}
    }
    if tool == super::APPLY_PATCH {
        let mut texts = Vec::new();
        strings(input, &mut texts);
        details["patch"] = texts.join("\n").into();
    }
    for target in targets {
        details["path"] = relative(target, root).into();
        if let Err(refusal) = hooks.check(HookPoint::Write, Role::Worker, root, details.clone()) {
            return Verdict::Refuse(format!(
                "this write to `{}`: {refusal}. Change it so the hook accepts it, then try again.",
                relative(target, root)
            ));
        }
    }
    Verdict::Allow
}

// The text a write or an edit puts in the file.
fn content(input: &Value) -> Option<String> {
    if let Some(text) = CONTENT.iter().find_map(|k| input[k].as_str()) {
        return Some(text.to_owned());
    }
    // pi's edit and Claude Code's MultiEdit list their changes.
    let edits = input["edits"].as_array()?;
    let news: Vec<&str> = edits
        .iter()
        .filter_map(|e| CONTENT.iter().find_map(|k| e[k].as_str()))
        .collect();
    (!news.is_empty()).then(|| news.join("\n"))
}

// `path` from the worktree's root, or as it is when it is not under it.
fn relative(path: &Path, root: Option<&Path>) -> String {
    let Some(root) = root else {
        return path.display().to_string();
    };
    let canonical = crate::canonical(root);
    let under = |root: &Path| path.strip_prefix(root).ok();
    let inside = under(root).or_else(|| canonical.as_deref().ok().and_then(under));
    inside.unwrap_or(path).display().to_string()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::{Verdict, judge_with};
    use crate::hooks::Hooks;
    use crate::settings::{HookPoint, NonBlank, ProjectHook};
    use serde_json::json;

    // Refuses a payload that writes a TODO, giving the path it names.
    const NO_TODO: &str = "grep -q TODO && { echo 'leave no TODO behind' >&2; exit 2; }; exit 0";

    fn hooks(command: &str) -> Hooks {
        let hook = ProjectHook {
            point: HookPoint::Write,
            command: NonBlank::try_from(command.to_owned()).unwrap(),
        };
        Hooks::new("koji", "acme/koji", std::path::Path::new("/"), &[hook])
    }

    struct World {
        _dir: tempfile::TempDir,
        root: std::path::PathBuf,
    }

    fn world() -> World {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("wt/src")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        World { _dir: dir, root }
    }

    impl World {
        fn judge(&self, hooks: &Hooks, tool: &str, input: serde_json::Value) -> Verdict {
            let call = json!({
                "cwd": self.root.join("wt"),
                "tool_name": tool,
                "tool_input": input,
            });
            let folders = [self.root.join("wt"), self.root.join("target")];
            judge_with(call.to_string().as_bytes(), &folders, hooks)
        }

        fn file(&self, name: &str) -> String {
            self.root.join(name).display().to_string()
        }

        fn seen(&self) -> std::path::PathBuf {
            self.root.join("seen.jsonl")
        }

        fn recorder(&self) -> Hooks {
            hooks(&format!("cat >> {}; echo >> {0}", self.seen().display()))
        }

        fn payloads(&self) -> Vec<serde_json::Value> {
            let text = std::fs::read_to_string(self.seen()).unwrap_or_default();
            text.lines()
                .filter(|l| !l.is_empty())
                .map(|l| serde_json::from_str(l).unwrap())
                .collect()
        }
    }

    #[test]
    fn a_write_hook_refuses_a_file_write_with_its_reason() {
        let w = world();
        let hooks = hooks(NO_TODO);

        let input = json!({ "file_path": w.file("wt/src/lib.rs"), "content": "// TODO later" });
        let Verdict::Refuse(why) = w.judge(&hooks, "Write", input) else {
            panic!("the write went ahead");
        };

        assert!(why.contains("this write to `src/lib.rs`"), "{why}");
        assert!(
            why.contains("the `write` hook `grep` refused: leave no TODO behind"),
            "{why}"
        );
        let clean = json!({ "file_path": w.file("wt/src/lib.rs"), "content": "fn main() {}" });
        assert_eq!(w.judge(&hooks, "Write", clean), Verdict::Allow);
    }

    #[test]
    fn a_write_hook_reads_the_path_from_the_root_the_tool_and_the_content() {
        let w = world();
        let hooks = w.recorder();

        let write = json!({ "file_path": w.file("wt/src/lib.rs"), "content": "fn a() {}" });
        assert_eq!(w.judge(&hooks, "Write", write), Verdict::Allow);
        let edit = json!({ "file_path": "src/lib.rs", "old_string": "a", "new_string": "b" });
        assert_eq!(w.judge(&hooks, "Edit", edit), Verdict::Allow);

        let [write, edit] = &w.payloads()[..] else {
            panic!("{:?}", w.payloads());
        };
        assert_eq!(write["point"], "write");
        assert_eq!(write["role"], "worker");
        assert_eq!(write["tool"], "Write");
        assert_eq!(write["path"], "src/lib.rs");
        assert_eq!(write["content"], "fn a() {}");
        assert_eq!(edit["tool"], "Edit");
        assert_eq!(edit["path"], "src/lib.rs");
        assert_eq!(edit["content"], "b");
    }

    #[test]
    fn a_write_hook_reads_the_patch_of_an_apply_patch_once_for_each_file() {
        let w = world();
        let hooks = w.recorder();
        let patch = "*** Begin Patch\n*** Add File: src/a.rs\n+fn a() {}\n\
                     *** Update File: src/b.rs\n@@\n-x\n+y\n*** End Patch";

        let verdict = w.judge(&hooks, "apply_patch", json!({ "command": patch }));

        assert_eq!(verdict, Verdict::Allow);
        let payloads = w.payloads();
        let paths: Vec<_> = payloads.iter().map(|p| p["path"].clone()).collect();
        assert_eq!(paths, [json!("src/a.rs"), json!("src/b.rs")]);
        for payload in &payloads {
            assert_eq!(payload["tool"], "apply_patch");
            assert_eq!(payload["patch"], patch);
            assert!(payload.get("content").is_none());
        }
    }

    #[test]
    fn a_write_hook_reads_what_pi_s_write_and_edit_give() {
        let w = world();
        let hooks = w.recorder();

        let write = json!({ "file_path": w.file("wt/a.rs"), "content": "one" });
        let edit = json!({
            "file_path": w.file("wt/a.rs"),
            "edits": [{ "oldText": "a", "newText": "b" }, { "oldText": "c", "newText": "d" }],
        });
        assert_eq!(w.judge(&hooks, "Write", write), Verdict::Allow);
        assert_eq!(w.judge(&hooks, "Edit", edit), Verdict::Allow);

        let contents: Vec<_> = w.payloads().iter().map(|p| p["content"].clone()).collect();
        assert_eq!(contents, [json!("one"), json!("b\nd")]);
    }

    #[test]
    fn a_write_hook_reads_the_new_text_of_each_edit_in_a_multi_edit() {
        let w = world();
        let hooks = w.recorder();

        let multi = json!({
            "file_path": w.file("wt/a.rs"),
            "edits": [
                { "old_string": "a", "new_string": "b" },
                { "old_string": "c", "new_string": "d" },
            ],
        });
        assert_eq!(w.judge(&hooks, "MultiEdit", multi), Verdict::Allow);

        let contents: Vec<_> = w.payloads().iter().map(|p| p["content"].clone()).collect();
        assert_eq!(contents, [json!("b\nd")]);
    }

    #[test]
    fn a_write_hook_reads_the_new_source_of_a_notebook_edit() {
        let w = world();
        let hooks = w.recorder();

        let notebook = json!({ "notebook_path": w.file("wt/a.ipynb"), "new_source": "print(1)" });
        assert_eq!(w.judge(&hooks, "NotebookEdit", notebook), Verdict::Allow);

        let payloads = w.payloads();
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0]["content"], "print(1)");
        assert_eq!(payloads[0]["tool"], "NotebookEdit");
    }

    #[test]
    fn a_write_outside_the_folders_never_reaches_the_hook() {
        let w = world();
        let hooks = w.recorder();

        let input = json!({ "file_path": w.file("elsewhere/x"), "content": "TODO" });
        let verdict = w.judge(&hooks, "Write", input);

        assert!(matches!(verdict, Verdict::Refuse(why) if why.contains("outside the folders")));
        assert!(w.payloads().is_empty());
    }

    #[test]
    fn a_write_hook_that_hangs_refuses_the_write_as_failed() {
        let w = world();
        let hooks = hooks("sleep 30").with_timeout(Duration::from_millis(100));

        let input = json!({ "file_path": w.file("wt/a.rs"), "content": "x" });
        let Verdict::Refuse(why) = w.judge(&hooks, "Write", input) else {
            panic!("the write went ahead");
        };

        assert!(why.contains("failed"), "{why}");
    }
}
