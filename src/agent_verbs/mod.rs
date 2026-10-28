//! `shep kelpie agents new`, `init`, `list` and `migrate`: bind a role's
//! baseline or a whole lineup, and show what is bound
//!
//! `new` writes the agent file that binds a baseline to a harness, model and
//! effort, after the loader has read the text. `init` writes one for every role
//! a harness serves, from kelpie's own models file. `list` shows every agent,
//! then each baseline no file stands for. `migrate` writes into each stub the
//! binding its baseline shipped with.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::agents::baseline::{self, Baseline};
use crate::agents::init::quoted;
use crate::agents::legacy;
use crate::agents::{Agents, Fit, Role, Runs};
use crate::settings::AgentName;
use crate::shep_home;

mod init;

#[cfg(test)]
mod tests;

/// What `shep kelpie agents` takes
pub const USAGE: &str = "usage: shep kelpie agents new <role> --harness <harness> --model <model> --effort <effort> [--baseline <name>] [--name <file name>] [--second-look] [--gateway <name> | --url <url>] [--context <tokens>]\n       shep kelpie agents init <harness> [--gateway <name> | --url <url>]\n       shep kelpie agents init --detect\n       shep kelpie agents list\n       shep kelpie agents migrate";

/// Runs `shep kelpie agents ...`
pub fn main(args: &[String]) -> ExitCode {
    let ran = (|| {
        let shep_home = shep_home::required(shep_home::FLOCK_FIX)?;
        let folder = crate::home::kelpie_home_of(&shep_home).join(crate::agents::FOLDER);
        match args.first().map(String::as_str) {
            Some("init") => {
                let rest = &args[1..];
                init::run(&folder, rest, &init::setup(rest, &shep_home)?)
            }
            _ => run(&folder, args),
        }
    })();
    match ran {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("shep kelpie agents: {message}");
            ExitCode::FAILURE
        }
    }
}

// The lines `agents <args>` prints, with `folder` as kelpie's agents folder.
fn run(folder: &Path, args: &[String]) -> Result<Vec<String>, String> {
    match args {
        [verb, rest @ ..] if verb == "new" => new(folder, rest),
        [verb] if verb == "list" => list(folder),
        [verb] if verb == "migrate" => migrate(folder),
        _ => Err(USAGE.to_owned()),
    }
}

// What `agents new <role> ...` was asked to write.
struct Ask {
    role: Role,
    baseline: Option<String>,
    name: Option<String>,
    harness: String,
    model: String,
    effort: String,
    second_look: bool,
    gateway: Option<String>,
    url: Option<String>,
    context: Option<u64>,
}

impl Ask {
    fn read(args: &[String]) -> Result<Self, String> {
        let [role, flags @ ..] = args else {
            return Err(USAGE.to_owned());
        };
        let role = Role::named(role).ok_or_else(|| format!("`{role}` is not a role"))?;
        let (mut baseline, mut name, mut harness, mut model, mut effort) =
            (None, None, None, None, None);
        let (mut second_look, mut gateway, mut url, mut context) = (false, None, None, None);
        let mut flags = flags.iter();
        while let Some(flag) = flags.next() {
            if flag == "--second-look" {
                second_look = true;
                continue;
            }
            let slot = match flag.as_str() {
                "--baseline" => &mut baseline,
                "--name" => &mut name,
                "--harness" => &mut harness,
                "--model" => &mut model,
                "--effort" => &mut effort,
                "--gateway" => &mut gateway,
                "--url" => &mut url,
                "--context" => &mut context,
                _ => return Err(format!("`{flag}` is not a flag of `agents new`")),
            };
            match flags.next().filter(|value| !value.starts_with("--")) {
                Some(value) => *slot = Some(value.clone()),
                None => return Err(format!("`{flag}` needs a value")),
            }
        }
        let need = |value: Option<String>, flag: &str| {
            value.ok_or_else(|| format!("`agents new` needs `{flag}`"))
        };
        if gateway.is_some() && url.is_some() {
            return Err("`--gateway` and `--url` name the same thing: give one".to_owned());
        }
        let context =
            match context {
                Some(tokens) => Some(tokens.parse().map_err(|_| {
                    format!("`--context` takes a number of tokens, not `{tokens}`")
                })?),
                None => None,
            };
        Ok(Self {
            role,
            baseline,
            name,
            harness: word(need(harness, "--harness")?, "--harness")?,
            model: need(model, "--model")?,
            effort: word(need(effort, "--effort")?, "--effort")?,
            second_look,
            gateway,
            url,
            context,
        })
    }

