//! The alert channel: the webhook rulings post to, and a test post when asked

use std::path::Path;

use super::Line;
use crate::channels::{Channel, Channels};
use crate::ports::{Alert, Alerts};
use crate::webhook::{KelpieSettings, WebhookKind};

const NO_WEBHOOK_FIX: &str = "add a `webhook` table to kelpie's [kelpie] section of dogs.toml, as \
    kelpie-settings.example.toml shows, or drop `webhook` from `ruling_channels`";

/// Kelpie's own settings: its `[kelpie]` section, else the file it had before one
///
/// # Errors
///
/// The line to report when either is malformed. It names the line that is
/// wrong, never its text, since that text can be the webhook's URL.
pub(super) fn kelpie_settings(section: &str, file: &Path) -> Result<KelpieSettings, Line> {
    let read = if !section.trim().is_empty() {
        KelpieSettings::from_section(section)
    } else if file.exists() {
        KelpieSettings::load(file)
    } else {
        Ok(KelpieSettings::default())
    };
    read.map_err(|e| {
        Line::missing(
            "kelpie settings",
            e.to_string(),
            "correct the line it names",
        )
    })
}

/// Whether one project's rulings can reach the maintainer
pub(super) fn channel(
    subject: String,
    project: Option<&Channels>,
    kelpie: &KelpieSettings,
) -> Line {
    let channels = project
        .or(kelpie.ruling_channels.as_ref())
        .cloned()
        .unwrap_or_default();
    if !channels.has(Channel::Webhook) {
        return Line::ok(
            subject,
            "rulings go to the relay session, and the webhook is off",
        );
    }
    match &kelpie.webhook {
        Some(webhook) => Line::ok(
            subject,
            format!("rulings post to the {} webhook", kind(webhook.kind)),
        ),
        None => Line::missing(
            subject,
            "rulings go to the webhook, and kelpie's settings name none",
            NO_WEBHOOK_FIX,
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
            "add one, as kelpie-settings.example.toml shows, or leave rulings to the relay",
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
