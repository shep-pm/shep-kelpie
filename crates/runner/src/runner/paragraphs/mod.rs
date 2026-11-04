//! The lines of a pull request body, joined so each paragraph is one line
//!
//! GitHub renders every newline in a body as a line break, so a body a worker
//! hard-wraps at 80 columns shows as a ragged narrow column.
//!
//! A line starts a line of its own when it is blank, follows a blank line, or
//! starts a list item, heading, blockquote, table row, HTML line or thematic
//! break. A line after a heading, table row, thematic break, hard break or
//! code does too. A fenced or indented code block, its fence lines included,
//! is kept as written.

/// A line indented this much after a blank line, outside a list, is code
const CODE_INDENT: usize = 4;

/// Join `body`'s soft-wrapped lines onto the line before each
///
/// A continuation keeps no indent of its own: its leading whitespace goes, and
/// one space joins it on. A body already written one line per paragraph comes
/// back as it went in.
pub(super) fn join(body: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    let mut indented = false;
    let mut in_list = false;
    let mut blank_before = true;
    let mut joins = false;

    for line in body.lines() {
        if let Some((marker, least)) = fence {
            lines.push(line.to_owned());
            if closes_fence(line, marker, least) {
                fence = None;
            }
            joins = false;
            continue;
        }
        if indented && !blank(line) && columns(line) >= CODE_INDENT {
            lines.push(line.to_owned());
            joins = false;
            continue;
        }
        indented = false;
        if blank(line) {
            lines.push(line.to_owned());
            joins = false;
            blank_before = true;
            continue;
        }
        if let Some(open) = opens_fence(line) {
            fence = Some(open);
            lines.push(line.to_owned());
            joins = false;
            continue;
        }
        let item = starts_item(line);
        in_list = match (item, columns(line)) {
            (true, _) => true,
            (false, 0) => false,
            _ => in_list,
        };
        if !item && !in_list && blank_before && columns(line) >= CODE_INDENT {
            indented = true;
            lines.push(line.to_owned());
            joins = false;
            continue;
        }
        if joins && !starts_block(line) {
            // `joins` is only set where a line was kept, so one is always there
            if let Some(last) = lines.last_mut() {
                *last = format!("{} {}", last.trim_end(), line.trim_start());
            }
        } else {
            lines.push(line.to_owned());
        }
        joins = !ends_block(line);
        blank_before = false;
    }
    lines.join("\n")
}

/// A line with nothing but spaces on it
fn blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// A line's leading whitespace, a tab counting as four columns
fn columns(line: &str) -> usize {
    let mut columns = 0;
    for c in line.chars() {
        match c {
            ' ' => columns += 1,
            '\t' => columns += 4,
            _ => break,
        }
    }
    columns
}

/// `- `, `* `, `+ `, or digits then `. ` or `) `, after any indent
///
/// `--no-deps` and `+478` start no item: the marker needs a space after it.
fn starts_item(line: &str) -> bool {
    let rest = line.trim_start();
    if rest.starts_with(['-', '*', '+']) {
        return rest[1..].starts_with(' ');
    }
    let digits = (rest.chars()).take_while(char::is_ascii_digit).count();
    digits > 0 && (rest[digits..].starts_with(". ") || rest[digits..].starts_with(") "))
}

/// `#` to `######` then a space or the line's end
///
/// `#1498` starts no heading: the marks need a space after them.
fn starts_heading(line: &str) -> bool {
    let rest = line.trim_start();
    let marks = (rest.chars()).take_while(|c| *c == '#').count();
    if !(1..=6).contains(&marks) {
        return false;
    }
    rest[marks..].is_empty() || rest[marks..].starts_with(' ')
}

/// `---`, `***` or `___`: three or more of one of them and nothing else
fn starts_rule(line: &str) -> bool {
    let rest = line.trim();
    let Some(marker) = rest.chars().next().filter(|c| matches!(c, '-' | '*' | '_')) else {
        return false;
    };
    rest.chars().count() >= 3 && rest.chars().all(|c| c == marker)
}

/// A table row, a blockquote or an HTML line
fn starts_row(line: &str) -> bool {
    line.trim_start().starts_with(['|', '>', '<'])
}

/// A line that starts a block of its own, so it never joins onto the line before
fn starts_block(line: &str) -> bool {
    starts_item(line) || starts_heading(line) || starts_rule(line) || starts_row(line)
}

/// A line nothing joins onto: a heading, a table row, a thematic break, or a
/// hard break, which is a line ending in two spaces or a backslash
fn ends_block(line: &str) -> bool {
    starts_heading(line)
        || line.trim_start().starts_with('|')
        || starts_rule(line)
        || line.ends_with("  ")
        || line.ends_with('\\')
}

/// A fence's opening: three or more backticks or tildes and any info string after
fn opens_fence(line: &str) -> Option<(char, usize)> {
    let rest = line.trim_start();
    let marker = rest.chars().next()?;
    if marker != '`' && marker != '~' {
        return None;
    }
    let marks = (rest.chars()).take_while(|c| *c == marker).count();
    (marks >= 3).then_some((marker, marks))
}

/// A fence's closing: the marker alone, at least as many of it as the opening had
fn closes_fence(line: &str, marker: char, least: usize) -> bool {
    let rest = line.trim();
    let marks = (rest.chars()).take_while(|c| *c == marker).count();
    marks >= least && marks == rest.chars().count()
}

#[cfg(test)]
mod tests;
