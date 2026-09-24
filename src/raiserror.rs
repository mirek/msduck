//! Session adapter for explicitly bound RAISERROR arguments. No database calls
//! occur here: an application message must not invalidate a DuckDB transaction.
use crate::parameter::Parameter;
use msduck_core::{
    diagnostic::SqlError,
    raiserror::{Delivery, RaisedMessage, ad_hoc, format::Argument},
    value::Value,
};
use msduck_sql::raiserror::{Call, argument};
use std::collections::HashMap;

pub(crate) fn bind(
    call: &Call<'_>,
    parameters: &HashMap<String, Parameter>,
) -> anyhow::Result<RaisedMessage> {
    let message = argument(call.message, parameters)?;
    let severity = integer(argument(call.severity, parameters)?)?;
    let state = integer(argument(call.state, parameters)?)?;
    let attributes = ad_hoc(severity, state, call.options.seterror);
    if attributes.requires_log && !call.options.log {
        return Ok(RaisedMessage::failure(SqlError::new(
            2754,
            1,
            "Error severity levels greater than 18 can only be specified by members of the sysadmin role, using the WITH LOG option.",
        )));
    }
    // These effects need a session/transport implementation, not a no-op.
    anyhow::ensure!(!call.options.log, "unsupported RAISERROR WITH LOG");
    anyhow::ensure!(!call.options.nowait, "unsupported RAISERROR WITH NOWAIT");
    anyhow::ensure!(!attributes.fatal, "unsupported fatal RAISERROR");
    anyhow::ensure!(
        call.substitutions.len() <= 20,
        "unsupported RAISERROR substitution count above 20"
    );
    let values = call
        .substitutions
        .iter()
        .map(|expression| argument(expression, parameters))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let arguments = values
        .iter()
        .map(|value| match value {
            Value::Null => Argument::Null,
            Value::UTinyInt(value) => Argument::TinyInt(*value),
            Value::SmallInt(value) => Argument::SmallInt(*value),
            Value::Int(value) => Argument::Int(*value),
            Value::BigInt(value) => Argument::BigInt(*value),
            Value::Text(value) => Argument::Text(value),
            Value::Blob(value) => Argument::Binary(value),
            _ => Argument::Incompatible,
        })
        .collect::<Vec<_>>();
    match message {
        Value::Text(template) => Ok(msduck_core::raiserror::formatted_message(
            &template,
            severity,
            state,
            call.options.seterror,
            &arguments,
        )),
        Value::Int(number) if number >= 50000 => Ok(RaisedMessage {
            diagnostic: SqlError::new(
                18054,
                1,
                format!(
                    "Error {number}, severity {severity}, state {state} was raised, but no message with that error number was found in sys.messages. If error is larger than 50000, make sure the user-defined message is added using sp_addmessage."
                ),
            ),
            delivery: Delivery::Error,
            error_number: if attributes.sets_error { number } else { 18054 },
        }),
        _ => anyhow::bail!("unsupported RAISERROR message type or system message catalog lookup"),
    }
}
fn integer(value: Value) -> anyhow::Result<i32> {
    match value {
        Value::UTinyInt(value) => Ok(i32::from(value)),
        Value::SmallInt(value) => Ok(i32::from(value)),
        Value::Int(value) => Ok(value),
        _ => anyhow::bail!("unsupported RAISERROR severity or state type"),
    }
}

#[cfg(test)]
mod tests {
    use crate::{engine::Session, server::Server, tds};
    use msduck_core::diagnostic::SqlError;

    #[test]
    fn messages_preserve_transactions_counters_catch_and_rethrow_units() {
        let server = Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        let parameters = Default::default();
        assert!(
            session
                .batch_response(
                    "CREATE TABLE dbo.raise_guard(n INT)",
                    &parameters,
                    false,
                    None
                )
                .1
        );
        let (_, success) = session.batch_response("BEGIN TRANSACTION; INSERT INTO dbo.raise_guard VALUES(1); RAISERROR(N'message',16,7); INSERT INTO dbo.raise_guard VALUES(@@ERROR); COMMIT", &parameters, false, None);
        assert!(!success);
        let sum: i32 = session
            .db
            .query_row("SELECT SUM(n)::INT FROM dbo.raise_guard", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(sum, 50001);
        assert_eq!(session.transactions, 0);
        let (_, success) = session.batch_response(
            "RAISERROR(N'info',9,7) WITH SETERROR",
            &parameters,
            false,
            None,
        );
        assert!(success);
        assert_eq!((session.last_error, session.rowcount), (50000, 0));
        let (_, success) =
            session.batch_response("RAISERROR(N'%q',16,99,1)", &parameters, true, None);
        assert!(!success);
        assert_eq!(session.last_error, 50000);
        let (_, success) = session.batch_response("BEGIN TRY RAISERROR(N'%q',16,99,1); END TRY BEGIN CATCH INSERT INTO dbo.raise_guard VALUES(ERROR_NUMBER()); END CATCH", &parameters, false, None);
        assert!(success);
        assert_eq!(
            session
                .db
                .query_row(
                    "SELECT COUNT(*)::INT FROM dbo.raise_guard WHERE n=2787",
                    [],
                    |row| row.get::<_, i32>(0)
                )
                .unwrap(),
            1
        );
        let (response, success) = session.batch_response(
            "BEGIN TRY RAISERROR(N'%.1s',16,7,N'🦆x'); END TRY BEGIN CATCH THROW; END CATCH",
            &parameters,
            false,
            None,
        );
        assert!(!success);
        let mut expected = Vec::new();
        for command in [349, 246, 350] {
            tds::done(&mut expected, 0xfd, 1, command, 0);
        }
        tds::sql_error(
            &mut expected,
            &SqlError::from_utf16(50000, 7, 16, vec![0xd83e]),
        );
        assert!(response.starts_with(&expected));
        for (sql, number) in [
            ("RAISERROR(N'x',NULL,1)", 156),
            ("RAISERROR(N'x',16,1,1+2)", 102),
            ("RAISERROR(N'x',16,1,+1)", 102),
            (
                "DECLARE @d DECIMAL(5,2)=1; RAISERROR(N'plain',16,1,@d)",
                2748,
            ),
        ] {
            let (response, success) = session.batch_response(
                &format!("INSERT INTO dbo.raise_guard VALUES(99999); {sql}"),
                &parameters,
                false,
                None,
            );
            assert!(!success);
            assert_eq!(
                i32::from_le_bytes(response[3..7].try_into().unwrap()),
                number
            );
        }
        assert_eq!(
            session
                .db
                .query_row(
                    "SELECT COUNT(*)::INT FROM dbo.raise_guard WHERE n=99999",
                    [],
                    |row| row.get::<_, i32>(0)
                )
                .unwrap(),
            0
        );
        // Preparation binds declarations without formatting or raising anything.
        session
            .validate_prepared_sql("RAISERROR(N'%q',16,1)", &[])
            .unwrap();
    }
}
