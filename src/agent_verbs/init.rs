//! `shep kelpie agents init`: bind a whole lineup from kelpie's models file
//!
//! `init <harness>` writes a bound file for the implementer and for every
//! baseline of every role that harness serves, and names each role it skips and
//! what the harness lacks. `init --detect` takes no harness: it finds each CLI
//! on the `PATH`, asks each gateway in kelpie's settings for the models it
//! serves, and gives each role the first harness in the models file's `order`
//! that was found and serves it. Neither writes over a file that is there nor
//! touches a project's settings, so neither prints a `shep kelpie set` line:
//! the agent board picks from the agent files `agents.allow` holds.

use std::path::Path;

use crate::adapters::host::{Host, SystemHost};
use crate::agents::init::{self, Choice, Found, Listed};
use crate::forwarder::Upstream;
use crate::settings::{EndpointUrl, GatewayName, Gateways, Harness, ModelHost};
use crate::shepherd::{self, Socket};

/// What `shep kelpie agents init` takes
pub const USAGE: &str = "usage: shep kelpie agents init <harness> [--gateway <name> | --url <url>]\n       shep kelpie agents init --detect";

/// What `agents init` reads: the models file, and for `--detect` this machine
/// and the gateways in kelpie's own settings
pub struct Setup<'a> {
    /// The models file's text
    pub models: &'a str,
    /// The machine `--detect` looks for each CLI on
    pub host: &'a dyn Host,
    /// The gateways `--detect` asks for their models, empty for `init <harness>`
    pub gateways: Gateways,
}

/// The setup `agents init <args>` runs on
///
/// Only `--detect` reaches the shepherd, for the gateways kelpie's settings hold.
///
/// # Errors
///
/// A message naming why kelpie's shepherd could not be reached.
pub fn setup(args: &[String], shep_home: &Path) -> Result<Setup<'static>, String> {
    let gateways = match args.iter().any(|arg| arg == "--detect") {
        false => Gateways::default(),
        true => shepherd::block_on(async {
            let client = shepherd::connect(&Socket, shep_home)
                .await
                .map_err(|e| e.describe(shep_home))?;
            crate::flock::gateways(client.as_ref()).await
        })?,
    };
    Ok(Setup {
        models: init::MODELS,
        host: &SystemHost,
        gateways,
    })
}

/// What `agents init <args>` prints for `folder`
///
/// # Errors
///
/// A message naming the flag it lacks, the harness it has no rows for, or the
/// file it could not write.
pub fn run(folder: &Path, args: &[String], setup: &Setup<'_>) -> Result<Vec<String>, String> {
    let ask = Ask::read(args)?;
    let file = init::read(setup.models)?;
    match ask.detect {
        true => detect(folder, &file, setup),
        false => one(folder, &file, ask),
    }
}

// `agents init --detect`: what this machine has, and each role the first harness
// in `order` that serves it.
fn detect(folder: &Path, file: &init::File, setup: &Setup<'_>) -> Result<Vec<String>, String> {
    let found = found(setup);
    if found.harnesses.is_empty() && found.gateways.is_empty() {
        return Ok(vec![
            "nothing found: no harness kelpie runs is on the PATH, and kelpie's settings hold no gateway".to_owned(),
        ]);
    }
    let bindings = file.bind(&found);
    let report = init::write(folder, &bindings.choices)?;
    let mut lines = found.lines();
    lines.extend(report.lines());
    lines.extend(bindings.none);
    Ok(lines)
}

// `agents init <harness>`: every row the file holds for it, and each role it skips.
fn one(folder: &Path, file: &init::File, ask: Ask) -> Result<Vec<String>, String> {
    let harness = ask
        .harness
        .expect("`Ask::read` gives one without `--detect`");
    let rows = file.rows_for(harness);
    if rows.is_empty() {
        return Err(format!(
            "{} holds no rows for {}: add them to {}, or name a harness it has rows for: {}",
            init::FILE,
            harness.as_str(),
            init::FILE,
            harnesses()
        ));
    }
    let server = server(&ask, harness)?;
    let choices: Vec<Choice> = rows
        .into_iter()
        .filter(|row| harness.serves(row.role))
        .map(|row| Choice {
            row: row.clone(),
            server: server.clone(),
        })
        .collect();
    let mut report = init::write(folder, &choices)?;
    report.skipped = init::skipped(harness);
    Ok(report.lines())
}

