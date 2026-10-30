//! Files on `origin`'s default branch, read with one git call to list them
//! and one to read them however many there are

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::worktree::in_repo;

/// The regular files under `folders` on `origin/<base>`, by path in the tree, with their bytes
///
/// A link in the tree is left out, so nothing the branch holds leads out of
/// where its files are written.
pub(super) fn files_under(
    checkout: &Path,
    base: &str,
    folders: &[&Path],
) -> Result<Vec<(PathBuf, Vec<u8>)>, String> {
    let output = in_repo(checkout)
        .args(["--literal-pathspecs", "ls-tree", "-r", "-z"])
        .arg(format!("origin/{base}"))
        .arg("--")
        .args(folders)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    let mut regular = Vec::new();
    for entry in output.stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let text = String::from_utf8_lossy(entry);
        let Some((meta, path)) = text.split_once('\t') else {
            continue;
        };
        let fields: Vec<&str> = meta.split(' ').collect();
        if let ["100644" | "100755", "blob", id] = fields[..] {
            regular.push((PathBuf::from(path), id.to_owned()));
        }
    }
    let ids: Vec<&str> = regular.iter().map(|(_, id)| id.as_str()).collect();
    let blobs = read(checkout, &ids)?;
    (regular.into_iter().zip(blobs))
        .map(|((path, _), blob)| {
            let blob = blob.ok_or_else(|| format!("{} is listed and missing", path.display()))?;
            Ok((path, blob))
        })
        .collect()
}

/// The bytes at each of `specs`, none where git finds nothing, from one `cat-file --batch`
pub(super) fn read(checkout: &Path, specs: &[&str]) -> Result<Vec<Option<Vec<u8>>>, String> {
    if specs.is_empty() {
        return Ok(Vec::new());
    }
    if specs.iter().any(|spec| spec.contains('\n')) {
        return Err("a name with a line break cannot be read".to_owned());
    }
    let mut child = in_repo(checkout)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let input: String = specs.iter().map(|spec| format!("{spec}\n")).collect();
    let mut stdin = child.stdin.take().ok_or("git took no input")?;
    // Git answers while it reads, so the input goes in from a thread of its own.
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    let _ = writer.join();
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    answers(&output.stdout, specs.len()).ok_or_else(|| "git's answer was cut short".to_owned())
}

// `cat-file --batch`'s answers: `<id> <type> <size>`, the bytes and a line
// break for an object, `<spec> missing` or `<spec> ambiguous` for none.
fn answers(mut out: &[u8], count: usize) -> Option<Vec<Option<Vec<u8>>>> {
    let mut found = Vec::with_capacity(count);
    for _ in 0..count {
        let end = out.iter().position(|b| *b == b'\n')?;
        let header = std::str::from_utf8(&out[..end]).ok()?;
        out = &out[end + 1..];
        if header.ends_with(" missing") || header.ends_with(" ambiguous") {
            found.push(None);
            continue;
        }
        let size: usize = header.rsplit(' ').next()?.parse().ok()?;
        found.push(Some(out.get(..size)?.to_vec()));
        out = out.get(size + 1..)?;
    }
    Some(found)
}

#[cfg(test)]
mod tests {
    use shep_kelpie_test_support::{git, write_in};

    use super::*;

    // A checkout whose `origin/main` holds `files`.
    fn checkout(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "--quiet", "-b", "main"]);
        for (file, text) in files {
            write_in(dir.path(), file, text);
        }
        git(dir.path(), &["add", "--all"]);
        git(
            dir.path(),
            &["commit", "--quiet", "--allow-empty", "-m", "first"],
        );
        git(
            dir.path(),
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
        dir
    }

    #[test]
    fn the_files_under_several_folders_come_back_with_their_bytes() {
        let dir = checkout(&[
            (".claude/skills/a/SKILL.md", "a\n"),
            (".claude/agents/b.md", "line\nbreaks\n\n"),
            (".claude/other/c.md", "not asked\n"),
        ]);
        let folders = [Path::new(".claude/skills"), Path::new(".claude/agents")];

        let mut files = files_under(dir.path(), "main", &folders).unwrap();
        files.sort();

        assert_eq!(
            files,
            [
                (
                    PathBuf::from(".claude/agents/b.md"),
                    b"line\nbreaks\n\n".to_vec()
                ),
                (PathBuf::from(".claude/skills/a/SKILL.md"), b"a\n".to_vec()),
            ]
        );
    }

    #[test]
    fn a_name_git_finds_nothing_at_reads_as_none_beside_one_it_finds() {
        let dir = checkout(&[(".mcp.json", "{}")]);

        let read = read(
            dir.path(),
            &["origin/main:.claude/settings.json", "origin/main:.mcp.json"],
        );

        assert_eq!(read.unwrap(), [None, Some(b"{}".to_vec())]);
    }

    #[test]
    fn a_folder_that_is_not_there_holds_no_files() {
        let dir = checkout(&[("README.md", "a project\n")]);

        assert_eq!(
            files_under(dir.path(), "main", &[Path::new(".claude/skills")]),
            Ok(Vec::new())
        );
    }
}
