//! Deterministic attributes for an ad-hoc RAISERROR message.
//! Message lookup, permissions and connection effects live elsewhere.

pub mod format;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Information,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attributes {
    pub delivery: Delivery,
    pub severity: u8,
    pub state: u8,
    pub sets_error: bool,
    pub catchable: bool,
    pub fatal: bool,
    pub requires_log: bool,
}

/// Normalize attributes for a string message (number 50000).
/// Catalog-message severity -1 must be resolved before applying these rules.
/// LOG permission checks must happen before delivering the resulting event.
pub fn ad_hoc(severity: i32, state: i32, set_error: bool) -> Attributes {
    let severity = severity.clamp(0, 25) as u8;
    Attributes {
        delivery: if severity <= 10 {
            Delivery::Information
        } else {
            Delivery::Error
        },
        severity: if severity == 10 { 0 } else { severity },
        state: if state < 0 { 1 } else { state as u8 },
        sets_error: set_error || severity > 10,
        catchable: (11..=19).contains(&severity),
        fatal: severity >= 20,
        requires_log: severity >= 19,
    }
}

/// A delivered diagnostic and its separate statement counter identity.
#[derive(Clone, Debug)]
pub struct RaisedMessage {
    pub diagnostic: crate::diagnostic::SqlError,
    pub delivery: Delivery,
    pub error_number: i32,
}
impl RaisedMessage {
    pub fn failure(diagnostic: crate::diagnostic::SqlError) -> Self {
        Self {
            error_number: diagnostic.number,
            diagnostic,
            delivery: Delivery::Error,
        }
    }
}

pub fn formatted_message(
    template: &str,
    severity: i32,
    state: i32,
    set_error: bool,
    arguments: &[format::Argument<'_>],
) -> RaisedMessage {
    let attributes = ad_hoc(severity, state, set_error);
    match format::message(template, arguments) {
        Ok(units) => RaisedMessage {
            diagnostic: crate::diagnostic::SqlError::from_utf16(
                50000,
                attributes.state,
                attributes.severity,
                units,
            ),
            delivery: attributes.delivery,
            error_number: if attributes.sets_error { 50000 } else { 0 },
        },
        Err(diagnostic) => RaisedMessage {
            error_number: if attributes.sets_error {
                50000
            } else {
                diagnostic.number
            },
            diagnostic,
            delivery: Delivery::Error,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_reference_message_attributes_and_error_counter_rules() {
        for (severity, state, set_error, wire_severity, wire_state, sets_error, catchable) in [
            (0, 1, false, 0, 1, false, false),
            (9, 2, false, 9, 2, false, false),
            (10, 3, false, 0, 3, false, false),
            (11, 4, false, 11, 4, true, true),
            (16, 5, false, 16, 5, true, true),
            (-1, -1, false, 0, 1, false, false),
            (16, 256, false, 16, 0, true, true),
            (0, 6, true, 0, 6, true, false),
        ] {
            let actual = ad_hoc(severity, state, set_error);
            assert_eq!(actual.severity, wire_severity);
            assert_eq!(actual.state, wire_state);
            assert_eq!(actual.sets_error, sets_error);
            assert_eq!(actual.catchable, catchable);
            assert_eq!(
                actual.delivery,
                if wire_severity <= 10 {
                    Delivery::Information
                } else {
                    Delivery::Error
                }
            );
            assert!(!actual.fatal);
            assert!(!actual.requires_log);
        }
    }
}
