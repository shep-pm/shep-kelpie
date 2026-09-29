//! The authenticator code that proves an answer from ntfy is the maintainer's
//!
//! RFC 6238 TOTP: HMAC-SHA-1, 30-second steps, six digits, which every
//! authenticator app reads from an `otpauth://` URI. Kelpie draws the secret
//! once, keeps it under its home with only the owner able to read it, and
//! never posts it anywhere: `kelpie totp` shows it to the maintainer to scan.
//! Anyone who can read the ntfy topic sees each code the maintainer sends, so
//! a step answers once across every project: see [`answers`].

use std::fmt;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;

use crate::ports::Timestamp;

pub mod answers;

/// Seconds a code lasts
pub const STEP: u64 = 30;

/// RFC 4648 base32, which `otpauth://` URIs carry the secret in
const BASE32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// The shared secret: 160 random bits, the length RFC 4226 recommends
///
/// `Debug` does not show it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret([u8; 20]);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// Why the secret could not be read or made, naming the file and never
/// its contents
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretError {
    /// The file could not be read or written
    Io(PathBuf, io::ErrorKind),
    /// The file does not hold a secret kelpie wrote
    Malformed(PathBuf),
    /// Someone other than its owner may read or write the file
    Exposed(PathBuf),
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(path, kind) => write!(f, "cannot use {}: {kind}", path.display()),
            Self::Malformed(path) => write!(
                f,
                "{} is not an authenticator secret kelpie wrote: remove it and run `kelpie totp`",
                path.display()
            ),
            Self::Exposed(path) => write!(
                f,
                "{} may be read by others: `chmod 600` it, or run `kelpie totp --rotate` \
                 if someone else may have read it",
                path.display()
            ),
        }
    }
}

impl core::error::Error for SecretError {}

