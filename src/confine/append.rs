//! `kelpie confine --append <file>`: the project manager's hook, which lets
//! its file tools add to the end of one file and change nothing else
//!
//! A `Write` must keep the whole file as its start, and an `Edit` must leave
//! the file starting with all it held, adding at most [`MOST`] bytes. A file
//! that is a symlink, any other file or tool, and a call that cannot be read
//! are refused. The check reads the file before the write lands, so two
//! writes racing could each pass; one call runs at a time, so none do.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{Verdict, resolve};

/// The flag that holds the file tools to appending to one file
pub const APPEND: &str = "--append";

/// The most one write may add, in bytes
const MOST: usize = 64 * 1024;

/// Judges the tool call in `input`, which may only add to the end of `file`
pub fn judge_append(input: impl Read, file: &Path) -> Verdict {
    #[derive(Deserialize)]
    struct Call {
        cwd: PathBuf,
        #[serde(default)]
        tool_name: String,
        tool_input: Input,
    }
    #[derive(Deserialize)]
    struct Input {
        file_path: Option<PathBuf>,
        content: Option<String>,
        old_string: Option<String>,
        new_string: Option<String>,
        #[serde(default)]
        replace_all: bool,
    }
    let call: Call = match serde_json::from_reader(input) {
        Ok(call) => call,
        Err(e) => return Verdict::Refuse(format!("kelpie cannot read this tool call: {e}")),
    };
    let name = file.display();
    let target = call.tool_input.file_path.as_ref().map(|p| call.cwd.join(p));
    let same = |p: &Path| resolve(p).is_some_and(|r| Some(r) == resolve(file));
    if !target.as_deref().is_some_and(same) {
        return Verdict::Refuse(format!("{name} is the only file you may write"));
    }
    if std::fs::symlink_metadata(file).is_ok_and(|m| m.file_type().is_symlink()) {
        return Verdict::Refuse(format!(
            "{name} is a symlink, which kelpie never writes through"
        ));
    }
    let held = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Verdict::Refuse(format!("kelpie cannot read {name}: {}", e.kind())),
    };
    let input = call.tool_input;
    let after = match (call.tool_name.as_str(), input.content, input.old_string) {
        ("Write", Some(content), None) => content,
        ("Edit", None, Some(old)) if !old.is_empty() => {
            let new = input.new_string.unwrap_or_default();
            match input.replace_all {
                true => held.replace(&old, &new),
                false => held.replacen(&old, &new, 1),
            }
        }
        (tool, ..) => return Verdict::Refuse(format!("{tool} cannot be checked to only append")),
    };
    if !after.starts_with(&held) {
        return Verdict::Refuse(format!(
            "{name} takes additions at its end only: keep everything it holds, and add after it"
        ));
    }
    match after.len() - held.len() > MOST {
        true => Verdict::Refuse(format!(
            "one write may add at most {} KiB to {name}: keep notes short",
            MOST / 1024
        )),
        false => Verdict::Allow,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    struct World {
        _dir: tempfile::TempDir,
        notes: PathBuf,
    }

    fn world(held: &str) -> World {
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().canonicalize().unwrap().join("pm-notes.md");
        std::fs::write(&notes, held).unwrap();
        World { _dir: dir, notes }
    }

    impl World {
        fn judge(&self, tool: &str, input: serde_json::Value) -> Verdict {
            let call = json!({
                "cwd": self.notes.parent().unwrap(),
                "tool_name": tool,
                "tool_input": input,
            });
            judge_append(call.to_string().as_bytes(), &self.notes)
        }
    }

    #[test]
    fn a_write_or_edit_that_adds_to_the_end_goes_ahead() {
        let w = world("# Notes\n- #12 waits on #9\n");
        let write = json!({
            "file_path": "pm-notes.md",
            "content": "# Notes\n- #12 waits on #9\n- told: hold #4\n",
        });
        assert_eq!(w.judge("Write", write), Verdict::Allow);
        let edit = json!({
            "file_path": w.notes,
            "old_string": "waits on #9\n",
            "new_string": "waits on #9\n- told: hold #4\n",
        });
        assert_eq!(w.judge("Edit", edit), Verdict::Allow);
    }

    #[test]
    fn a_change_to_what_the_notes_hold_is_refused() {
        let w = world("- #12 waits on #9\n");
        let rewrite = json!({ "file_path": w.notes, "content": "- nothing waits\n" });
        let edit = json!({ "file_path": w.notes, "old_string": "#9", "new_string": "#10" });
        for (tool, input) in [("Write", rewrite), ("Edit", edit)] {
            let Verdict::Refuse(why) = w.judge(tool, input) else {
                panic!("{tool} went ahead");
            };
            assert!(
                why.ends_with("keep everything it holds, and add after it"),
                "{why}"
            );
        }
    }

    #[test]
    fn any_other_file_or_tool_is_refused() {
        let w = world("");
        let board = w.notes.with_file_name("board.md");
        let elsewhere = json!({ "file_path": board, "content": "forged" });
        assert!(matches!(w.judge("Write", elsewhere), Verdict::Refuse(_)));
        let climbing = json!({ "file_path": "../pm-notes.md", "content": "x" });
        assert!(matches!(w.judge("Write", climbing), Verdict::Refuse(_)));
        let notebook = json!({ "file_path": w.notes, "new_source": "x" });
        assert!(matches!(
            w.judge("NotebookEdit", notebook),
            Verdict::Refuse(_)
        ));
        assert!(matches!(
            judge_append(&b"not json"[..], &w.notes),
            Verdict::Refuse(_)
        ));
    }

    #[test]
    fn notes_that_are_a_symlink_or_a_write_past_the_cap_are_refused() {
        let w = world("- kept\n");
        let long = format!("- kept\n{}", "x".repeat(MOST + 1));
        let Verdict::Refuse(why) =
            w.judge("Write", json!({ "file_path": w.notes, "content": long }))
        else {
            panic!("a write past the cap went ahead");
        };
        assert!(why.starts_with("one write may add at most 64 KiB"), "{why}");

        let elsewhere = w.notes.with_file_name("elsewhere.md");
        std::fs::write(&elsewhere, "").unwrap();
        std::fs::remove_file(&w.notes).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &w.notes).unwrap();
        let write = json!({ "file_path": w.notes, "content": "- new\n" });
        let Verdict::Refuse(why) = w.judge("Write", write) else {
            panic!("a write through a symlink went ahead");
        };
        assert!(
            why.ends_with("is a symlink, which kelpie never writes through"),
            "{why}"
        );
    }

    #[test]
    fn notes_not_written_yet_take_a_first_write() {
        let w = world("");
        std::fs::remove_file(&w.notes).unwrap();
        let write = json!({ "file_path": w.notes, "content": "- first\n" });
        assert_eq!(w.judge("Write", write), Verdict::Allow);
    }
}
