//! The project manager's answer to a ruling
//!
//! The project manager answers a ruling with one JSON object, `{"choice":
//! "<option key>", "why": "<at most 350 characters>", "design":
//! true|false}`, with an optional `"ask": true|false`. An answer kelpie
//! cannot read is an abstention, and the ruling goes to the maintainer.

use serde_json::{Map, Value};

/// The most an answer's `why` keeps, in characters: four lines of a GitHub comment
pub const WHY_LONGEST: usize = 350;

/// The project manager's answer to one ruling
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PmAnswer {
    /// The option's key, `A` for the first
    pub choice: char,
    /// Why, in one line of at most [`WHY_LONGEST`] characters
    pub why: String,
    /// Whether the question changes an ADR, the glossary, a public interface
    /// or the project's direction
    pub design: bool,
    /// Whether a resume of a timed-out or failed turn first asks the worker
    /// what it has done so far
    pub ask: bool,
}

/// The answer in `text`, which holds options `A` to the `options`th letter
///
/// `text` is the project manager's reply: the last JSON object in it with
/// a `choice` is the answer. One that is not an object with all three
/// fields (`ask` may be left out), or whose choice is no option's key, is an error that says why,
/// for the log.
/// A `why` past [`WHY_LONGEST`] characters is cut there.
///
/// # Errors
///
/// Why the reply holds no answer kelpie reads.
pub fn read(text: &str, options: usize) -> Result<PmAnswer, String> {
    let fields = last_object(text).ok_or("its reply held no JSON object with a `choice`")?;
    from_fields(&fields, options)
}

/// The answer the `fields` of a JSON object make, as [`read`] has it
///
/// # Errors
///
/// Why the object is no answer kelpie reads.
pub fn from_fields(fields: &Map<String, Value>, options: usize) -> Result<PmAnswer, String> {
    let choice = match fields.get("choice") {
        Some(Value::String(key)) => key.trim().to_ascii_uppercase(),
        other => return Err(format!("`choice` is not a string: {other:?}")),
    };
    let mut keys = choice.chars();
    let (Some(key), None) = (keys.next(), keys.next()) else {
        return Err(format!("`choice` is `{choice}`, not one option's key"));
    };
    let valid = ('A'..='Z').take(options).any(|k| k == key);
    if !valid {
        return Err(format!(
            "`choice` is `{key}`, and there are {options} options"
        ));
    }
    let why = match fields.get("why") {
        Some(Value::String(why)) => oneline(why),
        other => return Err(format!("`why` is not a string: {other:?}")),
    };
    let Some(Value::Bool(design)) = fields.get("design") else {
        return Err(format!(
            "`design` is not true or false: {:?}",
            fields.get("design")
        ));
    };
    let ask = match fields.get("ask") {
        None => false,
        Some(Value::Bool(ask)) => *ask,
        other => return Err(format!("`ask` is not true or false: {other:?}")),
    };
    Ok(PmAnswer {
        choice: key,
        why,
        design: *design,
        ask,
    })
}

// The last top-level object in `text` that has a `choice`. Read from the
// start, each whole JSON value is skipped past, so nothing inside one, a
// string's braces included, is taken for an answer of its own.
fn last_object(text: &str) -> Option<Map<String, Value>> {
    let mut found = None;
    let mut at = 0;
    while let Some(offset) = text[at..].find('{') {
        let start = at + offset;
        let mut values = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
        match values.next() {
            Some(Ok(value)) => {
                at = start + values.byte_offset();
                if let Value::Object(fields) = value
                    && fields.contains_key("choice")
                {
                    found = Some(fields);
                }
            }
            _ => at = start + 1,
        }
    }
    found
}

// `why` on one line with no control character, cut past [`WHY_LONGEST`]
// characters, since it is posted as it is.
fn oneline(why: &str) -> String {
    let flat: Vec<&str> = why.split_whitespace().collect();
    let text: String = flat.join(" ").chars().filter(|c| !c.is_control()).collect();
    match text.chars().count() > WHY_LONGEST {
        true => {
            let mut cut: String = text.chars().take(WHY_LONGEST - 1).collect();
            cut.push('…');
            cut
        }
        false => text,
    }
}

#[cfg(test)]
mod tests;
