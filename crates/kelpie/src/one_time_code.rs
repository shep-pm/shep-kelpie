//! A ruling's one-time code, which a reply on the webhook's topic must
//! carry to answer it

use std::fmt;

use serde::{Deserialize, Serialize};

/// A ruling's one-time code: what makes a reply on the webhook's topic
/// answer that ruling and no other
///
/// Forty random bits, as eight characters a phone keyboard types without
/// ambiguity. `Debug` does not show it.
// wire format: changing this is a breaking change to the state file
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OneTimeCode(String);

impl OneTimeCode {
    /// Lower-case Crockford base32: no `i`, `l`, `o` or `u`
    const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

    /// A fresh code from the OS's randomness
    ///
    /// # Errors
    ///
    /// The OS's error when `/dev/urandom` cannot be read.
    pub fn draw() -> std::io::Result<Self> {
        use std::io::Read;
        let mut bytes = [0u8; 5];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        let bits = bytes.iter().fold(0u64, |n, &b| n << 8 | u64::from(b));
        let code = (0..8)
            .rev()
            .map(|i| char::from(Self::ALPHABET[(bits >> (i * 5) & 31) as usize]))
            .collect();
        Ok(Self(code))
    }

    /// The code, for the alert that carries it and nothing else
    #[inline]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether `typed` is this code, ignoring case, in time that does not
    /// depend on where they differ
    pub fn matches(&self, typed: &str) -> bool {
        let (ours, typed) = (self.0.as_bytes(), typed.as_bytes());
        ours.len() == typed.len()
            && ours
                .iter()
                .zip(typed)
                .fold(0u8, |diff, (a, b)| diff | (a ^ b.to_ascii_lowercase()))
                == 0
    }
}

impl fmt::Debug for OneTimeCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OneTimeCode(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_is_eight_unambiguous_characters_and_never_repeats() {
        let codes: std::collections::BTreeSet<_> = (0..500)
            .map(|_| OneTimeCode::draw().unwrap().expose().to_owned())
            .collect();
        assert_eq!(codes.len(), 500);
        for code in &codes {
            assert_eq!(code.len(), 8, "{code}");
            assert!(
                code.bytes().all(|b| OneTimeCode::ALPHABET.contains(&b)),
                "{code}"
            );
        }
        // Every character turns up, so no bits are dropped.
        let seen: std::collections::BTreeSet<_> = codes.iter().flat_map(|c| c.bytes()).collect();
        assert_eq!(seen.len(), 32);
    }

    #[test]
    fn a_code_matches_only_itself_in_any_case() {
        let code = OneTimeCode("7hq2mx9d".into());
        assert!(code.matches("7hq2mx9d"));
        assert!(code.matches("7HQ2MX9D"));
        for other in ["7hq2mx9", "7hq2mx9dd", "7hq2mx9e", "", "7hq2mx9d "] {
            assert!(!code.matches(other), "{other:?}");
        }
        assert_eq!(format!("{code:?}"), "OneTimeCode(..)");
    }
}