impl Secret {
    /// The secret in `path`, or `None` when there is no file
    ///
    /// # Errors
    ///
    /// [`SecretError`] when the file cannot be read, anyone but its owner
    /// may read or write it, or it is not base32 of 20 bytes.
    pub fn load(path: &Path) -> Result<Option<Self>, SecretError> {
        let io = |e: io::Error| SecretError::Io(path.to_owned(), e.kind());
        let mut file = match fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io(e)),
        };
        if file.metadata().map_err(io)?.mode() & 0o077 != 0 {
            return Err(SecretError::Exposed(path.to_owned()));
        }
        // Someone who can write the folder can put their own secret in it.
        let folder = path.parent().unwrap_or(Path::new("."));
        let folder_mode = fs::metadata(folder)
            .map_err(|e| SecretError::Io(folder.to_owned(), e.kind()))?
            .mode();
        if folder_mode & 0o077 != 0 {
            return Err(SecretError::Exposed(folder.to_owned()));
        }
        let mut text = String::new();
        file.read_to_string(&mut text).map_err(io)?;
        decode(text.trim())
            .map(|bytes| Some(Self(bytes)))
            .ok_or_else(|| SecretError::Malformed(path.to_owned()))
    }

    /// The secret in `path`, drawing one from `/dev/urandom` and writing it
    /// there, readable by its owner alone, when there is none
    ///
    /// # Errors
    ///
    /// [`SecretError`] when the file cannot be read or written.
    pub fn load_or_create(path: &Path) -> Result<Self, SecretError> {
        if let Some(secret) = Self::load(path)? {
            return Ok(secret);
        }
        match Self::write(path, false) {
            // Another `kelpie totp` wrote one first: that one is the secret.
            Err(SecretError::Io(_, io::ErrorKind::AlreadyExists)) => Self::load(path)?
                .ok_or_else(|| SecretError::Io(path.to_owned(), io::ErrorKind::NotFound)),
            written => written,
        }
    }

    /// A fresh secret in place of the one in `path`, for one that may have
    /// been read: every code of the old one stops working at once
    ///
    /// # Errors
    ///
    /// [`SecretError`] when the file cannot be written.
    pub fn rotate(path: &Path) -> Result<Self, SecretError> {
        Self::write(path, true)
    }

    // Draws a secret and puts it in `path` whole: written beside it and
    // synced, then renamed over the old one when `replace`, else linked into
    // place, which fails when a secret is already there. The folder is synced
    // after, so a crash cannot bring the old secret back.
    fn write(path: &Path, replace: bool) -> Result<Self, SecretError> {
        let io = |e: io::Error| SecretError::Io(path.to_owned(), e.kind());
        let mut bytes = [0u8; 20];
        fs::File::open("/dev/urandom")
            .and_then(|mut random| random.read_exact(&mut bytes))
            .map_err(|e| SecretError::Io(PathBuf::from("/dev/urandom"), e.kind()))?;
        let folder = path.parent().unwrap_or(Path::new("."));
        private_dir(folder).map_err(io)?;
        let fresh = folder.join(format!(".secret.{}", std::process::id()));
        let _ = fs::remove_file(&fresh);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&fresh)
            .map_err(io)?;
        let placed = writeln!(file, "{}", encode(&bytes))
            .and_then(|()| file.sync_all())
            .and_then(|()| {
                if replace {
                    fs::rename(&fresh, path)
                } else {
                    fs::hard_link(&fresh, path)
                }
            });
        let _ = fs::remove_file(&fresh);
        placed
            .and_then(|()| fs::File::open(folder)?.sync_all())
            .map_err(io)?;
        Ok(Self(bytes))
    }

    /// The `otpauth://` URI an authenticator app scans
    pub fn uri(&self) -> String {
        format!(
            "otpauth://totp/kelpie?secret={}&issuer=kelpie&algorithm=SHA1&digits=6&period={STEP}",
            encode(&self.0)
        )
    }

    /// The code for time step `step`
    pub fn code(&self, step: u64) -> u32 {
        let mut mac = Hmac::<Sha1>::new_from_slice(&self.0).expect("HMAC takes any key length");
        mac.update(&step.to_be_bytes());
        let hash = mac.finalize().into_bytes();
        // RFC 4226's dynamic truncation
        let at = usize::from(hash[19] & 0x0f);
        let bits = u32::from_be_bytes([hash[at], hash[at + 1], hash[at + 2], hash[at + 3]]);
        (bits & 0x7fff_ffff) % 1_000_000
    }

    /// The step `typed` is the code for, at `at` or the step before, so a
    /// code sent as its step turns is still taken
    pub fn verify(&self, typed: &str, at: Timestamp) -> Option<u64> {
        if typed.len() != 6 || !typed.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let typed: u32 = typed.parse().ok()?;
        let now = step_of(at);
        [now, now.saturating_sub(1)]
            .into_iter()
            .find(|&step| same(self.code(step), typed))
    }
}

/// What `kelpie totp` prints: the secret in `path`, drawn the first time or
/// drawn afresh when `rotate`, as the URI an authenticator app takes and a
/// QR code of it for the terminal
///
/// # Errors
///
/// [`SecretError`] when the secret cannot be read or written.
pub fn show(path: &Path, rotate: bool) -> Result<String, SecretError> {
    use qrcode::QrCode;
    use qrcode::render::unicode::Dense1x2;
    let secret = if rotate {
        Secret::rotate(path)?
    } else {
        Secret::load_or_create(path)?
    };
    let uri = secret.uri();
    let qr = QrCode::new(uri.as_bytes()).expect("a short URI fits a QR code");
    // Light on dark, which reads on a dark terminal and a light one alike.
    let qr = qr
        .render::<Dense1x2>()
        .dark_color(Dense1x2::Light)
        .light_color(Dense1x2::Dark)
        .build();
    Ok(format!(
        "Scan this with an authenticator app, then end a reply on the ntfy topic \
         with its code:\n\n{qr}\n\n{uri}\n\nThe secret is in {}. Keep it private.\n",
        path.display()
    ))
}

