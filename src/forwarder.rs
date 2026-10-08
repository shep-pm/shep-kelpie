//! What a local worker's model calls may ask of the model server
//!
//! A sandbox allows a host on every port, and a model server answers its
//! admin calls (pull, delete, create) where it answers chat. So a local
//! worker never reaches the server. It reaches a forwarder kelpie runs
//! outside the sandbox, which passes one request, the chat completions call,
//! and refuses every other path and method by name. A model behind a
//! gateway gets the gateway's key from the forwarder, in place of whatever
//! the worker sent, so the key never enters the sandbox.

use std::net::{IpAddr, Ipv4Addr};

use crate::confine::Verdict;
use crate::settings::{EndpointUrl, GatewayKey};

/// The host a worker's sandbox is told its model lives on
///
/// Nothing resolves it: the sandbox hands its traffic to the forwarder.
pub const WORKER_HOST: &str = "model.kelpie.test";

/// The longest a refusal quotes of what was asked, in bytes
const QUOTE_MAX: usize = 200;

/// The one model server a forwarder passes chat calls to
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    address: String,
    host: String,
    base: String,
    key: Option<GatewayKey>,
}

/// Why a model server's URL cannot be forwarded to
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamError {
    /// The URL is `https://`, which the forwarder cannot dial
    Https,
    /// The URL has a login, a query or a fragment, which no OpenAI-compatible base has
    Unusable,
}

impl core::fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Https => {
                "kelpie's forwarder reaches a model server over plain `http://`, \
                 so give the server's `http://` address"
            }
            Self::Unusable => "a model server's URL has no login, query or fragment",
        })
    }
}

impl core::error::Error for UpstreamError {}

impl Upstream {
    /// The server at `url`, whose base path ends in `/v1`
    ///
    /// # Errors
    ///
    /// [`UpstreamError`] when the forwarder cannot dial the URL.
    pub fn new(url: &EndpointUrl) -> Result<Self, UpstreamError> {
        let url = url.as_str();
        let Some(rest) = url.strip_prefix("http://") else {
            return Err(UpstreamError::Https);
        };
        let (authority, base) = rest.find('/').map_or((rest, ""), |at| rest.split_at(at));
        let unusable = |text: &str| text.contains(['?', '#']);
        if authority.contains('@') || unusable(authority) || unusable(base) {
            return Err(UpstreamError::Unusable);
        }
        let (host, address) = match authority.rsplit_once(':') {
            Some((host, port)) if !port.contains(']') => (host, authority.to_owned()),
            _ => (authority, format!("{authority}:80")),
        };
        Ok(Self {
            address,
            host: canonical(host),
            base: base.to_owned(),
            key: None,
        })
    }

    /// The same server, a gateway that takes `key` as its bearer key
    pub fn with_key(self, key: GatewayKey) -> Self {
        Self {
            key: Some(key),
            ..self
        }
    }

    /// The gateway's key the forwarder sends in place of the worker's, if any
    #[inline]
    pub fn key(&self) -> Option<&GatewayKey> {
        self.key.as_ref()
    }

    /// The server's host, without port or brackets, in lower case
    #[inline]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The `host:port` the forwarder dials
    #[inline]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Whether an allowed domain such as `*.example.com` would let a sandbox reach the server
    ///
    /// Loopback names count in any spelling, since the sandbox opens every
    /// local port to them. A different name or address for the same machine
    /// cannot be told without resolving it.
    pub fn is_reached_by(&self, domain: &str) -> bool {
        let domain = canonical(without_port(domain.trim()));
        if let Some(suffix) = domain.strip_prefix("*.") {
            return self.host.ends_with(&format!(".{suffix}")) || loops_back(suffix);
        }
        let same_address = address(&domain).is_some() && address(&domain) == address(&self.host);
        domain == self.host || same_address || loops_back(&domain)
    }

    /// The path the one passed request asks for
    pub fn chat_path(&self) -> String {
        format!("{}/chat/completions", self.base)
    }

    /// The base URL the worker's own harness is given, on [`WORKER_HOST`]
    pub fn worker_url(&self) -> String {
        format!("http://{WORKER_HOST}{}", self.base)
    }

    /// Judges a `CONNECT` to `authority`, which only the worker's own host passes
    ///
    /// A client behind the sandbox's proxy opens a tunnel before its one
    /// request, and the request in it is judged as any other.
    pub fn judge_tunnel(&self, authority: &str) -> Verdict {
        if authority == format!("{WORKER_HOST}:80") {
            return Verdict::Allow;
        }
        Verdict::Refuse(format!(
            "kelpie opens a tunnel only to {WORKER_HOST}:80, so {} was refused",
            quote("CONNECT", authority),
        ))
    }

