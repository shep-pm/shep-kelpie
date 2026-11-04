//! A worker's call that left a record, as a handover finds it

use crate::adapters::process::record::{CallFiles, Record};
use std::io::ErrorKind;

use crate::ports::{Adopted, AgentCall, NotAdopted, Role, Session};
use crate::settings::Harness;

/// What finishes `call` on another image, if it runs now as a worker's call
/// that left a record, or why nothing does
///
/// # Errors
///
/// [`NotAdopted::RecordGone`] when the call's record is missing, and
/// [`NotAdopted::Refused`] when the call is not a worker's or its record
/// cannot be read.
pub(crate) fn adoption_of(call: &AgentCall, harness: Harness) -> Result<Adopted, NotAdopted> {
    if call.role != Role::Worker {
        return Err("only a worker's call is carried".into());
    }
    let record = CallFiles::of(call).record;
    let read = Record::read(&record).map_err(|e| {
        let why = format!("the call's record {} cannot be read: {e}", record.display());
        match e.kind() {
            ErrorKind::NotFound => NotAdopted::RecordGone(why),
            _ => NotAdopted::Refused(why),
        }
    })?;
    Ok(Adopted {
        pid: read.pid,
        record,
        flying: read.flying,
        resumes: match &call.session {
            Session::Resume(id) => Some(id.clone()),
            Session::New(_) => None,
        },
        harness,
        kept: None,
        session: None,
        api: call.harness.bills_api(),
    })
}
