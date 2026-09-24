//! Execution-only aggregate observer injection over already lowered SQL.
use sqlparser::ast::*;

pub fn instrument(statement: &mut Statement, ticket: Expr) -> usize {
    // Persisted definitions must never capture an execution ticket. DML and
    // stored-view aggregate bodies require additional execution integration.
    if !matches!(statement, Statement::Query(_)) {
        return 0;
    }
    msduck_sql::aggregate_diagnostics::instrument(statement, &ticket, |name| {
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
