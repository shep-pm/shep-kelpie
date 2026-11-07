//! A file read as it grows, a whole line at a time

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

/// A file and how far into it has been read
#[derive(Debug)]
pub(super) struct Tail {
    path: PathBuf,
    offset: u64,
}

impl Tail {
    /// A tail that reads `path` from its first line
    pub fn from_start(path: PathBuf) -> Self {
        Self { path, offset: 0 }
    }

    /// A tail that reads only what `path` gets from now on
    pub fn at_end(path: PathBuf) -> Self {
        let offset = std::fs::metadata(&path).map_or(0, |m| m.len());
        Self { path, offset }
    }

    /// The whole lines written since the last call
    ///
    /// A line still being written waits for its end. A file that shrank
    /// was replaced, and is read from its start. A bad byte costs only its
    /// own line. A file that cannot be read gives none, as for a file that
    /// has grown by nothing.
    pub fn lines(&mut self) -> Vec<String> {
        self.read_lines().unwrap_or_default()
    }

    /// The whole lines written since the last call, or why the file could not be read
    ///
    /// As [`Tail::lines`], but a file that cannot be opened or read is an
    /// error and not a file with nothing new.
    pub fn read_lines(&mut self) -> std::io::Result<Vec<String>> {
        let mut file = std::fs::File::open(&self.path)?;
        let len = file.metadata()?.len();
        if len < self.offset {
            self.offset = 0;
        }
        if len == self.offset {
            return Ok(Vec::new());
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut bytes = Vec::new();
        file.take(len - self.offset).read_to_end(&mut bytes)?;
        let Some(end) = bytes.iter().rposition(|&b| b == b'\n') else {
            return Ok(Vec::new());
        };
        self.offset += end as u64 + 1;
        Ok(String::from_utf8_lossy(&bytes[..end])
            .split('\n')
            .map(|line| line.trim_end_matches('\r').to_owned())
            .filter(|line| !line.is_empty())
            .collect())
    }
}
