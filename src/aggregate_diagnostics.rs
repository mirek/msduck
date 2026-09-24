//! Execution-only aggregate observer injection over already lowered SQL.
use sqlparser::ast::*;

pub fn instrument(statement: &mut Statement, ticket: Expr) -> usize {
    // Persisted definitions must never capture an execution ticket.
    if !matches!(
        statement,
        Statement::Query(_) | Statement::Insert(_) | Statement::Update(_) | Statement::Delete(_)
    ) {
        return 0;
    }
    expressions(statement, &ticket)
}

/// Bind observation only to an AST that will execute in the supplied scope.
/// This is also used by scalar subquery and checked-expression execution.
pub fn bind_expressions<T: VisitMut>(
    node: &mut T,
    diagnostics: &crate::statement_diagnostics::Scope,
    values: &mut Vec<duckdb::types::Value>,
) {
    let ticket = Expr::Value(Value::Placeholder(format!("${}", values.len() + 1)).into());
    if expressions(node, &ticket) > 0 {
        values.push(duckdb::types::Value::Blob(diagnostics.ticket().to_vec()));
    }
}

fn expressions<T: VisitMut>(node: &mut T, ticket: &Expr) -> usize {
    msduck_sql::aggregate_diagnostics::instrument(node, ticket, |name| {
        matches!(
            name,
            "min"
                | "max"
                | "sum"
                | "avg"
                | "count"
                | "stddev_samp"
                | "stddev_pop"
                | "var_samp"
                | "var_pop"
                | "__msduck_sum_int"
                | "__msduck_sum_big"
                | "__msduck_avg_int"
                | "__msduck_avg_big"
                | "__msduck_sum_money"
                | "__msduck_avg_money"
                | "__msduck_min_bin2_unicode"
                | "__msduck_max_bin2_unicode"
                | "__msduck_min_bin2_ansi"
                | "__msduck_max_bin2_ansi"
        ) || name
            .strip_prefix("__msduck_avg_decimal_")
            .and_then(|s| s.parse::<u8>().ok())
            .is_some_and(|s| s <= 38)
    })
}

pub fn append_warning(tokens: &mut Vec<u8>) {
    crate::tds::diagnostic_utf16(
        tokens,
        crate::tds::DiagnosticKind::Information,
        0,
        1,
        8153,
        &"Warning: Null value is eliminated by an aggregate or other SET operation."
            .encode_utf16()
            .collect::<Vec<_>>(),
    );
}

#[cfg(test)]
mod tests {
    use crate::{engine::Session, server::Server};

    #[test]
    fn dml_and_scalar_assignments_share_their_statement_diagnostics() {
        let server = Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        let (tokens, ok) = session.batch_response(
            "CREATE TABLE warning_source(v INT); INSERT INTO warning_source VALUES(1),(NULL),(2); CREATE TABLE warning_sink(v INT)",
            &Default::default(), false, None,
        );
        assert!(ok, "{tokens:?}");
        let message = "Warning: Null value is eliminated by an aggregate or other SET operation."
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        for mode in ["ON", "OFF"] {
            let (_, ok) = session.batch_response(
                &format!("SET ANSI_WARNINGS {mode}"),
                &Default::default(),
                false,
                None,
            );
            assert!(ok);
            for sql in [
                "INSERT INTO warning_sink SELECT MIN(v) FROM warning_source",
                "UPDATE warning_sink SET v=(SELECT MAX(v) FROM warning_source)",
                "DELETE FROM warning_sink WHERE v IN(SELECT MAX(v) FROM warning_source)",
                "UPDATE warning_sink SET v=(SELECT MAX(v) FROM warning_source)",
                "DECLARE @v INT=(SELECT MIN(v) FROM warning_source); SELECT @v",
                "DECLARE @v INT; SET @v=(SELECT MIN(v) FROM warning_source); SELECT @v",
            ] {
                let (tokens, ok) = session.batch_response(sql, &Default::default(), false, None);
                assert!(ok, "{sql}: {tokens:?}");
                assert_eq!(
                    tokens
                        .windows(message.len())
                        .filter(|bytes| *bytes == message)
                        .count(),
                    usize::from(mode == "ON"),
                    "{mode}: {sql}"
                );
            }
            assert_eq!(
                session
                    .db
                    .query_row("SELECT COUNT(*) FROM warning_sink", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
    }
}
