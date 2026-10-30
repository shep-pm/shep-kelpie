//! A runner's `status` answer as the short block `shep kelpie status` prints
//!
//! It reads the answer as the JSON object it is, so a field a build leaves
//! out reads as none, and the object itself stays the runner's to build.

use serde_json::Value;

use crate::ports::Probability;
use crate::state::TriageHeld;

/// `sheep`'s block from `status`, the runner's answer, under a header that
/// carries `marks`, beside the build the installed kelpie names (`installed`)
/// when it can be asked
pub(super) fn block(
    sheep: &str,
    marks: &[String],
    status: &Value,
    installed: Option<&str>,
) -> Vec<String> {
    let mut lines = vec![match marks {
        [] => sheep.to_owned(),
        marks => format!("{sheep} ({})", marks.join(", ")),
    }];
    let run = status["run"].as_str().unwrap_or("running");
    lines.push(format!("  run: {run}"));
    build(status, installed, &mut lines);
    refused(status, &mut lines);
    items(status, &mut lines);
    adopting(status, &mut lines);
    red_workflows(status, &mut lines);
    contributions(status, &mut lines);
    bumps(status, &mut lines);
    triage(status, &mut lines);
    rulings(status, &mut lines);
    pacer(status, &mut lines);
    lines
}

/// What `sheep`'s runner's answer `body` to `action` with `params` says as
/// plain lines, for the verbs that print one: the answer itself when it is
/// not the object that action answers with
pub(super) fn reply(sheep: &str, action: &str, params: Option<&str>, body: &str) -> Vec<String> {
    let Ok(answer) = serde_json::from_str::<Value>(body) else {
        return vec![body.to_owned()];
    };
    let of = || match params {
        Some(ticket) => ticket_name(ticket),
        None => "the open work item".to_owned(),
    };
    match action {
        "timings" => match answer["table"].as_str() {
            Some(table) => table.lines().map(str::to_owned).collect(),
            None => vec![body.to_owned()],
        },
        // An answer that contradicts the action is printed as it came.
        "drain" if answer["draining"].is_object() => {
            vec![draining(sheep, &answer["draining"])]
        }
        "undrain" if answer["draining"].is_null() => {
            vec![format!("{sheep} is not draining: calls start again")]
        }
        "drain" | "undrain" => vec![body.to_owned()],
        "board" => board(sheep, &answer["board"]),
        "gate" => vec![format!("{sheep}: sent {} to the review gate", of())],
        "drop" => vec![format!("{sheep}: dropped {}", of())],
        // The runner answers these with its status, which says nothing of the
        // request: it queued it, and a refusal shows later in its log and in
        // `status`'s `standing`.
        "add" => vec![format!(
            "{sheep}: queued a work item for {}",
            params.map_or_else(|| "?".to_owned(), ticket_name)
        )],
        "triage" => vec![format!(
            "{sheep}: queued {} for triage",
            params.map_or_else(|| "?".to_owned(), ticket_name)
        )],
        "rework" => vec![format!(
            "{sheep}: queued a rework of PR #{}",
            params.unwrap_or("?")
        )],
        "adopt" => vec![format!(
            "{sheep}: queued adoption of PR #{}",
            params.unwrap_or("?")
        )],
        _ => vec![body.to_owned()],
    }
}

// A ticket as the maintainer writes it: an issue number is `#7`, a local
// ticket's id `k12` stands as it is.
fn ticket_name(id: &str) -> String {
    match id.strip_prefix('k') {
        Some(_) => id.to_owned(),
        None => format!("#{id}"),
    }
}

// The board's list, one numbered line an issue.
fn board(sheep: &str, list: &Value) -> Vec<String> {
    let Some(places) = list.as_array() else {
        return vec![list.to_string()];
    };
    if places.is_empty() {
        return vec![format!("{sheep}: no issue is ready on the board")];
    }
    places
        .iter()
        .map(|place| {
            let mut notes = vec![place["priority"].as_str().unwrap_or("no priority")];
            notes.extend(
                place["blocked"]
                    .as_bool()
                    .unwrap_or(false)
                    .then_some("blocked"),
            );
            notes.extend(place["held"].as_bool().unwrap_or(false).then_some("held"));
            format!(
                "{}. {} {} ({})",
                place["position"],
                board_id(&place["issue"]),
                place["title"].as_str().unwrap_or_default(),
                notes.join(", ")
            )
        })
        .collect()
}

