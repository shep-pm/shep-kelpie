//! The one check for what must not leave this machine
//!
//! Kelpie's own forge posts and the guard on every worker's commits and `gh`
//! calls ask the same question of a text: does it name this machine? The
//! answer is [`LocalPaths::find`], so both refuse the same things, and never
//! echo what they matched.
//!
//! A text is read as written and again with its percent-encoding undone and
//! its backslashes made slashes, because a dev server's stack-frame URLs carry
//! `%2FUsers%2F...` and a Windows path carries `\`. Matching ignores case.
//! It finds:
//!
//! - a folder it was given, in its canonical form too, so a symlinked home
//!   names the same place twice
//! - a word from the project's own list of private names
//!
//! A [`Surface::Prose`] is text a person reads, so it also refuses
//! `/home/<name>`, `C:\Users\<name>`, `/private/tmp` and `/var/folders`,
//! which name a machine's user or scratch space whoever the user is, a path
//! under `~`, and an address on a local network. Code and its patches are
//! full of all of them (another user's home in a fixture, `~/` in a doc, `self.local`,
//! an address in a fixture), so a [`Surface::Code`] leaves them be and holds
//! only to the folders it was given and the private names.

use std::fmt;
use std::fs;
use std::path::Path;

// Rounds of percent-decoding, for text encoded more than once.
const DECODE_ROUNDS: usize = 3;

// Places a path starts at, whoever's home it is.
const SYSTEM_PREFIXES: [&str; 4] = [
    "/home/",
    "/private/tmp",
    "/private/var/folders",
    "/var/folders",
];

/// What a text is, which decides what counts as naming this machine
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// Something a person reads: a comment, an issue, a title, a commit message
    Prose,
    /// Something a program reads: the lines a commit adds
    Code,
}

/// What a text named
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leak {
    /// A path on this machine
    Path,
    /// A word from the project's list of private names
    Name,
    /// A path under the home folder, written `~/...`
    Tilde,
    /// An address on a local network
    Lan,
}

impl fmt::Display for Leak {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Path => "a path on this machine, which names its user",
            Self::Name => "a name on this project's private list",
            Self::Tilde => "a path under the home folder",
            Self::Lan => "an address on a local network",
        })
    }
}

/// What to keep off the forge: folders, and private names
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalPaths {
    folders: Vec<String>,
    names: Vec<String>,
}

impl LocalPaths {
    /// A check for `folders` and for `names`
    ///
    /// A folder with no parent, such as `/`, names nothing and is left out,
    /// as is a blank name.
    pub fn new<'a>(
        folders: impl IntoIterator<Item = &'a Path>,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let mut found: Vec<String> = Vec::new();
        for folder in folders.into_iter().filter(|f| f.parent().is_some()) {
            let canonical = fs::canonicalize(folder).ok();
            for path in std::iter::once(folder).chain(canonical.as_deref()) {
                let text = normalize(&path.to_string_lossy());
                let text = text.trim_end_matches('/');
                if !text.is_empty() && !found.iter().any(|f| f == text) {
                    found.push(text.to_owned());
                }
            }
        }
        let names = names
            .into_iter()
            .map(|n| n.trim().to_lowercase())
            .filter(|n| !n.is_empty())
            .collect();
        Self {
            folders: found,
            names,
        }
    }

    /// What `text` names that must stay on this machine, if anything
    pub fn find(&self, text: &str, surface: Surface) -> Option<Leak> {
        variants(text).iter().find_map(|v| self.find_in(v, surface))
    }

    fn find_in(&self, text: &str, surface: Surface) -> Option<Leak> {
        let system = surface == Surface::Prose && names_system_path(text);
        if system || self.folders.iter().any(|f| names_folder(text, f)) {
            return Some(Leak::Path);
        }
        if self.names.iter().any(|n| has_word(text, n)) {
            return Some(Leak::Name);
        }
        if surface == Surface::Prose {
            if has_tilde_path(text) {
                return Some(Leak::Tilde);
            }
            if has_lan(text) {
                return Some(Leak::Lan);
            }
        }
        None
    }
}

// Lowercase, backslashes as slashes, each run of slashes as one.
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.to_lowercase().chars() {
        let c = if c == '\\' { '/' } else { c };
        if !(c == '/' && out.ends_with('/')) {
            out.push(c);
        }
    }
    out
}

// The text as written, then with its percent-encoding undone, each form normalized.
fn variants(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = text.to_owned();
    for _ in 0..=DECODE_ROUNDS {
        let form = normalize(&current);
        if !out.contains(&form) {
            out.push(form);
        }
        let decoded = percent_decode(&current);
        if decoded == current {
            break;
        }
        current = decoded;
    }
    out
}

