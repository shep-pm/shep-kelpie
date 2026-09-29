//! The authenticator code that proves an answer from ntfy is the maintainer's
//!
//! RFC 6238 TOTP: HMAC-SHA-1, 30-second steps, six digits, which every
//! authenticator app reads from an `otpauth://` URI. Kelpie draws the secret
//! once, keeps it under its home with only the owner able to read it, and
//! never posts it anywhere: `kelpie totp` shows it to the maintainer to scan.
//! Anyone who can read the ntfy topic sees each code the maintainer sends, so
//! a step answers once across every ruling of every project, claimed by
//! creating a file named for it that no second claim can create again.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;

use crate::ports::Timestamp;

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
        }
    }
}

impl core::error::Error for SecretError {}

impl Secret {
    /// The secret in `path`, or `None` when there is no file
    ///
    /// # Errors
    ///
    /// [`SecretError`] when the file cannot be read or is not base32 of 20 bytes.
    pub fn load(path: &Path) -> Result<Option<Self>, SecretError> {
        match fs::read_to_string(path) {
            Ok(text) => decode(text.trim())
                .map(|bytes| Some(Self(bytes)))
                .ok_or_else(|| SecretError::Malformed(path.to_owned())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(SecretError::Io(path.to_owned(), e.kind())),
        }
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
        let io = |e: io::Error| SecretError::Io(path.to_owned(), e.kind());
        let mut bytes = [0u8; 20];
        fs::File::open("/dev/urandom")
            .and_then(|mut random| random.read_exact(&mut bytes))
            .map_err(|e| SecretError::Io(PathBuf::from("/dev/urandom"), e.kind()))?;
        if let Some(folder) = path.parent() {
            fs::create_dir_all(folder).map_err(io)?;
        }
        // `create_new`, so two commands at once never write two secrets.
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(io)?;
        writeln!(file, "{}", encode(&bytes)).map_err(io)?;
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

/// What `kelpie totp` prints: the secret in `path`, drawn the first time,
/// as the URI an authenticator app takes and a QR code of it for the terminal
///
/// # Errors
///
/// [`SecretError`] when the secret cannot be read or written.
pub fn show(path: &Path) -> Result<String, SecretError> {
    use qrcode::QrCode;
    use qrcode::render::unicode::Dense1x2;
    let uri = Secret::load_or_create(path)?.uri();
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

/// The time step `at` falls in
pub fn step_of(at: Timestamp) -> u64 {
    at.0 / STEP
}

// Compares two codes in time that does not depend on where they differ.
fn same(a: u32, b: u32) -> bool {
    (a ^ b) == 0
}

/// The steps already used to answer a ruling, as files in one folder that
/// every project's runner shares
#[derive(Debug, Clone)]
pub struct Used(PathBuf);

impl Used {
    /// The steps recorded in `folder`
    pub fn in_folder(folder: PathBuf) -> Self {
        Self(folder)
    }

    /// Claims `step`, and whether this was its first claim. Steps too old to
    /// be sent again are let go.
    ///
    /// # Errors
    ///
    /// The OS's error when the folder or the claim cannot be written; the
    /// step then counts as not claimed.
    pub fn claim(&self, step: u64, now: Timestamp) -> io::Result<bool> {
        fs::create_dir_all(&self.0)?;
        let claimed = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.0.join(step.to_string()));
        let first = match claimed {
            Ok(_) => true,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => false,
            Err(e) => return Err(e),
        };
        // A reply is read at most an hour after it is sent, so older steps
        // can never be sent again.
        let oldest = step_of(now).saturating_sub(3600 / STEP);
        for entry in fs::read_dir(&self.0)?.flatten() {
            let old = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u64>().ok());
            if old.is_some_and(|old| old < oldest) {
                let _ = fs::remove_file(entry.path());
            }
        }
        Ok(first)
    }
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
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
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
    fn show_prints_the_uri_and_a_qr_code_of_one_secret() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("totp/secret");
        let shown = show(&path).unwrap();
        let uri = Secret::load(&path).unwrap().unwrap().uri();
        assert!(shown.contains(&uri), "{shown}");
        assert!(shown.contains('▀') || shown.contains('▄'), "{shown}");
        assert_eq!(show(&path).unwrap(), shown, "the same secret every time");
    }

    #[test]
    fn base32_round_trips_rfc_4648s_example() {
        assert_eq!(encode(b"foobar"), "MZXW6YTBOI");
        let bytes = *b"12345678901234567890";
        assert_eq!(encode(&bytes), "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
        assert_eq!(decode(&encode(&bytes)), Some(bytes));
        assert_eq!(decode("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJ1"), None);
    }

    #[test]
    fn a_step_is_claimed_once_across_every_runner() {
        let home = tempfile::tempdir().unwrap();
        let now = Timestamp(1_111_111_109);
        let (one, two) = (
            Used::in_folder(home.path().join("used")),
            Used::in_folder(home.path().join("used")),
        );
        let step = step_of(now);
        assert!(one.claim(step, now).unwrap());
        assert!(!two.claim(step, now).unwrap());
        assert!(two.claim(step + 1, now).unwrap());

        let later = Timestamp(now.0 + 2 * 3600);
        assert!(one.claim(step_of(later), later).unwrap());
        assert!(!home.path().join(format!("used/{step}")).exists(), "let go");
    }
}
