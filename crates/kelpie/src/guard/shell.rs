//! The commands one Bash call runs, as the guard reads them
//!
//! Separators inside quotes are data, and a `$( )`, `( )` or backtick body
//! is a command list of its own. A heredoc body is data too: it is lifted
//! out before the split and kept with each command whose text names it, so
//! `gh pr create --body "$(cat <<'EOF' ... EOF)"` carries its body.
//! This reads commands the way a shell would for the guard's purposes. It
//! is not a shell, and it does not expand anything.

/// One command a Bash call runs
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Command {
    /// Its words with quotes removed, after any keyword or assignment in front
    pub words: Vec<String>,
    /// The heredoc bodies its text reads
    pub heredocs: Vec<String>,
}

// A heredoc's place in the lifted text: `<<`, then its index between these.
const MARK: char = '\u{0}';

// Words a command can hide behind: `if gh pr create`, `env git commit`.
const LEADS: [&str; 18] = [
    "if", "while", "until", "do", "then", "else", "elif", "time", "command", "builtin", "exec",
    "env", "nohup", "nice", "setsid", "!", "{", "}",
];

/// Every command in `line`, a nested body's before the command around it
pub(super) fn commands(line: &str) -> Vec<Command> {
    let (text, bodies) = lift_heredocs(line);
    let chars: Vec<char> = text.chars().collect();
    split(&chars)
        .into_iter()
        .filter_map(|segment| {
            // A word keeps its `<<`, not the mark after it.
            let words: Vec<String> = words(&segment)
                .into_iter()
                .map(|w| w.split(MARK).step_by(2).collect())
                .collect();
            let words = lead_stripped(words);
            (!words.is_empty()).then(|| Command {
                heredocs: marks(&segment)
                    .into_iter()
                    .filter_map(|i| bodies.get(i).cloned())
                    .collect(),
                words,
            })
        })
        .collect()
}

// Where a scan stands: inside quotes, or in a command list.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Context {
    Single,
    Double,
    // A `$( )` or `( )` body starting at this index.
    Sub(usize),
    Backtick(usize),
}

// Replaces each heredoc body with a mark naming it, and returns the bodies.
fn lift_heredocs(line: &str) -> (String, Vec<String>) {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut bodies = Vec::new();
    let mut pending: Vec<(String, bool)> = Vec::new();
    let mut stack: Vec<char> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        let top = stack.last().copied();
        if top == Some('\'') {
            if ch == '\'' {
                stack.pop();
            }
        } else if ch == '\\' {
            out.push(ch);
            i += 1;
            if let Some(&next) = chars.get(i) {
                out.push(next);
            }
            i += 1;
            continue;
        } else if top == Some('"') && ch == '"' {
            stack.pop();
        } else if ch == '$' && chars.get(i + 1) == Some(&'(') {
            stack.push('(');
            out.push_str("$(");
            i += 2;
            continue;
        } else if top != Some('"') {
            match ch {
                // A comment's apostrophes are not quotes, as in `split`.
                '#' if i == 0 || chars[i - 1].is_whitespace() => {
                    let end = chars[i..]
                        .iter()
                        .position(|c| *c == '\n')
                        .map_or(chars.len(), |n| i + n);
                    out.extend(&chars[i..end]);
                    i = end;
                    continue;
                }
                '\'' | '"' => stack.push(ch),
                '(' => stack.push('('),
                ')' if top == Some('(') => {
                    stack.pop();
                }
                '<' if chars.get(i + 1) == Some(&'<') && chars.get(i + 2) != Some(&'<') => {
                    if let Some((delimiter, strip, end)) = delimiter(&chars, i + 2) {
                        out.push_str(&format!("<<{MARK}{}{MARK}", bodies.len() + pending.len()));
                        pending.push((delimiter, strip));
                        i = end;
                        continue;
                    }
                }
                '\n' if !pending.is_empty() => {
                    out.push('\n');
                    i += 1;
                    for (delimiter, strip) in pending.drain(..) {
                        let (body, next) = body(&chars, i, &delimiter, strip);
                        bodies.push(body);
                        i = next;
                    }
                    continue;
                }
                _ => {}
            }
        }
        out.push(ch);
        i += 1;
    }
    // A body with no newline before the end is still read to the end.
    for (delimiter, strip) in pending.drain(..) {
        let (body, _) = body(&chars, chars.len(), &delimiter, strip);
        bodies.push(body);
    }
    (out, bodies)
}

