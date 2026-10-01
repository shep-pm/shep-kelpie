//! The shell scripts a command runs from a file
//!
//! `bash push.sh`, `source push.sh` and `./push.sh` run commands the call's
//! own text never shows, so the guard reads the file and judges them too. A
//! script the guard cannot read is refused, as is one the same call could
//! write first.

use std::fs::{self, File};
use std::io::Read as _;
use std::path::Path;

use super::moved;
use super::wrap::{OTHER_SHELLS, SHELLS, program};

// A script larger than this is not a worker's own.
const SCRIPT_MAX: u64 = 1 << 20;

/// The file `source` or `.` runs
pub(super) fn sourced(words: &[String]) -> Option<&str> {
    match words.get(1).map(String::as_str) {
        Some("--") => words.get(2).map(String::as_str),
        file => file,
    }
}

/// The script `name` names, read for its commands
///
/// # Errors
///
/// The refusal, for a script the guard cannot read, or one a text of the
/// call in `texts` names again, which could write it before it runs.
pub(super) fn read(
    name: &str,
    cwd: Option<&Path>,
    home: Option<&Path>,
    texts: &[String],
) -> Result<String, String> {
    let base = program(name);
    if base.is_empty() || texts.iter().any(|t| t.matches(base).count() > 1) {
        return Err(
            "kelpie checks a script only in a call that does not name it twice, since \
                    the call could change it before it runs: run the script in a call of its own."
                .into(),
        );
    }
    let unreadable = || {
        "kelpie cannot read the script this command runs: it must be a file already there, \
         in a folder the guard can follow."
            .to_owned()
    };
    let path = moved(cwd, name, home).ok_or_else(unreadable)?;
    // A pipe would hold the hook open.
    let small = fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() <= SCRIPT_MAX);
    if !small {
        return Err(unreadable());
    }
    fs::read_to_string(path).map_err(|_| unreadable())
}

/// Whether the program `word` names by its path is a shell script, by its `#!`
///
/// # Errors
///
/// The refusal, for a script of a shell the guard does not read, or a path
/// under a folder it cannot follow.
pub(super) fn interpreted(
    word: &str,
    cwd: Option<&Path>,
    home: Option<&Path>,
) -> Result<bool, String> {
    let Some(path) = moved(cwd, word, home) else {
        return Err(
            "kelpie cannot tell what a program named from a folder it cannot follow \
                    runs: name it from the worktree's root or by its full path."
                .into(),
        );
    };
    if !fs::metadata(&path).is_ok_and(|m| m.is_file()) {
        return Ok(false);
    }
    let mut head = Vec::new();
    // Unopened, it runs nothing.
    let Ok(file) = File::open(&path) else {
        return Ok(false);
    };
    if file.take(4).read_to_end(&mut head).is_err() || BINARIES.iter().any(|b| head.starts_with(b))
    {
        return Ok(false);
    }
    // A file with no `#!` that the system will not run, a shell runs itself.
    if !head.starts_with(b"#!") {
        return Ok(true);
    }
    let text = fs::read_to_string(&path).unwrap_or_default();
    let first = text.lines().next().unwrap_or_default();
    let mut words = first.trim_start_matches("#!").split_whitespace();
    let shell = match words.next().map(program) {
        // `env`'s own options and assignments come before the program.
        Some("env") => {
            let mut program_word = None;
            while let Some(word) = words.next() {
                match word {
                    "-u" | "--unset" | "-C" | "--chdir" | "-P" => {
                        words.next();
                    }
                    w if w.starts_with('-') || w.contains('=') => {}
                    w => {
                        program_word = Some(program(w));
                        break;
                    }
                }
            }
            program_word
        }
        first => first,
    };
    match shell {
        Some(shell) if OTHER_SHELLS.contains(&shell) => Err(format!(
            "kelpie cannot check a `{shell}` script: run the commands directly, or with `bash -c`."
        )),
        Some(shell) => Ok(SHELLS.contains(&shell)),
        None => Ok(true),
    }
}

// How a program the system runs itself starts: ELF, then Mach-O's thin and
// fat headers, each byte order.
const BINARIES: [&[u8]; 6] = [
    b"\x7fELF",
    b"\xfe\xed\xfa\xce",
    b"\xfe\xed\xfa\xcf",
    b"\xce\xfa\xed\xfe",
    b"\xcf\xfa\xed\xfe",
    b"\xca\xfe\xba\xbe",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn w(words: &str) -> Vec<String> {
        words.split(' ').map(str::to_owned).collect()
    }

    #[test]
    fn source_runs_its_file() {
        assert_eq!(sourced(&w("source push.sh")), Some("push.sh"));
        assert_eq!(sourced(&w(". ./push.sh x")), Some("./push.sh"));
        assert_eq!(sourced(&w("source -- push.sh")), Some("push.sh"));
        assert_eq!(sourced(&w("source")), None);
    }
}