// A board line's ticket, as the answer names it: `#284` for a GitHub issue,
// `k12` for a local ticket, and `?` for an answer that names none. A runner
// from before ticket ids writes a number, which reads as GitHub's.
fn board_id(issue: &Value) -> String {
    ticket_id(issue).unwrap_or_else(|| "?".to_owned())
}

fn draining(sheep: &str, draining: &Value) -> String {
    let calls = draining["calls"].as_array().map_or(&[][..], Vec::as_slice);
    if calls.is_empty() {
        return format!("{sheep} is draining: no new call starts, and none is running");
    }
    let running: Vec<String> = calls
        .iter()
        .map(|call| match ticket_id(&call["issue"]) {
            Some(ticket) => format!("{} {ticket}", call["role"].as_str().unwrap_or("?")),
            None => call["role"].as_str().unwrap_or("?").to_owned(),
        })
        .collect();
    format!(
        "{sheep} is draining: no new call starts; still running: {}",
        running.join(", ")
    )
}

// The build this runner runs, and the installed one when it is not that, so a
// runner an upgrade left behind shows up. The names are compared as they
// stand: a build made from changes on top of the installed commit carries
// `-dirty`, and is a different binary from it.

// A name carries nothing beyond the commit, so two builds made from two
// uncommitted trees of one commit carry the same name.
fn build(status: &Value, installed: Option<&str>, lines: &mut Vec<String>) {
    let mine = status["commit"].as_str();
    let other = installed.filter(|&other| mine.is_none_or(|mine| mine != other));
    lines.push(match (mine, other) {
        (Some(mine), Some(other)) => format!("  build: {mine}, not the installed {other}"),
        (Some(mine), None) => format!("  build: {mine}"),
        (None, Some(other)) => {
            format!("  build: not recorded, the installed build is {other}")
        }
        (None, None) => "  build: not recorded".to_owned(),
    });
}

// A settings change the runner refused, which stands until a reread holds.
fn refused(status: &Value, lines: &mut Vec<String>) {
    let refusal = &status["settings_refused"];
    let Some(reason) = refusal["reason"].as_str() else {
        return;
    };
    let when = match refusal["at"].as_u64() {
        Some(at) => format!(
            "{} UTC",
            crate::board::briefing::date_time(crate::ports::Timestamp(at))
        ),
        None => "an unknown time".to_owned(),
    };
    lines.push(format!(
        "  settings change refused at {when}, so the old settings stay: {reason}"
    ));
}

