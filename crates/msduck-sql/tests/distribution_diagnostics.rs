use msduck_sql::{dialect::ServerDialect, ranking};
use sqlparser::{
    ast::{Expr, Function, SelectItem, SetExpr, Statement},
    parser::Parser,
};

fn function(sql: &str) -> Function {
    let Statement::Query(query) = Parser::parse_sql(&ServerDialect, sql).unwrap().remove(0) else {
        panic!("expected query: {sql}")
    };
    let SetExpr::Select(select) = *query.body else {
        panic!("expected select: {sql}")
    };
    let SelectItem::UnnamedExpr(Expr::Function(function)) =
        select.projection.into_iter().next().unwrap()
    else {
        panic!("expected function: {sql}")
    };
    function
}

#[test]
fn distribution_diagnostics_match_pinned_sql_server_reference() {
    // reference/distribution-reference.json: both fresh SQL Server 2025 runs.
    for name in ["PERCENT_RANK", "CUME_DIST"] {
        for (sql, number, message) in [
            (
                format!("SELECT {name}(1) OVER (ORDER BY n)"),
                4114,
                format!("The function '{name}' takes exactly 0 argument(s)."),
            ),
            (
                format!("SELECT {name}()"),
                10753,
                format!("The function '{name}' must have an OVER clause."),
            ),
            (
                format!("SELECT {name}() OVER (PARTITION BY g)"),
                4112,
                format!("The function '{name}' must have an OVER clause with ORDER BY."),
            ),
            (
                format!("SELECT {name}() OVER (ORDER BY n ROWS UNBOUNDED PRECEDING)"),
                10752,
                format!("The function '{name}' may not have a window frame."),
            ),
        ] {
            let error = ranking::validate(&function(&sql)).unwrap_err();
            assert_eq!(error, message, "{sql}");
            assert_eq!(ranking::error_number(&error), Some(number), "{sql}");
        }
    }
}

#[test]
fn other_ranking_diagnostics_remain_unchanged() {
    for (sql, number, message) in [
        (
            "SELECT RANK(1) OVER (ORDER BY n)",
            174,
            "The rank function requires 0 argument(s).",
        ),
        (
            "SELECT ROW_NUMBER() OVER (ORDER BY n ROWS UNBOUNDED PRECEDING)",
            4106,
            "The function 'row_number' may not have a window frame.",
        ),
    ] {
        let error = ranking::validate(&function(sql)).unwrap_err();
        assert_eq!(error, message, "{sql}");
        assert_eq!(ranking::error_number(&error), Some(number), "{sql}");
    }
}