    // The baseline the file extends: None for an implementer, which has none.
    fn baseline(&self) -> Result<Option<&'static Baseline>, String> {
        let own = baseline::of_role(self.role);
        let names = || own.iter().map(|b| b.name).collect::<Vec<_>>().join(", ");
        let role = self.role.as_str();
        match (&self.baseline, own.as_slice()) {
            (Some(given), []) => Err(format!("the {role} role has no baseline, so not `{given}`")),
            (None, []) => Ok(None),
            (Some(given), _) => match own.iter().find(|b| b.name == given) {
                Some(found) => Ok(Some(found)),
                None => Err(format!(
                    "`{given}` is not a baseline of the {role} role: its baselines are {}",
                    names()
                )),
            },
            (None, [only]) => Ok(Some(only)),
            (None, _) => Err(format!(
                "the {role} role has several baselines: give one with `--baseline`: {}",
                names()
            )),
        }
    }

    // The file's name: `--name`, else its baseline's.
    fn file_name(&self, chosen: &Result<Option<&Baseline>, String>) -> Result<String, String> {
        let name = match (&self.name, chosen) {
            (Some(name), _) => name.clone(),
            (None, Ok(Some(baseline))) => baseline.name.to_owned(),
            (None, Ok(None)) => return Err("an implementer has no baseline: give `--name`".into()),
            (None, Err(why)) => return Err(why.clone()),
        };
        AgentName::try_from(name.clone()).map_err(|e| e.to_string())?;
        Ok(name)
    }

    // The file's text. Over a file that binds nothing, `old`, it keeps that
    // file's body and the keys it holds besides these, and with no baseline
    // to choose it keeps the file's own `extends` or `role`.
    fn text(
        &self,
        chosen: Result<Option<&Baseline>, String>,
        old: Option<&str>,
    ) -> Result<String, String> {
        let old = old.map(Old::parse);
        let kept_identity = |keys: &[&str]| -> String {
            old.iter()
                .flat_map(|old| old.blocks.iter())
                .filter(|(key, _)| keys.contains(&key.as_str()))
                .map(|(_, block)| block.as_str())
                .collect()
        };
        let identity = match (chosen, &old) {
            (Ok(Some(baseline)), _) => format!("extends: kelpie:{}\n", baseline.name),
            (Ok(None), _) => format!("role: {}\n", self.role.as_str()),
            (Err(_), Some(_)) if self.baseline.is_none() => kept_identity(&["extends", "role"]),
            (Err(why), _) => return Err(why),
        };
        let mut text = format!(
            "---\n{identity}harness: {}\nmodel: {}\neffort: {}\n",
            self.harness,
            quoted(&self.model),
            self.effort
        );
        if self.second_look {
            text.push_str("second_look: true\n");
        }
        if let Some(gateway) = &self.gateway {
            text.push_str(&format!("gateway: {}\n", quoted(gateway)));
        }
        if let Some(url) = &self.url {
            text.push_str(&format!("url: {}\n", quoted(url)));
        }
        if let Some(tokens) = self.context {
            text.push_str(&format!("context: {tokens}\n"));
        }
        let replaced = [
            "extends",
            "role",
            "harness",
            "model",
            "effort",
            "second_look",
            "gateway",
            "url",
            "context",
        ];
        if let Some(old) = &old {
            for (key, block) in &old.blocks {
                if !replaced.contains(&key.as_str()) {
                    text.push_str(block);
                }
            }
        }
        text.push_str("---\n");
        if let Some(old) = &old {
            text.push_str(&old.body);
        }
        Ok(text)
    }
}

// An agent file split into the blocks of its frontmatter and its body.
struct Old {
    // Each top-level key with the lines under it, and any lines before the first key under "".
    blocks: Vec<(String, String)>,
    body: String,
}

impl Old {
    fn parse(text: &str) -> Self {
        let Some(rest) = text.strip_prefix("---\n") else {
            return Self {
                blocks: Vec::new(),
                body: text.to_owned(),
            };
        };
        let mut blocks: Vec<(String, String)> = Vec::new();
        let mut at = 0;
        let mut body = "";
        for line in rest.split_inclusive('\n') {
            at += line.len();
            if line.trim_end() == "---" {
                body = &rest[at..];
                break;
            }
            let key = match line.starts_with(|c: char| !c.is_whitespace() && c != '#' && c != '-') {
                true => line.split_once(':').map(|(key, _)| key.trim().to_owned()),
                false => None,
            };
            match (key, blocks.last_mut()) {
                (Some(key), _) => blocks.push((key, line.to_owned())),
                (None, Some((_, block))) => block.push_str(line),
                (None, None) => blocks.push((String::new(), line.to_owned())),
            }
        }
        Self {
            blocks,
            body: body.to_owned(),
        }
    }
}