// The delimiter after `<<` at `from`, whether `<<-` strips tabs, and where it ends.
fn delimiter(chars: &[char], from: usize) -> Option<(String, bool, usize)> {
    let mut i = from;
    let strip = chars.get(i) == Some(&'-');
    if strip {
        i += 1;
    }
    while chars.get(i).is_some_and(|c| *c == ' ' || *c == '\t') {
        i += 1;
    }
    let quote = chars.get(i).copied().filter(|c| *c == '\'' || *c == '"');
    if quote.is_some() {
        i += 1;
    }
    let start = i;
    while chars
        .get(i)
        .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
    {
        i += 1;
    }
    if i == start {
        return None;
    }
    let word: String = chars[start..i].iter().collect();
    if let Some(q) = quote {
        if chars.get(i) != Some(&q) {
            return None;
        }
        i += 1;
    }
    Some((word, strip, i))
}

// The body's lines from `from` up to the delimiter's line, and where the text resumes.
fn body(chars: &[char], from: usize, delimiter: &str, strip: bool) -> (String, usize) {
    let mut lines = Vec::new();
    let mut i = from;
    while i < chars.len() {
        let end = chars[i..]
            .iter()
            .position(|c| *c == '\n')
            .map_or(chars.len(), |n| i + n);
        let line: String = chars[i..end].iter().collect();
        let next = (end + 1).min(chars.len());
        let bare = if strip {
            line.trim_start_matches('\t')
        } else {
            &line
        };
        if bare.trim_end() == delimiter {
            return (lines.join("\n"), next);
        }
        lines.push(line);
        i = next;
    }
    (lines.join("\n"), chars.len())
}

