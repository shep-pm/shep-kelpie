//! What kelpie does with the answer of `shep_handover::boot`

use shep_handover::{Arrival, BootError};

/// The arrival `boot` found, or `None` for a cold start
///
/// A platform with no `execve` answers `Unsupported` whatever the environment
/// holds, and carries no marker, so there is no handover to lose.
///
/// # Errors
///
/// Any `BootError` but `Unsupported`: a handover that was found and could not be
/// adopted leaves nothing to start from.
pub fn arrival<S>(
    result: Result<Option<Arrival<S>>, BootError>,
) -> Result<Option<Arrival<S>>, BootError> {
    match result {
        Err(BootError::Unsupported) => Ok(None),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_platform_with_no_handover_is_a_cold_start() {
        let result = arrival::<()>(Err(BootError::Unsupported));
        assert!(matches!(result, Ok(None)), "{result:?}");
    }

    #[test]
    fn a_marker_that_names_no_handover_stays_an_error() {
        let result = arrival::<()>(Err(BootError::Marker {
            value: "not a pid".to_owned(),
        }));
        assert!(
            matches!(result, Err(BootError::Marker { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn no_marker_is_a_cold_start() {
        let result = arrival::<()>(Ok(None));
        assert!(matches!(result, Ok(None)), "{result:?}");
    }
}
