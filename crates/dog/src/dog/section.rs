//! The `[kelpie]` section the dog reads when it starts and reads again

use shep_client::RequestError;
use shep_client::shep_core::protocol::{DogSectionToml, RpcErrorCode};

use crate::shepherd::Connection;
use crate::webhook::KelpieSettings;

/// What asking the shepherd for the section came to
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Asked {
    /// The section's text, empty when `dogs.toml` has none
    Text(String),
    /// The shepherd refused the section, worded as a start words it
    Refused(String),
    /// No section came back: the connection closed, the ask timed out or there is no entry
    Unreadable(String),
}

/// The settings a start holds, and the text they were read from
#[derive(Debug)]
pub(super) struct Started {
    pub(super) settings: KelpieSettings,
    pub(super) text: String,
}

// Kelpie's section, for its leases. One that cannot be read holds them at
// the default capacities. A section the shepherd refuses stops the dog.
// So does a webhook `ca_cert` that cannot be read, and `trusted` with no
// `token`, since replies would then be taken on a topic anyone can post to.
pub(super) async fn settings(connection: &dyn Connection) -> Result<Started, String> {
    match ask(connection).await {
        Asked::Text(text) => Ok(Started {
            settings: read_section(&text)?,
            text,
        }),
        Asked::Refused(reason) => Err(reason),
        Asked::Unreadable(why) => {
            println!("{why}");
            Ok(Started {
                settings: KelpieSettings::default(),
                text: String::new(),
            })
        }
    }
}

// The section's text as the shepherd hands it over.
pub(super) async fn ask(connection: &dyn Connection) -> Asked {
    answered(connection.dog_config(crate::shepherd::DOG).await)
}

fn answered(answer: Result<DogSectionToml, RequestError>) -> Asked {
    match answer {
        Ok(toml) => Asked::Text(toml.as_str().to_owned()),
        Err(RequestError::Rpc(refusal))
            if matches!(
                refusal.code,
                RpcErrorCode::InvalidConfig | RpcErrorCode::Unsupported
            ) =>
        {
            Asked::Refused(format!(
                "the shepherd refused the [kelpie] section: {}",
                refusal.message
            ))
        }
        Err(e) => Asked::Unreadable(format!("cannot read the [kelpie] section: {e}")),
    }
}

// The start's read: a section that does not parse whole is salvaged table by table.
fn read_section(text: &str) -> Result<KelpieSettings, String> {
    if text.trim().is_empty() {
        return Ok(KelpieSettings::default());
    }
    checked(text)?;
    Ok(KelpieSettings::from_section(text).unwrap_or_else(|e| {
        println!("{e}");
        salvage(text)
    }))
}

/// A reread's read: the section whole, as the start reads it, with no salvage
///
/// Empty text is the defaults.
pub(super) fn read_whole(text: &str) -> Result<KelpieSettings, String> {
    if text.trim().is_empty() {
        return Ok(KelpieSettings::default());
    }
    checked(text)?;
    KelpieSettings::from_section(text).map_err(|e| e.to_string())
}

// The text is checked, not the settings, so a mistake elsewhere in the
// section that salvage works around cannot drop the certificate.
fn checked(text: &str) -> Result<(), String> {
    KelpieSettings::check_ca_cert(text).map_err(|e| e.to_string())?;
    KelpieSettings::check_trusted(text).map_err(|e| e.to_string())
}