// `value` if it is one lowercase word, since it is written as it stands.
fn word(value: String, flag: &str) -> Result<String, String> {
    match value
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        true => Ok(value),
        false => Err(format!("`{flag}` takes one lowercase word, not `{value}`")),
    }
}

fn new(folder: &Path, args: &[String]) -> Result<Vec<String>, String> {
    let ask = Ask::read(args)?;
    let chosen = ask.baseline();
    let name = ask.file_name(&chosen)?;
    let path = folder.join(format!("{name}.md"));
    let stub = path.exists() && is_unbound(folder, &path)?;
    let old = match stub {
        true => Some(fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?),
        false => None,
    };
    let text = ask.text(chosen, old.as_deref())?;
    baseline::check(&name, &text)?;
    fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let mut file = OpenOptions::new();
    file.write(true);
    match stub {
        true => file.truncate(true),
        false => file.create_new(true),
    };
    let written = file
        .open(&path)
        .and_then(|mut f| f.write_all(text.as_bytes()));
    written.map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => already(&name),
        _ => format!("{}: {e}", path.display()),
    })?;
    Ok(vec![format!("wrote {name}.md")])
}

fn already(name: &str) -> String {
    format!(
        "`{name}.md` already exists, and `agents new` writes over only a file that binds nothing"
    )
}

// Whether the file at `path` is one the loader leaves out for binding nothing.
fn is_unbound(folder: &Path, path: &PathBuf) -> Result<bool, String> {
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let book = Agents::load(folder).map_err(|e| e.to_string())?;
    match book.unbound().iter().any(|u| &u.path == path) {
        true => Ok(true),
        false => Err(already(name)),
    }
}

/// What `agents migrate` prints for the stubs in `folder`: a line for each file written, then
/// each skipped, else `nothing to migrate`
///
/// # Errors
///
/// A message naming the folder or file that could not be read or written.
pub fn migrate(folder: &Path) -> Result<Vec<String>, String> {
    let report = legacy::migrate(folder).map_err(|e| e.to_string())?;
    let lines = report.lines();
    Ok(if lines.is_empty() {
        vec!["nothing to migrate".to_owned()]
    } else {
        lines
    })
}

fn list(folder: &Path) -> Result<Vec<String>, String> {
    let book = Agents::load(folder).map_err(|e| e.to_string())?;
    Ok(lines(&book))
}

// The tier, the kinds when the file sets them and the description when it has
// one, each after a comma, on one line.
fn fit_keys(fit: &Fit) -> String {
    let mut keys = format!(", tier {}", fit.tier().as_str());
    if let Some(kinds) = &fit.kinds {
        let kinds: Vec<&str> = kinds.iter().collect();
        keys.push_str(&format!(", kinds [{}]", kinds.join(", ")));
    }
    if let Some(description) = &fit.description {
        let words: Vec<&str> = description.as_str().split_whitespace().collect();
        keys.push_str(&format!(", description: {}", words.join(" ")));
    }
    keys
}

// A line for each agent, each unbound file, then each baseline that binds
// nothing and has no file.
fn lines(book: &Agents) -> Vec<String> {
    let mut lines = Vec::new();
    for (name, agent) in book.iter() {
        let role = agent.role.as_str();
        let mut line = match &agent.runs {
            Runs::Session { model, .. } => format!(
                "{name}: {role}, {}, {}, {}",
                model.harness.harness().as_str(),
                model.model.as_str(),
                model.effort.as_str()
            ),
            Runs::Command(_) => format!("{name}: {role}, command"),
            Runs::Endpoint(_) => format!("{name}: {role}, endpoint"),
            Runs::Bot(_) => format!("{name}: {role}, review bot"),
        };
        if matches!(agent.role, Role::Implementer | Role::Reviewer) {
            line.push_str(&fit_keys(&agent.fit));
        }
        lines.push(line);
    }
    for unbound in book.unbound() {
        lines.push(format!(
            "{}: {}, unbound: run `{}`",
            unbound.name,
            unbound.role.as_str(),
            unbound.command()
        ));
    }
    for baseline in book.bare_baselines() {
        let Some(role) = baseline.role() else {
            continue;
        };
        let has_file = |u: &baseline::Unbound| u.name.as_str() == baseline.name;
        if book.unbound().iter().any(has_file) {
            continue;
        }
        let command = baseline::new_command(role, Some(baseline.name), baseline.name);
        lines.push(format!(
            "{}: baseline, no file: run `{command}`",
            baseline.name
        ));
    }
    lines
}
