//! The alert channel: the webhook rulings post to, and a test post when asked

use std::path::Path;

use super::Line;
use crate::ports::{Alert, Alerts};
use crate::settings::SettingsError;
use crate::webhook::{KelpieSettings, WebhookKind};

/// Kelpie's own settings: its `[kelpie]` section, empty when there is none
///
/// # Errors
///
/// The line to report when it is malformed. It names the line that is
/// wrong, never its text, since that text can be the webhook's URL.
pub(super) fn kelpie_settings(section: &str) -> Result<KelpieSettings, Line> {
    let read = if section.trim().is_empty() {
        Ok(KelpieSettings::default())
    } else {
        KelpieSettings::from_section(section)
    };
    read.map_err(|e| {
        Line::missing(
            "kelpie settings",
            e.to_string(),
            "correct the line it names",
        )
    })
}

/// The line to report when there is no `[kelpie]` section and kelpie's own
/// old settings file is still there, which kelpie no longer reads
pub(super) fn old_file(section: &str, kelpie_home: &Path) -> Option<Line> {
    let file = kelpie_home.join("settings.toml");
    if !section.trim().is_empty() || !file.exists() {
        return None;
    }
    let what = SettingsError::Section {
        message: format!(
            "there is none, and kelpie no longer reads {}: move its keys into the section",
            file.display()
        ),
    };
    Some(Line::missing(
        "kelpie settings",
        what.to_string(),
        "move its keys into the `[kelpie]` section of dogs.toml, then delete the file",
    ))
}

/// Whether one project's rulings can reach the maintainer
pub(super) fn channel(subject: String, kelpie: &KelpieSettings) -> Line {
    match &kelpie.webhook {
        Some(webhook) => Line::ok(
            subject,
            format!("rulings post to the {} webhook", kind(webhook.kind)),
        ),
        None => Line::ok(
            subject,
            "no webhook, so rulings reach you only in the log, `status` and `shep kelpie rule`",
        ),
    }
}

/// Posts one test alert to the webhook, and reports whether it was taken
pub(super) fn test_alert(kelpie: Option<&KelpieSettings>, alerts: &dyn Alerts) -> Line {
    let subject = "test alert";
    let Some(kelpie) = kelpie else {
        return Line::unsure(
            subject,
            "not sent, since kelpie's settings cannot be read",
            "correct them, then ask again",
        );
    };
    let Some(webhook) = &kelpie.webhook else {
        return Line::unsure(
            subject,
            "there is no webhook to post to",
            "add one, as kelpie-settings.example.toml shows",
        );
    };
    let alert = Alert {
        title: "kelpie doctor".to_owned(),
        text: "A test alert from `shep kelpie doctor`. There is nothing to answer.".to_owned(),
        reply: None,
    };
    match alerts.post(webhook, &alert) {
        Ok(()) => Line::ok(subject, "posted, so look for it where you read alerts"),
        Err(e) => Line::missing(
            subject,
            format!("the webhook did not take it: {e}"),
            "check `webhook.url` in kelpie's [kelpie] section",
        ),
    }
}

fn kind(kind: WebhookKind) -> &'static str {
    match kind {
        WebhookKind::Discord => "Discord",
        WebhookKind::Ntfy => "ntfy",
    }
}