// A section that does not parse whole: the `[leases]` and `[agent_board]` tables
// and the rest are read apart, so a mistake in one leaves the others standing. A leftover
// `[api_credit]` table is taken out first and named, so the rest of the
// section still reads.
fn salvage(text: &str) -> KelpieSettings {
    let Ok(mut table) = text.parse::<toml::Table>() else {
        println!("cargo-test is held by 3 at a time");
        return KelpieSettings::default();
    };
    let leases = table.remove("leases");
    let board = table.remove("agent_board");
    let credit = table.remove("api_credit");
    let rest = toml::to_string(&table).unwrap_or_default();
    let mut settings = KelpieSettings::from_section(&rest).unwrap_or_default();
    match leases.map(toml::Value::try_into).transpose() {
        Ok(leases) => settings.leases = leases.unwrap_or_default(),
        Err(e) => println!("[leases] is not right ({e}): cargo-test is held by 3 at a time"),
    }
    match board.map(toml::Value::try_into).transpose() {
        Ok(board) => settings.agent_board = board.unwrap_or_default(),
        Err(e) => println!("[agent_board] is not right ({e}): the board ranks no agent first"),
    }
    if credit.is_some() {
        println!("[api_credit] is gone: delete it from the [kelpie] section");
    }
    settings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bad_leases_table_leaves_the_rest_standing() {
        let text = "gpu_metrics_url = \"http://gpu-box:9835/metrics\"\n[leases]\ncargo-test = 0\n";
        let settings = salvage(text);
        assert!(settings.gpu_metrics_url.is_some());
        assert_eq!(settings.leases.cargo_test_capacity().get(), 3);
    }

    #[test]
    fn a_bad_agent_board_list_leaves_the_rest_standing_and_ranks_none() {
        let good = "gpu_metrics_url = \"http://gpu-box:9835/metrics\"\n\
                    [leases]\ncargo-test = 5\n[agent_board]\nprefer = [\"opus\"]\n";
        let settings = read_section(good).unwrap();
        assert_eq!(settings.agent_board.prefer.len(), 1);

        for bad in [
            "prefer = [\"Opus\"]",
            "prefer = \"opus\"",
            "rank = [\"opus\"]",
        ] {
            let text = good.replace("prefer = [\"opus\"]", bad);
            let settings = read_section(&text).unwrap();
            assert!(settings.gpu_metrics_url.is_some(), "{bad}");
            assert_eq!(settings.leases.cargo_test_capacity().get(), 5, "{bad}");
            assert!(settings.agent_board.prefer.is_empty(), "{bad}");
        }
    }

    // The `[api_credit]` table is a removed setting, so the dog names it and
    // reads the rest of the section anyway.
    #[test]
    fn a_leftover_api_credit_table_leaves_the_rest_of_the_section_standing() {
        let text = "gpu_metrics_url = \"http://gpu-box:9835/metrics\"\n\
                    [leases]\ncargo-test = 5\n[api_credit]\nusd = 50\nday = 1\n";
        let settings = read_section(text).unwrap();
        assert!(settings.gpu_metrics_url.is_some());
        assert_eq!(
            settings.leases.cargo_test_capacity().get(),
            5,
            "the table beside it is read"
        );

        let both = "[leases]\ncargo-test = 0\n[api_credit]\nusd = 50\nday = 9\n";
        let settings = read_section(both).unwrap();
        assert_eq!(settings.leases.cargo_test_capacity().get(), 3);
    }

    #[test]
    fn the_start_salvages_a_bad_table_and_a_reread_refuses_it() {
        let text = "gpu_metrics_url = \"http://gpu-box:9835/metrics\"\n[leases]\ncargo-test = 0\n";
        let started = read_section(text).unwrap();
        assert_eq!(started.leases.cargo_test_capacity().get(), 3);
        assert!(started.gpu_metrics_url.is_some());

        let refused = read_whole(text).unwrap_err();
        assert!(refused.contains("leases"), "{refused}");
        assert_eq!(
            read_whole("[leases]\ncargo-test = 5\n")
                .unwrap()
                .leases
                .cargo_test_capacity()
                .get(),
            5
        );
    }

    #[test]
    fn a_ca_cert_that_cannot_be_read_stops_the_dog_naming_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let ca = dir.path().join("s3cr3t-ca.pem");
        let section = format!(
            "[webhook]\nkind = \"ntfy\"\nurl = \"http://ntfy.tail1234.ts.net/t\"\n\
             ca_cert = {:?}\n[leases]\ncargo-test = 5\n",
            ca.display().to_string()
        );
        let err = read_section(&section).unwrap_err();
        assert!(err.contains("`webhook.ca_cert`"), "{err}");
        assert!(!err.contains("s3cr3t"), "{err}");

        std::fs::write(&ca, "-----BEGIN CERTIFICATE-----\n").unwrap();
        let settings = read_section(&section).unwrap();
        assert_eq!(settings.leases.cargo_test_capacity().get(), 5);
    }

    fn refusal(code: RpcErrorCode) -> RequestError {
        RequestError::Rpc(shep_client::shep_core::protocol::RpcError {
            code,
            message: "[kelpie] webhook.token: no secret named ntfy".into(),
            daemon_version: None,
        })
    }

    #[test]
    fn a_secret_the_shepherd_cannot_resolve_stops_the_dog_naming_the_reference() {
        for code in [RpcErrorCode::InvalidConfig, RpcErrorCode::Unsupported] {
            let Asked::Refused(err) = answered(Err(refusal(code))) else {
                panic!("not refused");
            };
            assert!(err.contains("no secret named ntfy"), "{err}");
        }
    }

    #[test]
    fn a_section_that_cannot_be_asked_for_is_unreadable_and_not_refused() {
        for answer in [RequestError::Closed, refusal(RpcErrorCode::NotFound)] {
            assert!(matches!(answered(Err(answer)), Asked::Unreadable(_)));
        }
    }

    #[test]
    fn a_trusted_webhook_with_no_token_stops_the_dog_naming_the_key() {
        let section = "[webhook]\nkind = \"ntfy\"\nurl = \"https://ntfy.example/t\"\n\
                       trusted = true\n[leases]\ncargo-test = 5\n";
        let err = read_section(section).unwrap_err();
        assert!(err.contains("`webhook.trusted`"), "{err}");

        let with_token = section.replace("trusted", "token = \"tk_abc\"\ntrusted");
        assert!(read_section(&with_token).unwrap().webhook.unwrap().trusted);
    }

    #[test]
    fn refused_review_windows_leave_the_leases_standing() {
        let settings =
            salvage("[reviewers.cubic]\nreviews = 20\nhours = 720\n[leases]\ncargo-test = 5\n");
        assert_eq!(settings.leases.cargo_test_capacity().get(), 5);
    }
}