/// Every six digits in a row in `text` that could be a code, however it
/// was typed: full-width digits read as digits, spaces inside a group of
/// digits are dropped, and a longer run gives each six in a row
///
/// For claiming, never for acting: a right code anywhere in a reply is
/// spent, so a phone's stray period or a code typed first leaves nothing
/// for a reader of the topic to reuse.
pub fn codes_in(text: &str) -> Vec<String> {
    let mut codes = Vec::new();
    let mut run = String::new();
    let mut flush = |run: &mut String| {
        let digits: Vec<char> = run.chars().collect();
        for window in digits.windows(6) {
            codes.push(window.iter().collect());
        }
        run.clear();
    };
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let digit = match c {
            '0'..='9' => Some(c),
            // Full-width digits, which some keyboards type
            '\u{ff10}'..='\u{ff19}' => char::from_u32(u32::from(c) - 0xff10 + u32::from('0')),
            _ => None,
        };
        match digit {
            Some(d) => run.push(d),
            None if c.is_whitespace()
                && !run.is_empty()
                && chars.peek().is_some_and(|n| {
                    n.is_ascii_digit() || ('\u{ff10}'..='\u{ff19}').contains(n)
                }) => {}
            None => flush(&mut run),
        }
    }
    flush(&mut run);
    codes
}

/// The time step `at` falls in
pub fn step_of(at: Timestamp) -> u64 {
    at.0 / STEP
}

// Compares two codes in time that does not depend on where they differ.
fn same(a: u32, b: u32) -> bool {
    (a ^ b) == 0
}

/// Makes `folder` and any folder above it, the new ones readable by their
/// owner alone
pub(crate) fn private_dir(folder: &Path) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(folder)
}

fn encode(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(5) {
        let mut block = [0u8; 5];
        block[..chunk.len()].copy_from_slice(chunk);
        let bits = block.iter().fold(0u64, |n, &b| n << 8 | u64::from(b));
        let chars = (chunk.len() * 8).div_ceil(5);
        for i in 0..chars {
            let at = (bits >> (35 - i * 5)) & 31;
            out.push(char::from(BASE32[at as usize]));
        }
    }
    out
}