fn percent_decode(text: &str) -> String {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let Some((&b, tail)) = rest.split_first() {
        let hex = match (b, tail) {
            (b'%', [hi, lo, ..]) if hi.is_ascii_hexdigit() && lo.is_ascii_hexdigit() => {
                u8::from_str_radix(std::str::from_utf8(&[*hi, *lo]).unwrap_or("zz"), 16).ok()
            }
            _ => None,
        };
        match hex {
            Some(byte) => {
                bytes.push(byte);
                rest = &tail[2..];
            }
            None => {
                bytes.push(b);
                rest = tail;
            }
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

// Whether a path may start right after `before`: not in the middle of a word,
// a host name, or another path's name.
fn starts_path(before: Option<char>) -> bool {
    before.is_none_or(|c| !(is_word(c) || matches!(c, '.' | '-' | '~')))
}

fn before(text: &str, at: usize) -> Option<char> {
    text[..at].chars().next_back()
}

// `folder`, or a path under it; not a longer name that starts with it.
fn names_folder(text: &str, folder: &str) -> bool {
    text.match_indices(folder).any(|(at, _)| {
        text[at + folder.len()..]
            .chars()
            .next()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '-'))
    })
}

fn names_system_path(text: &str) -> bool {
    for prefix in SYSTEM_PREFIXES {
        for (at, _) in text.match_indices(prefix) {
            let after = text[at + prefix.len()..].chars().next();
            // `/home/` needs a name after it; the rest end at their own name.
            let ends = if prefix.ends_with('/') {
                after.is_some_and(is_word)
            } else {
                after.is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '-'))
            };
            if ends && starts_path(before(text, at)) {
                return true;
            }
        }
    }
    // `c:/users/<name>`
    text.match_indices(":/users/").any(|(at, _)| {
        let drive = before(text, at).is_some_and(|c| c.is_ascii_alphabetic());
        let name = text[at + ":/users/".len()..]
            .chars()
            .next()
            .is_some_and(is_word);
        let start = text[..at].chars().rev().nth(1);
        drive && name && start.is_none_or(|c| !is_word(c))
    })
}

// `word` between characters that are not letters or digits.
fn has_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
        edge(before(text, at)) && edge(text[at + word.len()..].chars().next())
    })
}

fn has_tilde_path(text: &str) -> bool {
    text.match_indices("~/")
        .any(|(at, _)| starts_path(before(text, at)))
}

fn has_lan(text: &str) -> bool {
    has_private_ipv4(text) || has_local_host(text)
}

// 10/8, 172.16/12, 192.168/16 and 169.254/16, as four dotted numbers.
fn has_private_ipv4(text: &str) -> bool {
    let mut start = None;
    let mut spans = Vec::new();
    for (i, c) in text.char_indices() {
        match (c.is_ascii_digit() || c == '.', start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                spans.push((s, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        spans.push((s, text.len()));
    }
    spans.into_iter().any(|(s, e)| {
        // A sentence's full stop is not part of an address.
        let run = text[s..e].trim_end_matches('.');
        let edge_before = before(text, s).is_none_or(|c| !(is_word(c) || c == '-'));
        let octets: Vec<Option<u8>> = run
            .split('.')
            .map(|o| {
                (!o.is_empty() && o.len() <= 3)
                    .then(|| o.parse().ok())
                    .flatten()
            })
            .collect();
        let [Some(a), Some(b), Some(_), Some(_)] = octets[..] else {
            return false;
        };
        let edge_after = text[s + run.len()..]
            .chars()
            .next()
            .is_none_or(|c| !(is_word(c) || c == '-'));
        edge_before
            && edge_after
            && (a == 10
                || (a == 172 && (16..32).contains(&b))
                || (a == 192 && b == 168)
                || (a == 169 && b == 254))
    })
}

// `name.local` as a host: after `://` or `@`, or with a port or a path. The
// same shape in code, `self.local.iter()`, is a field and stays.
fn has_local_host(text: &str) -> bool {
    text.match_indices(".local").any(|(at, _)| {
        let label = text[..at]
            .char_indices()
            .rev()
            .find(|&(_, c)| !(c.is_ascii_alphanumeric() || c == '-' || c == '.'))
            .map_or(0, |(i, c)| i + c.len_utf8());
        if label == at {
            return false;
        }
        let rest = &text[at + ".local".len()..];
        let next = rest.chars().next();
        let longer = next.is_some_and(|c| is_word(c) || c == '-')
            || rest
                .strip_prefix('.')
                .is_some_and(|r| r.starts_with(is_word));
        if longer {
            return false;
        }
        let named = text[..label].ends_with(":/") || text[..label].ends_with('@');
        let addressed = rest.starts_with('/')
            || rest
                .strip_prefix(':')
                .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()));
        named || addressed
    })
}

#[cfg(test)]
mod tests;
