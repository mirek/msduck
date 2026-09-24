//! SQL error identity, independent of backend, session and transport.

/// Number, severity, state and message of a SQL error.
///
/// Explicit errors retain their supplied identity even when their message matches
/// a built-in error. Backend text recognition belongs to adapters; constructing this value does not classify or rewrite the message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SqlError {
    pub number: i32,
    pub state: u8,
    pub severity: u8,
    pub message: String,
    /// Original UTF-16 when formatting split a surrogate. Display text is lossy
    /// only in that case; wire encoders must use these units.
    pub message_utf16: Option<Vec<u16>>,
}
impl SqlError {
    pub fn new(number: i32, state: u8, message: impl Into<String>) -> Self {
        Self {
            number,
            state,
            severity: 16,
            message: message.into(),
            message_utf16: None,
        }
    }
    pub fn from_utf16(number: i32, state: u8, severity: u8, units: Vec<u16>) -> Self {
        let (message, message_utf16) = match String::from_utf16(&units) {
            Ok(message) => (message, None),
            Err(_) => (String::from_utf16_lossy(&units), Some(units)),
        };
        Self {
            number,
            state,
            severity,
            message,
            message_utf16,
        }
    }

    /// Construct a syntax diagnostic with explicit identity and severity 15.
    pub fn syntax(number: i32, state: u8, message: impl Into<String>) -> Self {
        Self {
            number,
            state,
            severity: 15,
            message: message.into(),
            message_utf16: None,
        }
    }
}
impl std::fmt::Display for SqlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for SqlError {}

pub const TEXT_INT_OVERFLOW: &str = "The conversion of a character value overflowed an int column.";

/// Recognize only canonical messages emitted by our numeric rules. Backend
/// envelopes and explicit application errors must be handled by the adapter.
pub fn numeric(message: &str) -> Option<SqlError> {
    if message == "Arithmetic overflow error converting expression to data type numeric." {
        return Some(SqlError::new(8115, 2, message));
    }
    let number = match message {
        "Divide by zero error encountered." => 8134,
        TEXT_INT_OVERFLOW => 248,
        _ => {
            let target = message
                .strip_prefix("Arithmetic overflow error converting expression to data type ")?
                .strip_suffix('.')?;
            if !matches!(
                target,
                "tinyint"
                    | "smallint"
                    | "int"
                    | "bigint"
                    | "money"
                    | "smallmoney"
                    | "char"
                    | "varchar"
                    | "nchar"
                    | "nvarchar"
            ) {
                return None;
            }
            8115
        }
    };
    Some(SqlError::new(number, 1, message))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn numeric_recognition_requires_complete_canonical_messages() {
        let decimal = "Arithmetic overflow error converting expression to data type numeric.";
        assert_eq!(numeric(decimal), Some(SqlError::new(8115, 2, decimal)));
        for changed in [
            format!("Invalid Input Error: {decimal}"),
            format!("{decimal} trailing text"),
        ] {
            assert!(numeric(&changed).is_none());
        }
        for (message, number) in [
            ("Divide by zero error encountered.", 8134),
            (TEXT_INT_OVERFLOW, 248),
            (
                "Arithmetic overflow error converting expression to data type money.",
                8115,
            ),
            (
                "Arithmetic overflow error converting expression to data type nvarchar.",
                8115,
            ),
        ] {
            assert_eq!(numeric(message), Some(SqlError::new(number, 1, message)));
            for changed in [
                format!("Invalid Input Error: {message}"),
                format!("user text: {message}"),
                format!("{message} trailing text"),
                message.trim_end_matches('.').into(),
            ] {
                assert!(numeric(&changed).is_none(), "{changed}");
            }
        }
        assert!(
            numeric("Arithmetic overflow error converting expression to data type arbitrary.")
                .is_none()
        );
    }
}
