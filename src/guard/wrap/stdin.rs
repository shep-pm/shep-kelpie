//! Where a shell reads its commands: its own text, stdin, or a file
//!
//! `-c` and a `<<<` string put the commands in the call's text. With
//! neither, a shell given no script file, or `-s`, or stdin redirected in
//! before its script file, reads stdin, which only a heredoc in the call's
//! text would hold.

/// What a shell runs, as its words give it
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::guard) enum Shell<'a> {
    /// Commands in the call's own text: its `-c` script, or its `<<<` strings
    Text(Vec<&'a str>),
    /// Commands on stdin, which only a heredoc in the call's text would hold
    Stdin,
    /// A script file, which the guard does not read, or nothing
    Unread,
}

/// What the shell `words` starts runs
pub(in crate::guard) fn shell(words: &[String]) -> Shell<'_> {
    if let Some(script) = script(words) {
        return Shell::Text(vec![script]);
    }
    let mut strings = Vec::new();
    let mut stdin = false;
    let mut operand = None;
    let mut options = true;
    let mut i = 1;
    while let Some(word) = words.get(i) {
        i += 1;
        if let Some(text) = herestring(word) {
            if text.is_empty() {
                strings.extend(words.get(i).map(String::as_str));
                i += 1;
            } else {
                strings.push(text);
            }
            continue;
        }
        if let Some(redirect) = redirection(word) {
            // Stdin redirected in before any script file holds the commands.
            stdin |= redirect.stdin && operand.is_none();
            i += usize::from(redirect.separate);
            continue;
        }
        if operand.is_some() {
            continue;
        }
        if options && word.starts_with(['-', '+']) && word != "-" {
            match word.as_str() {
                "--" => options = false,
                "--version" | "--help" => return Shell::Unread,
                "-o" | "+o" | "-O" | "+O" | "--rcfile" | "--init-file" => i += 1,
                w if !w.starts_with("--") && w.contains('s') => stdin = true,
                _ => {}
            }
            continue;
        }
        operand = Some(word.as_str());
    }
    match operand {
        Some(file) if !stdin && !stdin_path(file) => Shell::Unread,
        _ if strings.is_empty() => Shell::Stdin,
        _ => Shell::Text(strings),
    }
}

/// The script a shell runs with `-c`, `-lc` and the like
fn script(words: &[String]) -> Option<&str> {
    let at = words[1..].iter().position(|w| is_c_flag(w))?;
    words.get(at + 2).map(String::as_str)
}

pub(super) fn is_c_flag(word: &str) -> bool {
    word.starts_with('-') && !word.starts_with("--") && word.contains('c')
}

// A `<<<` string's text in the word, or `""` when it is the next word.
fn herestring(word: &str) -> Option<&str> {
    word.trim_start_matches(|c: char| c.is_ascii_digit())
        .strip_prefix("<<<")
}

// A path that is the command's own stdin.
pub(super) fn stdin_path(word: &str) -> bool {
    word == "-" || word == "/dev/stdin" || word.starts_with("/dev/fd/")
}

// A redirection word, as the guard's reader leaves it.
#[derive(Debug, Clone, Copy)]
pub(super) struct Redirect {
    // Whether it feeds the command's stdin.
    pub stdin: bool,
    // Whether its target is the next word.
    separate: bool,
}

// Longest first. A heredoc's `<<` has its body lifted out, so no target follows.
const REDIRECTS: [&str; 11] = [
    "<<-", "<<", "<>", "<&", "<", ">>", ">&", ">|", ">", "&>>", "&>",
];

pub(super) fn redirection(word: &str) -> Option<Redirect> {
    let op = word.trim_start_matches(|c: char| c.is_ascii_digit());
    let fd = &word[..word.len() - op.len()];
    let symbol = REDIRECTS.iter().find(|s| op.starts_with(**s))?;
    Some(Redirect {
        stdin: symbol.starts_with('<') && (fd.is_empty() || fd == "0"),
        separate: op.len() == symbol.len() && !symbol.starts_with("<<"),
    })
}
