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
    let mut kept: Vec<String> = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    let mut indented = false;
    let mut in_list = false;
    let mut in_table = false;
    let mut blank_before = true;
    let mut joins = false;
    let mut rest = body.lines().peekable();

    while let Some(line) = rest.next() {
        if let Some((marker, least)) = fence {
            kept.push(line.to_owned());
            if closes_fence(line, marker, least) {
                fence = None;
            }
            joins = false;
            continue;
        }
        if indented && !blank(line) && columns(line) >= CODE_INDENT {
            kept.push(line.to_owned());
            joins = false;
            continue;
        }
        indented = false;
        if blank(line) {
            kept.push(line.to_owned());
            joins = false;
            in_table = false;
            blank_before = true;
            continue;
        }
        if let Some(open) = opens_fence(line) {
            fence = Some(open);
            kept.push(line.to_owned());
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
            kept.push(line.to_owned());
            joins = false;
            continue;
        }
        let row = starts_table(line, in_table, rest.peek().copied());
        if joins && !starts_block(line) && !row {
            // `joins` is only set where a line was kept, so one is always there
            if let Some(last) = kept.last_mut() {
                *last = format!("{} {}", last.trim_end(), line.trim_start());
            }
        } else {
            kept.push(line.to_owned());
        }
        joins = !(row || ends_block(line));
        in_table = row;
        blank_before = false;
    }
    kept.join("\n")
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

/// `---`, `===`, `***` or `___`: three or more of one of them and nothing else
///
/// A run of `=` or `-` under a paragraph is also that paragraph's setext
/// heading underline, which ends a line all the same.
fn starts_rule(line: &str) -> bool {
    let rest = line.trim();
    let Some(marker) = rest
        .chars()
        .next()
        .filter(|c| matches!(c, '-' | '=' | '*' | '_'))
    else {
        return false;
    };
    rest.chars().count() >= 3 && rest.chars().all(|c| c == marker)
}

/// A table row that names its columns with a leading pipe
fn starts_piped(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

/// A GFM table's delimiter row: `--- | ---`, `| :--- | ---: |` and the like
fn is_delimiter(line: &str) -> bool {
    let rest = line.trim().trim_matches('|');
    rest.contains('|') && rest.split('|').all(aligned)
}

/// A delimiter row's cell: dashes with an optional alignment colon at each end
fn aligned(cell: &str) -> bool {
    let cell = cell.trim().trim_matches(':');
    !cell.is_empty() && cell.chars().all(|c| c == '-')
}

/// A GFM table row, with or without leading pipes: a leading pipe, a delimiter
/// row, a row of a table already started, or a header row whose delimiter row
/// is the line after it
fn starts_table(line: &str, in_table: bool, next: Option<&str>) -> bool {
    starts_piped(line)
        || is_delimiter(line)
        || (in_table && line.contains('|'))
        || (line.contains('|') && next.is_some_and(is_delimiter))
}

/// A line that starts a block of its own, so it never joins onto the line before
fn starts_block(line: &str) -> bool {
    starts_item(line)
        || starts_heading(line)
        || starts_rule(line)
        || line.trim_start().starts_with(['>', '<'])
}

/// A line nothing joins onto: a heading, a thematic break, an HTML line, or a
/// hard break, which is a line ending in two spaces or a backslash
///
/// A blockquote is left out on purpose: a following line is its lazy
/// continuation, and joins into the quote as GitHub renders it.
fn ends_block(line: &str) -> bool {
    starts_heading(line)
        || starts_rule(line)
        || line.trim_start().starts_with('<')
        || hard_break(line)
}

/// A Markdown hard break: a line ending in two spaces or a backslash
fn hard_break(line: &str) -> bool {
    line.ends_with("  ") || line.ends_with('\\')
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
