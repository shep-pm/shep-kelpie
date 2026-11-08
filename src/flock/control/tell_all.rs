//! `shep kelpie tell --all "<note>"`: the note for every project's project
//! manager
//!
//! The API credit and the plans are shared by every project, so a standing note
//! about them has to reach every project manager. Each project is told in name
//! order on the one [`Reach`], so the socket-first path and its one-line
//! fallback hold for the command rather than once per project.

use crate::flock::tables;
use crate::home::ProjectName;
use crate::shepherd::Connection;

use super::{AllProjects, Format, Reach, json_status, no_projects, tell};

/// [`tell`](super::tell) for every project in the shepherd's book, one after the other
///
/// With [`Format::Json`] each project's answer is a line behind its name, as
/// [`status`](super::status) prints them. A project whose runner is stopped, or
/// refused the note, is named in `refused` and does not stop the rest.
///
/// # Errors
///
/// A message when the shepherd cannot list the projects.
pub async fn tell_all(
    reach: &Reach,
    client: &dyn Connection,
    note: &str,
    format: Format,
) -> Result<AllProjects, String> {
    let mut told = AllProjects {
        lines: Vec::new(),
        refused: Vec::new(),
    };
    for sheep in tables(client).await?.into_keys() {
        let project = match ProjectName::try_from(sheep.as_str()) {
            Ok(project) => project,
            Err(why) => {
                told.refused.push(format!("{sheep}: {why}"));
                continue;
            }
        };
        match tell(reach, client, &project, note, format).await {
            Ok(lines) if format == Format::Json => {
                told.lines
                    .extend(lines.iter().map(|body| json_status(&sheep, body)));
            }
            Ok(lines) => told.lines.extend(lines),
            Err(why) => told.refused.push(format!("{sheep}: {why}")),
        }
    }
    if told.lines.is_empty() && told.refused.is_empty() {
        told.lines.push(no_projects());
    }
    Ok(told)
}