// The command segments of `chars`, each with its full text, nested bodies too.
fn split(chars: &[char]) -> Vec<Vec<char>> {
    let mut out = Vec::new();
    let mut stack: Vec<Context> = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        let top = stack.last().copied();
        if top == Some(Context::Single) {
            if ch == '\'' {
                stack.pop();
            }
            i += 1;
            continue;
        }
        if ch == '\\' {
            i += 2;
            continue;
        }
        if ch == '$' && chars.get(i + 1) == Some(&'(') {
            stack.push(Context::Sub(i + 2));
            i += 2;
            continue;
        }
        if ch == '`' {
            if let Some(Context::Backtick(from)) = top {
                stack.pop();
                out.extend(split(&chars[from..i]));
            } else {
                stack.push(Context::Backtick(i + 1));
            }
            i += 1;
            continue;
        }
        if top == Some(Context::Double) {
            if ch == '"' {
                stack.pop();
            }
            i += 1;
            continue;
        }
        let at_word = i == 0 || chars[i - 1].is_whitespace();
        match ch {
            // A comment runs to the newline, and its apostrophes are not quotes.
            '#' if at_word => {
                i = chars[i..]
                    .iter()
                    .position(|c| *c == '\n')
                    .map_or(chars.len(), |n| i + n);
                continue;
            }
            '\'' => stack.push(Context::Single),
            '"' => stack.push(Context::Double),
            '(' => stack.push(Context::Sub(i + 1)),
            ')' => {
                if let Some(Context::Sub(from)) = top {
                    stack.pop();
                    out.extend(split(&chars[from..i]));
                }
            }
            '\n' | ';' | '&' | '|' if stack.is_empty() => {
                out.push(chars[start..i].to_vec());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(chars[start..].to_vec());
    out
}

// The heredoc indexes a segment's text names.
fn marks(segment: &[char]) -> Vec<usize> {
    let text: String = segment.iter().collect();
    text.split(MARK)
        .skip(1)
        .step_by(2)
        .filter_map(|n| n.parse().ok())
        .collect()
}

// A segment's words, with quotes removed. A `$( )` stays whole inside its word.
fn words(segment: &[char]) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut i = 0;
    while i < segment.len() {
        let ch = segment[i];
        match ch {
            // A comment runs to the segment's end, a newline.
            '#' if !started => break,
            c if c.is_whitespace() || c == '(' || c == ')' => {
                if started {
                    out.push(std::mem::take(&mut word));
                    started = false;
                }
                i += 1;
                continue;
            }
            '\'' => {
                let end = segment[i + 1..]
                    .iter()
                    .position(|c| *c == '\'')
                    .map_or(segment.len(), |n| i + 1 + n);
                word.extend(&segment[i + 1..end]);
                i = end + 1;
            }
            '"' => {
                i += 1;
                while i < segment.len() && segment[i] != '"' {
                    if segment[i] == '\\' && i + 1 < segment.len() {
                        i += 1;
                    }
                    word.push(segment[i]);
                    i += 1;
                }
                i += 1;
            }
            '\\' => {
                if let Some(&next) = segment.get(i + 1) {
                    word.push(next);
                }
                i += 2;
            }
            _ => {
                word.push(ch);
                i += 1;
            }
        }
        started = true;
    }
    if started {
        out.push(word);
    }
    out
}

// Drops the keywords and `NAME=value` assignments in front of a command.
fn lead_stripped(mut words: Vec<String>) -> Vec<String> {
    let lead = words
        .iter()
        .take_while(|w| LEADS.contains(&w.as_str()) || is_assignment(w))
        .count();
    words.drain(..lead);
    words
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words_of(line: &str) -> Vec<Vec<String>> {
        commands(line).into_iter().map(|c| c.words).collect()
    }

    fn w(words: &[&str]) -> Vec<String> {
        words.iter().map(|&s| s.to_owned()).collect()
    }

    #[test]
    fn separators_split_commands_and_quotes_keep_them_whole() {
        assert_eq!(
            words_of("cd sub && git commit -m 'a; b | c' || echo \"x && y\"\ngh pr view"),
            [
                w(&["cd", "sub"]),
                w(&["git", "commit", "-m", "a; b | c"]),
                w(&["echo", "x && y"]),
                w(&["gh", "pr", "view"]),
            ]
        );
    }

    #[test]
    fn a_command_behind_a_keyword_or_an_assignment_is_found() {
        for line in [
            "if gh pr create -t x; then echo; fi",
            "for i in 1; do gh pr create -t x; done",
            "GH_PAGER= gh pr create -t x",
            "x=$(gh pr create -t x)",
            "echo `gh pr create -t x`",
            "(gh pr create -t x)",
            "{ gh pr create -t x; }",
        ] {
            assert!(
                words_of(line).contains(&w(&["gh", "pr", "create", "-t", "x"])),
                "{line}: {:?}",
                words_of(line)
            );
        }
    }

    #[test]
    fn a_command_inside_quotes_or_a_comment_runs_nothing() {
        for line in [
            "grep 'a|gh pr create' file",
            "echo \"; gh pr create\"",
            "echo hi # don't gh pr create",
        ] {
            assert!(
                !words_of(line)
                    .iter()
                    .any(|c| c.first().is_some_and(|w| w == "gh")),
                "{line}: {:?}",
                words_of(line)
            );
        }
    }

    #[test]
    fn a_heredoc_body_is_data_and_goes_with_the_command_that_reads_it() {
        let line =
            "git commit -F - <<'EOF'\nfix: x\n\ngh pr create; it runs nothing\nEOF\ngit push";
        let all = commands(line);
        assert_eq!(all.len(), 2, "{all:?}");
        assert_eq!(all[0].words, w(&["git", "commit", "-F", "-", "<<"]));
        assert_eq!(all[0].heredocs, ["fix: x\n\ngh pr create; it runs nothing"]);
        assert_eq!(all[1].words, w(&["git", "push"]));
        assert!(all[1].heredocs.is_empty());
    }

    #[test]
    fn a_heredoc_inside_a_quoted_substitution_goes_with_the_outer_command() {
        let line = "gh pr create --title 'fix: x' --body \"$(cat <<'EOF'\nthe body\nEOF\n)\"";
        let all = commands(line);
        let gh = all.iter().find(|c| c.words[0] == "gh").unwrap();
        assert_eq!(gh.words[..4], w(&["gh", "pr", "create", "--title"]));
        assert_eq!(gh.heredocs, ["the body"]);
    }

    #[test]
    fn a_heredoc_after_a_comment_with_an_apostrophe_is_still_lifted() {
        let all = commands("# don't push yet\ngit commit -F - <<EOF\nfix: x\nEOF\ngit push");
        assert_eq!(all[0].heredocs, ["fix: x"], "{all:?}");
        assert_eq!(all[1].words, w(&["git", "push"]));
    }

    #[test]
    fn a_heredoc_marker_in_quotes_is_text() {
        let all = commands("echo \"a << b\"\ngit push");
        assert_eq!(all.len(), 2, "{all:?}");
        assert!(all.iter().all(|c| c.heredocs.is_empty()));
    }

    #[test]
    fn a_tab_stripping_heredoc_ends_at_an_indented_delimiter() {
        let all = commands("cat <<-END\n\tone\n\tEND\ngit push");
        assert_eq!(all[0].heredocs, ["\tone"]);
        assert_eq!(all[1].words, w(&["git", "push"]));
    }
}
