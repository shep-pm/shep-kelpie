//! `shep-kelpie browse-guard <domain>...`: a PreToolUse hook that holds a worker's
//! Playwright tools to the preview
//!
//! The Playwright MCP server runs outside the sandbox, and its own fence
//! blocks only the `file:` scheme: `view-source:file://` read any file. The
//! hook reads each Playwright tool call on stdin and lets a URL through only
//! when it is `http` or `https` to the dev server or a preview domain. It
//! refuses every file name, since the server checks one before it writes.
//! Screenshots go to kelpie's own output folder instead.

use std::io::Read;

use serde_json::Value;

use crate::confine::Verdict;
use crate::preview::LOCAL_HOSTS;

/// Judges the Playwright tool call in `input` against the preview's `domains`
pub fn judge(input: impl Read, domains: &[String]) -> Verdict {
    let call: Value = match serde_json::from_reader(input) {
        Ok(call) => call,
        Err(e) => return Verdict::Refuse(format!("kelpie cannot read this tool call: {e}")),
    };
    let Some(arguments) = call["tool_input"].as_object() else {
        return Verdict::Refuse("kelpie cannot read this tool call's input".into());
    };
    // The server resolves a file name's symlinks, then writes later: a worktree
    // folder swapped for a link in between took a write outside it.
    if arguments.get("filename").is_some_and(|f| !f.is_null()) {
        return Verdict::Refuse(
            "Playwright's tools save no file under kelpie: leave out `filename`, and a \
             screenshot goes to kelpie's own folder, whose path the tool returns"
                .into(),
        );
    }
    match arguments.get("url") {
        None | Some(Value::Null) => Verdict::Allow,
        Some(Value::String(url)) if allowed_url(url, domains) => Verdict::Allow,
        Some(url) => Verdict::Refuse(format!(
            "{url} is not the dev server or a preview domain: the browser opens only \
             http and https URLs to localhost, 127.0.0.1 and {}",
            domains.join(", ")
        )),
    }
}

/// Whether `url` is `http` or `https` to the dev server or one of `domains`
///
/// Anything this cannot read as such a URL is refused: another scheme, a
/// user name, an IPv6 host, an encoded host, or a character a browser drops.
pub fn allowed_url(url: &str, domains: &[String]) -> bool {
    let lower = url.to_ascii_lowercase();
    let Some(rest) = lower
        .strip_prefix("http://")
        .or_else(|| lower.strip_prefix("https://"))
    else {
        return false;
    };
    let authority = rest.split(['/', '?', '#', '\\']).next().unwrap_or_default();
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host,
        Some(_) => return false,
        None => authority,
    };
    let host = host.strip_suffix('.').unwrap_or(host);
    let plain = |c: char| c.is_ascii_alphanumeric() || c == '.' || c == '-';
    if host.is_empty() || !host.chars().all(plain) {
        return false;
    }
    LOCAL_HOSTS.contains(&host)
        || domains.iter().any(|d| match d.strip_prefix("*.") {
            Some(parent) => host.ends_with(&format!(".{parent}")),
            None => host == d.to_ascii_lowercase(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn domains() -> Vec<String> {
        vec!["pokemon-go-api.github.io".into(), "*.leekduck.com".into()]
    }

    fn call(tool: &str, input: Value) -> Verdict {
        let text = serde_json::json!({ "tool_name": tool, "tool_input": input }).to_string();
        judge(text.as_bytes(), &domains())
    }

    #[test]
    fn the_dev_server_and_the_preview_domains_open() {
        for url in [
            "http://localhost:3000/events",
            "http://127.0.0.1:3000",
            "https://pokemon-go-api.github.io/api/raidboss.json",
            "https://cdn.leekduck.com/a.png?x=1#y",
            "HTTP://LOCALHOST:3000/",
        ] {
            assert!(allowed_url(url, &domains()), "{url}");
        }
    }

    // The review's canary read a file through `view-source:file://`.
    #[test]
    fn every_other_scheme_and_every_other_host_is_refused() {
        for url in [
            "view-source:file:///Users/me/.kelpie/settings.toml",
            "file:///etc/hosts",
            "view-source:http://localhost:3000/",
            "javascript:alert(1)",
            "data:text/html,<p>",
            "about:blank",
            "chrome://settings",
            "ws://localhost:3000/",
            " http://localhost:3000/",
            "https://example.com/",
            "https://leekduck.com/",
            "https://evilpokemon-go-api.github.io/",
            "http://localhost@evil.example/",
            "http://evil.example\\@localhost/",
            "http://localhost.evil.example/",
            "http://[::1]:3000/",
            "http://%6cocalhost:3000/",
            "http://localhost:3000x/",
            "http://192.168.1.10:3000/",
        ] {
            assert!(!allowed_url(url, &domains()), "{url}");
        }
    }

    #[test]
    fn navigate_and_a_new_tab_are_held_to_the_preview() {
        assert_eq!(
            call(
                "mcp__playwright__browser_navigate",
                serde_json::json!({ "url": "http://localhost:3000/" })
            ),
            Verdict::Allow
        );
        let Verdict::Refuse(why) = call(
            "mcp__playwright__browser_navigate",
            serde_json::json!({ "url": "view-source:file:///etc/hosts" }),
        ) else {
            panic!("view-source was let through");
        };
        assert!(
            why.contains("not the dev server or a preview domain"),
            "{why}"
        );
        assert!(matches!(
            call(
                "mcp__playwright__browser_tabs",
                serde_json::json!({ "action": "new", "url": "file:///etc/hosts" })
            ),
            Verdict::Refuse(_)
        ));
        assert_eq!(
            call(
                "mcp__playwright__browser_tabs",
                serde_json::json!({ "action": "list" })
            ),
            Verdict::Allow
        );
    }

    // The review wrote outside the worktree by racing a folder against a link.
    #[test]
    fn no_playwright_tool_names_a_file() {
        // `browser_pdf_save` and `browser_storage_state` are not in 0.0.82's
        // default set; they come with its `--caps`, so the rule covers them too.
        for tool in [
            "browser_take_screenshot",
            "browser_snapshot",
            "browser_evaluate",
            "browser_console_messages",
            "browser_network_requests",
            "browser_network_request",
            "browser_pdf_save",
            "browser_storage_state",
        ] {
            for filename in [
                "shot.png",
                "d/x.json",
                ".claude/settings.local.json",
                "/tmp/x",
            ] {
                let verdict = call(
                    &format!("mcp__playwright__{tool}"),
                    serde_json::json!({ "filename": filename }),
                );
                assert!(matches!(verdict, Verdict::Refuse(_)), "{tool} {filename}");
            }
        }
        assert_eq!(
            call(
                "mcp__playwright__browser_take_screenshot",
                serde_json::json!({ "type": "png" })
            ),
            Verdict::Allow
        );
        assert_eq!(
            call(
                "mcp__playwright__browser_snapshot",
                serde_json::json!({ "filename": null })
            ),
            Verdict::Allow
        );
    }

    #[test]
    fn a_call_kelpie_cannot_read_is_refused() {
        assert!(matches!(
            judge("not json".as_bytes(), &domains()),
            Verdict::Refuse(_)
        ));
        assert!(matches!(
            judge("{}".as_bytes(), &domains()),
            Verdict::Refuse(_)
        ));
    }
}