fn items(status: &Value, lines: &mut Vec<String>) {
    let items = status["work_items"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    if items.is_empty() {
        lines.push("  work items: none".to_owned());
        return;
    }
    lines.push("  work items:".to_owned());
    for item in items {
        lines.push(format!(
            "    {} {}: phase {}, turn {}, {}",
            ticket_id(&item["issue"]).unwrap_or_else(|| "?".to_owned()),
            item["title"].as_str().unwrap_or_default(),
            item["phase"]["state"].as_str().unwrap_or("?"),
            item["turn"]["state"].as_str().unwrap_or("?"),
            pull_request(&item["pull_request"]),
        ));
        let failing = &item["failing"];
        if let Some(reason) = failing["reason"].as_str() {
            let since = match failing["since"].as_u64() {
                Some(at) => format!(
                    "{} UTC",
                    crate::board::briefing::date_time(crate::ports::Timestamp(at))
                ),
                None => "an unknown time".to_owned(),
            };
            lines.push(format!(
                "      step failed {} times since {since}: {reason}",
                failing["failures"].as_u64().unwrap_or(0),
            ));
        }
    }
}

// One line for each adopted pull request the board could not start on its
// last poll, which has failed every poll since the time shown
fn adopting(status: &Value, lines: &mut Vec<String>) {
    let skipped = status["skipped"].as_array().map_or(&[][..], Vec::as_slice);
    for skip in skipped.iter().filter(|s| s["reason"] == "adopt") {
        let since = match skip["since"].as_u64() {
            Some(at) => format!(
                "{} UTC",
                crate::board::briefing::date_time(crate::ports::Timestamp(at))
            ),
            None => "an unknown time".to_owned(),
        };
        lines.push(format!(
            "  adopted, cannot start: {} since {since}: {}",
            pull_request(&skip["pull_request"]),
            skip["error"].as_str().unwrap_or("?"),
        ));
    }
}

fn red_workflows(status: &Value, lines: &mut Vec<String>) {
    let red = status["red_workflows"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    if red.is_empty() {
        return;
    }
    lines.push(format!("  red on main: {}", red.len()));
    for workflow in red {
        lines.push(format!(
            "    {}: red for {}, {}; {}",
            workflow["workflow"].as_str().unwrap_or("?"),
            crate::board::briefing::elapsed(workflow["red_for"].as_u64().unwrap_or(0)),
            workflow["url"].as_str().unwrap_or("no link"),
            workflow["doing"].as_str().unwrap_or("nothing yet"),
        ));
    }
}

// One line for each pull request from someone else the board follows, and
// none when it follows none
fn contributions(status: &Value, lines: &mut Vec<String>) {
    let followed = status["contributions"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    if followed.is_empty() {
        return;
    }
    lines.push(format!("  contributions: {}", followed.len()));
    for contribution in followed {
        let author = match contribution["author"].as_str() {
            Some("") | None => "a deleted account",
            Some(login) => login,
        };
        let head = contribution["head"].as_str().unwrap_or_default();
        lines.push(format!(
            "    PR #{} by {author}, head {}",
            contribution["pull_request"],
            head.get(..7).unwrap_or(head),
        ));
        if let Some(clock) = contribution.get("clock") {
            lines.push(format!("      {}", waiting(clock)));
        }
    }
}

// One line for each bump from a bot the board follows, and none when it
// follows none
fn bumps(status: &Value, lines: &mut Vec<String>) {
    let followed = status["bumps"].as_array().map_or(&[][..], Vec::as_slice);
    if followed.is_empty() {
        return;
    }
    lines.push(format!("  bumps: {}", followed.len()));
    for bump in followed {
        let head = bump["head"].as_str().unwrap_or_default();
        lines.push(format!(
            "    PR #{} by {}, head {}: {}",
            bump["pull_request"],
            bump["author"].as_str().unwrap_or("?"),
            head.get(..7).unwrap_or(head),
            bump["standing"].as_str().unwrap_or("?"),
        ));
    }
}

// The call in flight, one line for each issue triage would pick up and one for
// each it commented on, and nothing when it has none of them
fn triage(status: &Value, lines: &mut Vec<String>) {
    let triage = &status["triage"];
    if let Some(issue) = triage["running"].as_u64() {
        lines.push(format!("  triage running: #{issue}"));
    }
    daily(triage, lines);
    let records = triage["issues"].as_array().map_or(&[][..], Vec::as_slice);
    for record in records {
        let (issue, comment) = (&record["issue"], &record["comment"]);
        // A classifier read it as spam: the pass made no call of its own and
        // posted no comment, and the label the maintainer is asked to look at is
        // the whole of it. A call the issue made before the check can still be in
        // the record, which is why the line says "of its own".
        if let Some(p) = spam(record) {
            lines.push(format!(
                "  triaged #{issue}: a classifier read it as {p} spam, so it is labelled \
                 possible-spam with no call of its own"
            ));
            continue;
        }
        // A limit held it: no call was made and no comment was posted, so the
        // line says which limit rather than a comment that is not there.
        if let Some(why) = held(record) {
            lines.push(format!("  triaged #{issue}: no call, {why}"));
            continue;
        }
        match record["duplicate_of"].as_u64() {
            // The step says whether the close the comment announced is already
            // done, so a close still owed is never shown as one that happened.
            Some(of) if record["step"].as_str() == Some("done") => lines.push(format!(
                "  triaged #{issue}: comment {comment}, closed as a duplicate of #{of}"
            )),
            Some(of) => lines.push(format!(
                "  triaged #{issue}: comment {comment}, to close as a duplicate of #{of}"
            )),
            None => lines.push(format!(
                "  triaged #{issue}: comment {comment}, {}",
                record["step"].as_str().unwrap_or("?"),
            )),
        }
    }
    let due = triage["due"].as_array().map_or(&[][..], Vec::as_slice);
    if due.is_empty() {
        return;
    }
    lines.push(format!("  triage due: {}", due.len()));
    for issue in due {
        let author = match issue["author"].as_str() {
            Some("") | None => "a deleted account",
            Some(login) => login,
        };
        lines.push(format!(
            "    #{} {} by {author}",
            issue["issue"],
            issue["title"].as_str().unwrap_or_default(),
        ));
    }
}

// The day's triage calls against `triage.per_day`, and when the next place
// opens once they fill it
fn daily(triage: &Value, lines: &mut Vec<String>) {
    let daily = &triage["daily"];
    let Some(cap) = daily["cap"].as_u64() else {
        return;
    };
    let used = daily["used"].as_u64().unwrap_or(0);
    let waiting = match daily["wait"].as_u64() {
        // Rounded up, so a place that opens in nine minutes is never shown as
        // one that opens in none.
        Some(seconds) => format!(", next place in {}h", seconds.div_ceil(3_600)),
        None => String::new(),
    };
    lines.push(format!("  triage daily: {used} of {cap} used{waiting}"));
}

// The probability a classifier read the issue as spam, and none for a record
// that was researched
fn spam(record: &Value) -> Option<Probability> {
    serde_json::from_value(record["spam"].clone()).ok()
}

// The limit a record says held its issue, in the words the state gives it, or
// none for a record that triaged one
fn held(record: &Value) -> Option<String> {
    let named: TriageHeld = serde_json::from_value(record["held"].clone()).ok()?;
    Some(named.said().to_owned())
}

// What a contribution's clock says: whose move it is and when the nudge is
fn waiting(clock: &Value) -> String {
    let days = |key: &str| clock[key].as_u64().unwrap_or(0);
    let due = clock["due"].as_str();
    let nudge = match due {
        Some(_) => "nudge due".to_owned(),
        None => format!("nudge in {}d", days("days_to_nudge")),
    };
    let quiet = match due {
        Some("quiet") => "quiet".to_owned(),
        _ => format!("quiet in {}d", days("days_to_quiet")),
    };
    let waited = days("waiting_for") / 86_400;
    format!("waiting on the contributor for {waited}d, {nudge}, {quiet}")
}

// A status field's ticket, as the maintainer reads it: `#7`, `k12`, and none
// for a ruling that names no work item. A runner from before ticket ids wrote
// a number, which reads as GitHub's.
fn ticket_id(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(ticket_name(text)),
        Value::Number(number) => number
            .as_u64()
            .filter(|number| *number > 0)
            .map(|number| format!("#{number}")),
        _ => None,
    }
}

// A status field's id as the line shows it: a ticket id is a string, and a
// runner from before wrote it as a number.
fn id(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        _ => "?".to_owned(),
    }
}

fn pull_request(number: &Value) -> String {
    match number.as_u64() {
        Some(number) => format!("PR #{number}"),
        None => "no PR".to_owned(),
    }
}

fn rulings(status: &Value, lines: &mut Vec<String>) {
    let rulings = status["rulings"].as_array().map_or(&[][..], Vec::as_slice);
    if rulings.is_empty() {
        lines.push("  rulings waiting: none".to_owned());
        return;
    }
    lines.push(format!("  rulings waiting: {}", rulings.len()));
    for ruling in rulings {
        lines.push(format!(
            "    ruling {}: {}, {}, {}",
            id(&ruling["id"]),
            ruling["kind"]["kind"].as_str().unwrap_or("?"),
            ticket_id(&ruling["issue"]).unwrap_or_else(|| "no issue".to_owned()),
            pull_request(&ruling["pull_request"]),
        ));
    }
}

fn pacer(status: &Value, lines: &mut Vec<String>) {
    let pacer = &status["pacer"];
    let on = pacer["enabled"].as_bool().unwrap_or(true);
    lines.push(format!("  pacer: {}", if on { "on" } else { "off" }));
    let accounts = pacer.as_object().into_iter().flatten();
    for (account, state) in accounts.filter(|(key, _)| key.as_str() != "enabled") {
        let reading = &state["reading"];
        lines.push(match reading.is_object() {
            true => format!(
                "    {account}: session {}%, week {}%, {}% of the week spent today of {}% allowed",
                reading["session"]["used_pct"],
                reading["week"]["used_pct"],
                reading["spent_today_pct"],
                reading["allowance_pct"],
            ),
            false => format!("    {account}: not read"),
        });
        let hold = &state["holding"];
        if hold.is_object() {
            lines.push(format!(
                "    {account} holding ({}): {}",
                hold["kind"].as_str().unwrap_or("?"),
                hold["reason"].as_str().unwrap_or_default(),
            ));
        }
    }
}
