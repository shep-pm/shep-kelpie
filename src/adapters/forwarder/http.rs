//! Reading one HTTP/1 request from a worker, every read bounded in bytes and in time

use std::io::{self, BufRead, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Instant;

use serde_json::json;

/// The most a request's head may take, in bytes
pub(super) const HEAD_MAX: usize = 64 * 1024;

/// The most a request's body may take, in bytes: a chat call carries its whole context
pub(super) const BODY_MAX: u64 = 64 << 20;

/// The most a chunk's size line may take, in bytes
const LINE_MAX: usize = 1024;

/// A refusal or failure, as the reply the worker's harness reads
#[derive(Debug)]
pub(super) struct Reply {
    pub(super) status: u16,
    text: String,
}

impl Reply {
    pub(super) fn new(status: u16, text: impl Into<String>) -> Self {
        Self {
            status,
            text: text.into(),
        }
    }

    pub(super) fn write(&self, to: &mut impl Write) -> io::Result<()> {
        let reason = match self.status {
            400 => "Bad Request",
            403 => "Forbidden",
            408 => "Request Timeout",
            413 => "Content Too Large",
            501 => "Not Implemented",
            503 => "Service Unavailable",
            _ => "Bad Gateway",
        };
        let body = json!({ "error": { "message": self.text, "type": "kelpie_refused" } });
        let body = body.to_string();
        write!(
            to,
            "HTTP/1.1 {} {reason}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.status,
            body.len()
        )?;
        to.flush()
    }
}

/// A client's stream that gives up at a deadline, however slowly it is fed
#[derive(Debug)]
pub(super) struct Timed {
    pub(super) stream: UnixStream,
    pub(super) deadline: Instant,
}

impl Read for Timed {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self
            .deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or(io::ErrorKind::TimedOut)?;
        self.stream.set_read_timeout(Some(left))?;
        self.stream.read(buf)
    }
}

fn bad(why: &str) -> Reply {
    Reply::new(400, format!("kelpie cannot read the request: {why}"))
}

// What a failed read tells the worker.
fn unread(e: &io::Error, what: &str) -> Reply {
    match e.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
            Reply::new(408, "kelpie gave up waiting for the request")
        }
        io::ErrorKind::UnexpectedEof => bad(&format!("{what} ended early")),
        io::ErrorKind::FileTooLarge => Reply::new(413, "the request body is too large"),
        _ => bad(&format!("{what} is too long or is not text")),
    }
}

// One line without its line ending, read through a limit so a line with no
// end cannot be buffered whole.
fn read_line(reader: &mut impl BufRead, max: usize) -> io::Result<String> {
    let mut line = Vec::new();
    reader
        .by_ref()
        .take(max as u64 + 1)
        .read_until(b'\n', &mut line)?;
    if line.last() != Some(&b'\n') {
        let kind = match line.len() > max {
            true => io::ErrorKind::InvalidData,
            false => io::ErrorKind::UnexpectedEof,
        };
        return Err(kind.into());
    }
    line.pop();
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    String::from_utf8(line).map_err(|_| io::ErrorKind::InvalidData.into())
}

fn is_token(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c))
}

/// A request's line and headers, names in lower case
#[derive(Debug)]
pub(super) struct Head {
    pub(super) method: String,
    pub(super) target: String,
    pub(super) headers: Vec<(String, String)>,
}

impl Head {
    pub(super) fn read(reader: &mut impl BufRead) -> Result<Self, Reply> {
        let mut left = HEAD_MAX;
        let mut lines = Vec::new();
        loop {
            let line = read_line(reader, left).map_err(|e| unread(&e, "the head"))?;
            left = left
                .checked_sub(line.len() + 2)
                .ok_or_else(|| bad("its head is too long"))?;
            if line.chars().any(|c| c.is_control() && c != '\t') {
                return Err(bad("a line has a control character"));
            }
            match (line.is_empty(), lines.is_empty()) {
                (true, false) => break,
                (true, true) => {}
                (false, _) => lines.push(line),
            }
        }
        let mut first = lines[0].split(' ');
        let (Some(method), Some(target), Some(version), None) =
            (first.next(), first.next(), first.next(), first.next())
        else {
            return Err(bad("its first line is not a request line"));
        };
        if !version.starts_with("HTTP/1.") || !is_token(method) {
            return Err(bad("it is not an HTTP/1 request"));
        }
        let headers = lines[1..]
            .iter()
            .map(|line| match line.split_once(':') {
                Some((name, value)) if is_token(name) => {
                    Ok((name.to_ascii_lowercase(), value.trim().to_owned()))
                }
                _ => Err(bad("a header has no valid name")),
            })
            .collect::<Result<_, Reply>>()?;
        Ok(Self {
            method: method.to_owned(),
            target: target.to_owned(),
            headers,
        })
    }

