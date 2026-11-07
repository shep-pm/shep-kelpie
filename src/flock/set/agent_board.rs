//! Takes kelpie's `[agent_board]` table out of its own section, line by line

/// The table the dog no longer reads
const TABLE: &str = "agent_board";

/// A section without its `[agent_board]` table, and what was in it
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Stripped {
    /// Every line but the table's, as the maintainer wrote them
    pub text: String,
    /// What it removed: the list with its value, or the bare table when it held none
    pub said: String,
}

/// `section` without its `[agent_board]` table, or None when it has none
///
/// The table runs from its header to the next header. Comment and blank lines
/// just before that header stay, as they belong to it. Every other line is
/// kept byte for byte.
///
/// # Errors
///
/// A message when the section does not parse, or when what is left does not
/// parse or is not the section less the table (a header-looking line inside a
/// string, or the table written inline).
pub(super) fn strip(section: &str) -> Result<Option<Stripped>, String> {
    let before: toml::Table =
        (section.parse()).map_err(|e| format!("the section does not parse: {e}"))?;
    let Some(held) = before.get(TABLE) else {
        return Ok(None);
    };
    let lines: Vec<&str> = section.split_inclusive('\n').collect();
    let text: String = kept(&lines).concat();
    let left: toml::Table = text.parse().map_err(|e| {
        format!(
            "the section does not parse with `[{TABLE}]` taken out, so it is left as it is: {e}"
        )
    })?;
    let mut expected = before.clone();
    expected.remove(TABLE);
    if left != expected {
        return Err(format!(
            "`[{TABLE}]` is not a table whose lines can be taken out alone, so the section is left as it is"
        ));
    }
    let said = match held.get("prefer") {
        Some(prefer) => format!("removed {TABLE}.prefer = {prefer}"),
        None => format!("removed {TABLE}"),
    };
    Ok(Some(Stripped { text, said }))
}

// The lines outside the table, with the idle lines before the next header put back.
fn kept<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    let mut inside = false;
    let mut dropped: Vec<bool> = lines
        .iter()
        .map(|line| {
            if let Some(name) = header(line) {
                inside = name == TABLE;
            }
            inside
        })
        .collect();
    let mut trailing = true;
    for (at, line) in lines.iter().enumerate().rev() {
        if !dropped[at] {
            trailing = true;
        } else if trailing && idle(line) {
            dropped[at] = false;
        } else {
            trailing = false;
        }
    }
    (lines.iter().zip(dropped))
        .filter_map(|(line, dropped)| (!dropped).then_some(*line))
        .collect()
}

// The first name of the table a header line opens, `agent_board` of `[agent_board.x]`.
fn header(line: &str) -> Option<&str> {
    let line = line.trim();
    let inner = line.strip_prefix("[[").or_else(|| line.strip_prefix('['))?;
    let close = inner.find(']')?;
    let rest = inner[close..].trim_start_matches(']').trim();
    if !(rest.is_empty() || rest.starts_with('#')) {
        return None;
    }
    let first = inner[..close].split('.').next()?;
    Some(first.trim().trim_matches(['"', '\'']))
}

// A blank line or one that holds only a comment.
fn idle(line: &str) -> bool {
    let line = line.trim();
    line.is_empty() || line.starts_with('#')
}