    /// Judges a request asking for `method` and `target`, as the request line says
    ///
    /// The target is an absolute URL, as the sandbox's proxy writes it, or a
    /// path. Whatever else it is, a query included, is refused.
    pub fn judge(&self, method: &str, target: &str) -> Verdict {
        let path = match target.strip_prefix("http://") {
            Some(rest) => rest.find('/').map_or("", |at| &rest[at..]),
            None => target,
        };
        if method == "POST" && path == self.chat_path() {
            return Verdict::Allow;
        }
        Verdict::Refuse(format!(
            "kelpie passes only POST {} to the model server, so {} was refused",
            self.chat_path(),
            quote(method, path),
        ))
    }
}

// A host as `srt` compares it: no brackets or trailing dot, in lower case.
fn canonical(name: &str) -> String {
    let name = name.trim().trim_matches(['[', ']']);
    name.trim_end_matches('.').to_ascii_lowercase()
}

// A domain entry as `srt` reads it: an optional port after the host, which
// is bracketed when it is an IPv6 address.
fn without_port(entry: &str) -> &str {
    if let Some(rest) = entry.strip_prefix('[') {
        return rest.split_once(']').map_or(entry, |(host, _)| host);
    }
    match entry.split_once(':') {
        Some((host, port)) if !port.contains(':') => {
            let valid = port.parse::<u16>().is_ok_and(|n| n > 0) && !port.starts_with('0');
            if valid { host } else { entry }
        }
        _ => entry,
    }
}

// An IP address in any spelling an `inet_aton` takes, `127.1` and `0x7f.1`
// included, with an IPv4-mapped IPv6 address as the IPv4 address.
fn address(name: &str) -> Option<IpAddr> {
    if let Ok(ip) = name.parse::<IpAddr>() {
        return Some(match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
            v4 => v4,
        });
    }
    let number = |part: &str| match part.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None if part.len() > 1 && part.starts_with('0') => u64::from_str_radix(part, 8).ok(),
        None => part.parse().ok(),
    };
    let parts: Vec<u64> = name.split('.').map(number).collect::<Option<_>>()?;
    let (last, leading) = parts.split_last()?;
    if leading.len() > 3 {
        return None;
    }
    let free = 8 * (4 - leading.len() as u32);
    if leading.iter().any(|p| *p > 255) || *last >= 1 << free {
        return None;
    }
    let high = leading.iter().fold(0u64, |all, p| all << 8 | p) << free;
    Some(Ipv4Addr::from(u32::try_from(high | last).ok()?).into())
}

// Whether `name` is this machine however it is written.
fn loops_back(name: &str) -> bool {
    let local =
        name == "localhost" || name.ends_with(".localhost") || name == "localhost.localdomain";
    let ours = |ip: Ipv4Addr| ip.is_loopback() || ip.is_unspecified();
    local
        || match address(name) {
            Some(IpAddr::V4(ip)) => ours(ip),
            Some(IpAddr::V6(ip)) => {
                ip.is_loopback() || ip.is_unspecified() || ip.to_ipv4_mapped().is_some_and(ours)
            }
            None => false,
        }
}