    fn values<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> {
        self.headers
            .iter()
            .filter(move |(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    pub(super) fn expects_continue(&self) -> bool {
        self.values("expect")
            .any(|v| v.eq_ignore_ascii_case("100-continue"))
    }

    // The body, whole: counted by its length or decoded from its chunks. A
    // request that says both is the shape of a smuggled one and is refused.
    pub(super) fn body(&self, reader: &mut impl BufRead) -> Result<Vec<u8>, Reply> {
        let encodings: Vec<_> = self.values("transfer-encoding").collect();
        let lengths: Vec<_> = self.values("content-length").collect();
        match (encodings.as_slice(), lengths.as_slice()) {
            ([], []) => Ok(Vec::new()),
            ([encoding], []) if encoding.eq_ignore_ascii_case("chunked") => {
                chunked(reader).map_err(|e| unread(&e, "its chunks"))
            }
            ([_, ..], _) if !lengths.is_empty() => Err(bad("it gives a length and an encoding")),
            ([_, ..], _) => Err(Reply::new(501, "kelpie reads only chunked bodies")),
            ([], [first, rest @ ..]) => {
                let length: u64 = first
                    .parse()
                    .map_err(|_| bad("its length is not a number"))?;
                if rest.iter().any(|other| other != first) {
                    return Err(bad("it gives two lengths"));
                }
                if length > BODY_MAX {
                    return Err(Reply::new(413, "the request body is too large"));
                }
                let mut body = Vec::new();
                let read = reader.take(length).read_to_end(&mut body);
                read.map_err(|e| unread(&e, "its body"))?;
                if body.len() as u64 == length {
                    Ok(body)
                } else {
                    Err(bad("its body ended early"))
                }
            }
        }
    }
}

// Decodes a chunked body, its trailers read and dropped.
pub(super) fn chunked(reader: &mut impl BufRead) -> io::Result<Vec<u8>> {
    let invalid = || io::Error::from(io::ErrorKind::InvalidData);
    let mut body = Vec::new();
    loop {
        let line = read_line(reader, LINE_MAX)?;
        let size = line.split(';').next().unwrap_or("").trim();
        let size = u64::from_str_radix(size, 16).map_err(|_| invalid())?;
        if size == 0 {
            break;
        }
        if size > BODY_MAX - body.len() as u64 {
            return Err(io::ErrorKind::FileTooLarge.into());
        }
        let before = body.len();
        reader.by_ref().take(size).read_to_end(&mut body)?;
        let mut end = [0u8; 2];
        reader.read_exact(&mut end)?;
        if (body.len() - before) as u64 != size || &end != b"\r\n" {
            return Err(invalid());
        }
    }
    let mut left = HEAD_MAX;
    loop {
        let line = read_line(reader, LINE_MAX)?;
        if line.is_empty() {
            return Ok(body);
        }
        left = left.checked_sub(line.len() + 2).ok_or_else(invalid)?;
    }
}

#[cfg(test)]
mod tests {
    use std::io::BufReader;

    use super::*;

    // Gives 'a' for ever, and fails the test once more than a megabyte is taken.
    struct Endless(usize);

    impl Read for Endless {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0 += buf.len();
            assert!(self.0 < 1 << 20, "read past any limit");
            buf.fill(b'a');
            Ok(buf.len())
        }
    }

    #[test]
    fn a_line_with_no_end_stops_at_its_limit() {
        let mut reader = BufReader::new(Endless(0));
        let err = read_line(&mut reader, 100).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let err = Head::read(&mut BufReader::new(Endless(0))).unwrap_err();
        assert_eq!(err.status, 400);
        let err = chunked(&mut BufReader::new(Endless(0))).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
