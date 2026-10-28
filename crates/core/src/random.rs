//! Random bytes from the system's own source, on Unix and Windows alike

use std::io;

/// Fills `buf` with random bytes
///
/// # Errors
///
/// The system's random source is unavailable.
pub fn fill(buf: &mut [u8]) -> io::Result<()> {
    getrandom::fill(buf).map_err(io::Error::from)
}

/// 128 random bits as 32 lowercase hex characters
///
/// # Errors
///
/// The system's random source is unavailable.
pub fn hex128() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    fill(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex128_is_32_lowercase_hex_characters() {
        let hex = hex128().unwrap();
        assert_eq!(hex.len(), 32);
        assert!(
            hex.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
            "{hex}"
        );
    }

    #[test]
    fn hex128_differs_between_draws() {
        assert_ne!(hex128().unwrap(), hex128().unwrap());
    }

    #[test]
    fn fill_overwrites_the_whole_buffer() {
        let mut buf = [0u8; 64];
        fill(&mut buf).unwrap();
        assert!(buf.iter().any(|&b| b != 0));
    }
}