// What was asked, cut short and with control characters dropped, since a
// worker's own code chose it and it goes back into a reply and a log.
fn quote(method: &str, path: &str) -> String {
    let asked: String = format!("{method} {path}")
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    match asked.char_indices().nth(QUOTE_MAX) {
        Some((cut, _)) => format!("{}...", &asked[..cut]),
        None => asked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upstream(url: &str) -> Upstream {
        Upstream::new(&EndpointUrl::try_from(url.to_owned()).unwrap()).unwrap()
    }

    fn refusal(verdict: Verdict) -> String {
        match verdict {
            Verdict::Refuse(why) => why,
            Verdict::Allow => panic!("passed"),
        }
    }

    #[test]
    fn the_chat_call_passes_as_a_path_or_as_the_proxys_absolute_url() {
        let u = upstream("http://192.0.2.9:11434/v1");
        assert_eq!(u.judge("POST", "/v1/chat/completions"), Verdict::Allow);
        let proxied = "http://model.kelpie.test/v1/chat/completions";
        assert_eq!(u.judge("POST", proxied), Verdict::Allow);
    }

    #[test]
    fn an_allowed_domain_that_would_reopen_the_server_or_the_loopback_is_named() {
        let u = upstream("http://models.example.test:11434/v1");
        for domain in [
            "models.example.test",
            "Models.Example.Test",
            "*.example.test",
            "localhost",
            "127.0.0.1",
            "[::1]",
            "*.localhost",
            "127.0.0.2",
            "127.1",
            "0x7f.1",
            "2130706433",
            "localhost.",
            "::ffff:127.0.0.1",
            "0:0:0:0:0:0:0:1",
            "localhost.localdomain",
            "models.example.test.",
            "models.example.test:11434",
            "models.example.test:22",
            "*.example.test:443",
            "localhost:22",
            "[::1]:22",
            "127.0.0.1:11434",
            "[::ffff:127.0.0.1]",
            "0.0.0.0",
            "::",
        ] {
            assert!(u.is_reached_by(domain), "{domain}");
        }
        for domain in [
            "github.com",
            "example.test",
            "*.other.test",
            "notlocalhost",
            "128.0.0.1",
            "10.0.0.1",
        ] {
            assert!(!u.is_reached_by(domain), "{domain}");
        }
        assert!(upstream("http://[2001:db8::9]:80/v1").is_reached_by("2001:db8::9"));
        let by_address = upstream("http://192.0.2.9:11434/v1");
        for domain in [
            "192.0.2.9:11434",
            "192.0.2.9:22",
            "[::ffff:192.0.2.9]",
            "[::ffff:c000:209]",
            "[::ffff:192.0.2.9]:80",
            "0xc0.0.2.9",
            "3221225993",
        ] {
            assert!(by_address.is_reached_by(domain), "{domain}");
        }
        assert!(!by_address.is_reached_by("192.0.2.10:11434"));
        assert!(!by_address.is_reached_by("box.lan:11434"));
        assert!(upstream("http://box.lan:11434/v1").is_reached_by("box.lan:11434"));
    }

    #[test]
    fn a_tunnel_opens_only_to_the_workers_own_host() {
        let u = upstream("http://192.0.2.9:11434/v1");
        assert_eq!(u.judge_tunnel("model.kelpie.test:80"), Verdict::Allow);
        for authority in ["192.0.2.9:11434", "192.0.2.9:22", "model.kelpie.test:443"] {
            let why = refusal(u.judge_tunnel(authority));
            assert!(why.contains(&format!("CONNECT {authority}")), "{why}");
        }
    }

    #[test]
    fn the_worker_is_told_its_own_host_and_the_forwarder_dials_the_servers() {
        let u = upstream("http://192.0.2.9:11434/v1/");
        assert_eq!(u.worker_url(), "http://model.kelpie.test/v1");
        assert_eq!(u.address(), "192.0.2.9:11434");
        assert_eq!(upstream("http://192.0.2.9/v1").address(), "192.0.2.9:80");
        assert_eq!(
            upstream("http://[2001:db8::9]/v1").address(),
            "[2001:db8::9]:80"
        );
        assert_eq!(
            upstream("http://[2001:db8::9]:8080/v1").address(),
            "[2001:db8::9]:8080"
        );
    }

    #[test]
    fn every_other_path_and_method_is_refused_and_named() {
        let u = upstream("http://192.0.2.9:11434/v1");
        for (method, target, named) in [
            ("DELETE", "/api/delete", "DELETE /api/delete"),
            ("POST", "/api/pull", "POST /api/pull"),
            ("POST", "/api/create", "POST /api/create"),
            ("GET", "/v1/models", "GET /v1/models"),
            ("GET", "/v1/chat/completions", "GET /v1/chat/completions"),
            ("POST", "/v1/completions", "POST /v1/completions"),
            (
                "POST",
                "/v1/chat/completions/",
                "POST /v1/chat/completions/",
            ),
            ("POST", "/v1/chat/completions?x=1", "?x=1"),
            ("POST", "/v1/chat/../../api/delete", "/api/delete"),
            ("POST", "/V1/chat/completions", "/V1/chat/completions"),
            ("CONNECT", "192.0.2.9:22", "CONNECT 192.0.2.9:22"),
            ("post", "/v1/chat/completions", "post /v1/chat"),
        ] {
            let why = refusal(u.judge(method, target));
            assert!(why.contains(named), "{why}");
            assert!(why.contains("only POST /v1/chat/completions"), "{why}");
        }
    }

    #[test]
    fn a_refusal_quotes_a_long_or_odd_request_safely() {
        let u = upstream("http://192.0.2.9/v1");
        let long = format!("/{}", "a".repeat(5000));
        let why = refusal(u.judge("GET", &long));
        assert!(why.len() < 400, "{}", why.len());
        assert!(why.contains("..."), "{why}");
        let why = refusal(u.judge("GET", "/a\r\nb\u{7}c"));
        assert!(why.contains("GET /abc"), "{why:?}");
    }

    #[test]
    fn a_url_the_forwarder_cannot_dial_is_refused_with_the_reason() {
        let url = |u: &str| EndpointUrl::try_from(u.to_owned()).unwrap();
        let https = Upstream::new(&url("https://192.0.2.9/v1"));
        assert_eq!(https, Err(UpstreamError::Https));
        assert!(https.unwrap_err().to_string().contains("http://"));
        for odd in [
            "http://user:pw@192.0.2.9/v1",
            "http://192.0.2.9/v1?key=1",
            "http://192.0.2.9/v1#x",
        ] {
            assert_eq!(
                Upstream::new(&url(odd)),
                Err(UpstreamError::Unusable),
                "{odd}"
            );
        }
    }
}