fn decode(text: &str) -> Option<[u8; 20]> {
    if text.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 20];
    for (block, chars) in text.as_bytes().chunks(8).enumerate() {
        let mut bits = 0u64;
        for &c in chars {
            let value = BASE32.iter().position(|&b| b == c)?;
            bits = bits << 5 | value as u64;
        }
        bytes[block * 5..block * 5 + 5].copy_from_slice(&bits.to_be_bytes()[3..]);
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 6238's SHA-1 key
    const RFC: Secret = Secret(*b"12345678901234567890");

    #[test]
    fn codes_match_rfc_6238s_sha1_vectors_in_six_digits() {
        for (time, eight) in [
            (59, 94_287_082),
            (1_111_111_109, 7_081_804),
            (1_111_111_111, 14_050_471),
            (1_234_567_890, 89_005_924),
            (2_000_000_000, 69_279_037),
            (20_000_000_000, 65_353_130),
        ] {
            assert_eq!(
                RFC.code(step_of(Timestamp(time))),
                eight % 1_000_000,
                "{time}"
            );
        }
    }

    #[test]
    fn a_code_is_taken_in_its_step_and_the_next_and_no_later() {
        let at = Timestamp(1_111_111_109);
        assert_eq!(RFC.verify("081804", at), Some(step_of(at)));
        let next = Timestamp(at.0 + STEP);
        assert_eq!(RFC.verify("081804", next), Some(step_of(at)));
        assert_eq!(RFC.verify("081804", Timestamp(at.0 + 2 * STEP)), None);
        for bad in ["81804", "0818040", "08180a", "", "081805", " 81804"] {
            assert_eq!(RFC.verify(bad, at), None, "{bad:?}");
        }
    }

    #[test]
    fn the_secret_is_written_once_for_its_owner_and_read_back() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("totp/secret");
        assert_eq!(Secret::load(&path), Ok(None));
        let made = Secret::load_or_create(&path).unwrap();
        assert_eq!(
            Secret::load_or_create(&path).unwrap(),
            made,
            "never redrawn"
        );
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        let uri = made.uri();
        let text = fs::read_to_string(&path).unwrap();
        assert!(uri.contains(&format!("secret={}&", text.trim())), "{uri}");
        assert!(uri.starts_with("otpauth://totp/kelpie?"), "{uri}");
        assert!(uri.ends_with("&digits=6&period=30"), "{uri}");
        assert_eq!(format!("{made:?}"), "Secret(..)");

        fs::write(&path, "not base32\n").unwrap();
        let err = Secret::load(&path).unwrap_err();
        assert_eq!(err, SecretError::Malformed(path.clone()));
        assert!(!err.to_string().contains("not base32"), "{err}");
    }

    #[test]
    fn a_secret_others_may_read_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("totp/secret");
        Secret::load_or_create(&path).unwrap();
        for mode in [0o640, 0o604, 0o620] {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(
                Secret::load(&path),
                Err(SecretError::Exposed(path.clone())),
                "{mode:o}"
            );
        }
    }

    #[test]
    fn a_folder_others_may_use_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("totp/secret");
        Secret::load_or_create(&path).unwrap();
        let folder = path.parent().unwrap();
        for mode in [0o750, 0o705, 0o770, 0o707] {
            fs::set_permissions(folder, fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(
                Secret::load(&path),
                Err(SecretError::Exposed(folder.to_owned())),
                "{mode:o}"
            );
        }
    }

    #[test]
    fn two_first_runs_at_once_keep_one_secret() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("totp/secret");
        let first = Secret::write(&path, false).unwrap();
        // The second lost the race: it keeps the first's secret.
        let err = Secret::write(&path, false).unwrap_err();
        assert_eq!(
            err,
            SecretError::Io(path.clone(), io::ErrorKind::AlreadyExists)
        );
        assert_eq!(Secret::load_or_create(&path).unwrap(), first);
        let left: Vec<_> = fs::read_dir(path.parent().unwrap()).unwrap().collect();
        assert_eq!(left.len(), 1, "no stray file beside it");
    }

    #[test]
    fn a_rotated_secret_replaces_the_old_one_whole() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("totp/secret");
        let old = Secret::load_or_create(&path).unwrap();
        let new = Secret::rotate(&path).unwrap();
        assert_ne!(old, new);
        assert_eq!(Secret::load(&path), Ok(Some(new)));
        let left: Vec<_> = fs::read_dir(path.parent().unwrap()).unwrap().collect();
        assert_eq!(left.len(), 1, "no stray file beside it");
    }

    #[test]
    fn show_prints_the_uri_and_a_qr_code_of_one_secret() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("totp/secret");
        let shown = show(&path, false).unwrap();
        let uri = Secret::load(&path).unwrap().unwrap().uri();
        assert!(shown.contains(&uri), "{shown}");
        assert!(shown.contains('▀') || shown.contains('▄'), "{shown}");
        assert_eq!(
            show(&path, false).unwrap(),
            shown,
            "the same secret every time"
        );
        let rotated = show(&path, true).unwrap();
        assert!(!rotated.contains(&uri), "a fresh secret");
    }

    #[test]
    fn every_way_of_typing_a_code_is_found() {
        let found = |text: &str| codes_in(text);
        assert_eq!(found("koji 1 no rename it 123456."), ["123456"]);
        assert_eq!(found("123456 koji 1 yes"), ["123456"]);
        assert_eq!(found("koji 1 yes 123 456"), ["123456"]);
        assert_eq!(found("123456"), ["123456"]);
        assert_eq!(found("koji 1 yes １２３４５６"), ["123456"]);
        assert_eq!(found("koji 1 yes 1234567"), ["123456", "234567"]);
        // The ruling id runs into the code when only a space parts them.
        assert!(found("koji 14 123456").contains(&"123456".to_owned()));
        assert_eq!(found("koji 1 yes 12345"), Vec::<String>::new());
        assert_eq!(found("koji 1 yes"), Vec::<String>::new());
    }

    #[test]
    fn base32_round_trips_rfc_4648s_example() {
        assert_eq!(encode(b"foobar"), "MZXW6YTBOI");
        let bytes = *b"12345678901234567890";
        assert_eq!(encode(&bytes), "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
        assert_eq!(decode(&encode(&bytes)), Some(bytes));
        assert_eq!(decode("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJ1"), None);
    }
}