// What `--detect` finds: each harness whose program is on the `PATH`, and each
// gateway kelpie's settings hold with the models it lists. It asks no other
// address, and nothing at all when kelpie's settings hold no gateway.
fn found(setup: &Setup<'_>) -> Found {
    let harnesses = Harness::ALL
        .into_iter()
        .filter(|harness| setup.host.on_path(harness.command()))
        .collect();
    let gateways = setup
        .gateways
        .iter()
        .map(|(name, gateway)| {
            let asked = Listed {
                name: name.clone(),
                models: Vec::new(),
                why: None,
            };
            let upstream = match Upstream::new(&gateway.base()) {
                Ok(upstream) => upstream,
                Err(why) => {
                    return Listed {
                        why: Some(why.to_string()),
                        ..asked
                    };
                }
            };
            // A gateway may list its models to its key alone, so the list is
            // asked with it when this shell has it.
            let upstream = match setup.gateways.key(gateway) {
                Ok(key) => upstream.with_key(key),
                Err(_) => upstream,
            };
            match crate::adapters::port(&setup.gateways, gateway).models(&upstream) {
                Ok(models) => Listed { models, ..asked },
                Err(why) => Listed {
                    why: Some(why),
                    ..asked
                },
            }
        })
        .collect();
    Found {
        harnesses,
        gateways,
    }
}

// The server `--gateway` or `--url` names, for a harness that runs its model on
// one, and nothing for one that runs none.
fn server(ask: &Ask, harness: Harness) -> Result<Option<ModelHost>, String> {
    let named = match (&ask.gateway, &ask.url) {
        (Some(_), Some(_)) => {
            return Err("`--gateway` and `--url` name the same thing: give one".to_owned());
        }
        (Some(name), None) => Some(ModelHost::Gateway(
            GatewayName::try_from(name.clone()).map_err(|e| format!("`--gateway` {e}"))?,
        )),
        (None, Some(url)) => Some(ModelHost::Url(
            EndpointUrl::try_from(url.clone()).map_err(|e| format!("`--url` {e}"))?,
        )),
        (None, None) => None,
    };
    match (harness.facts().needs_model_server, &named) {
        (true, None) => Err(format!(
            "`agents init {}` needs `--gateway <name>` or `--url <url>`: {} runs its model on \
             a server, and a row of {} names which",
            harness.as_str(),
            harness.as_str(),
            init::FILE
        )),
        (false, Some(_)) => Err(format!(
            "`--gateway` and `--url` are for a harness that runs its model on a server: {} \
             takes neither",
            harness.as_str()
        )),
        _ => Ok(named),
    }
}

// What `agents init <args>` was asked for.
struct Ask {
    harness: Option<Harness>,
    detect: bool,
    gateway: Option<String>,
    url: Option<String>,
}

impl Ask {
    fn read(args: &[String]) -> Result<Self, String> {
        let (mut harness, mut detect) = (None, false);
        let (mut gateway, mut url) = (None, None);
        let mut words = args.iter();
        while let Some(word) = words.next() {
            if word == "--detect" {
                detect = true;
                continue;
            }
            let slot = match word.as_str() {
                "--gateway" => &mut gateway,
                "--url" => &mut url,
                _ if word.starts_with('-') => {
                    return Err(format!("`{word}` is not a flag of `agents init`"));
                }
                _ => {
                    if harness.is_some() {
                        return Err(format!("`agents init` takes one harness, not `{word}`"));
                    }
                    harness = Some(
                        named(word)
                            .ok_or_else(|| format!("`{word}` is no harness: {}", harnesses()))?,
                    );
                    continue;
                }
            };
            match words.next().filter(|value| !value.starts_with("--")) {
                Some(value) => *slot = Some(value.clone()),
                None => return Err(format!("`{word}` needs a value")),
            }
        }
        match (harness, detect) {
            (Some(_), true) => Err("`--detect` takes no harness: it finds them itself".to_owned()),
            (None, true) => match (gateway.as_ref(), url.as_ref()) {
                (None, None) => Ok(Self {
                    harness,
                    detect,
                    gateway,
                    url,
                }),
                _ => Err(
                    "`--gateway` and `--url` are for `agents init <harness>`: `--detect` asks the \
                     gateways kelpie's own settings hold, and binds a pi or goose row only where \
                     one of them lists its model"
                        .to_owned(),
                ),
            },
            (Some(_), false) => Ok(Self {
                harness,
                detect,
                gateway,
                url,
            }),
            (None, false) => Err(USAGE.to_owned()),
        }
    }
}

// The harness `word` names, if kelpie runs one.
fn named(word: &str) -> Option<Harness> {
    Harness::ALL.into_iter().find(|h| h.as_str() == word)
}

fn harnesses() -> String {
    Harness::ALL.map(|harness| harness.as_str()).join(", ")
}
