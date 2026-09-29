use msduck_sql::{aggregate_columns, dialect::ServerDialect};
use sqlparser::{ast::*, parser::Parser};
fn bind(sql: &str) -> Statement {
    let mut statement = Parser::parse_sql(&ServerDialect, sql).unwrap().remove(0);
    aggregate_columns::resolve(&Default::default(), &mut statement, &Default::default()).unwrap();
    statement
}
#[test]
fn float_values_declarations_reach_aggregate_operands_without_native_effects() {
    for (values, expected) in [
        (
            "(CAST('1e308' AS FLOAT)),(CAST('1e308' AS FLOAT))",
            "float(53)",
        ),
        ("(CAST(0.1 AS REAL)),(CAST(0.2 AS REAL))", "real"),
        (
            "(CAST(0.1 AS FLOAT(24))),(CAST(0.2 AS FLOAT(24)))",
            "float(24)",
        ),
        ("(CAST(1 AS REAL)),(2),(NULL)", "real"),
        ("(CAST(1 AS REAL)),(CAST(2 AS FLOAT))", "float(53)"),
        ("(CAST(1 AS REAL)),(CAST(2 AS DECIMAL(5,2)))", "real"),
    ] {
        for sql in [
            format!("SELECT SUM(v),AVG(v) FROM (VALUES {values}) d(v)"),
            format!(
                "WITH c(v) AS (SELECT v FROM (VALUES {values}) d(v)) SELECT SUM(v),AVG(v) FROM c"
            ),
        ] {
            let output = bind(&sql).to_string().to_lowercase();
            assert!(
                output.contains(&format!("sum(cast(v as {expected}))")),
                "{output}"
            );
            assert!(
                output.contains(&format!("avg(cast(v as {expected}))")),
                "{output}"
            );
        }
    }
}
#[test]
fn unknown_values_stay_unknown_and_volatile_sources_are_not_duplicated() {
    let source = "SELECT SUM(v) FROM (VALUES(CAST(nextval('calls') AS REAL)),(CAST(nextval('calls') AS REAL))) d(v)";
    let output = bind(source).to_string().to_lowercase();
    assert_eq!(output.matches("nextval('calls')").count(), 2);
    for source in [
        "SELECT SUM(v) FROM (VALUES(CAST(1 AS REAL)),(unknown_function())) d(v)",
        "SELECT SUM(v) FROM (VALUES(CAST(1 AS REAL)),(CAST('x' AS XML))) d(v)",
    ] {
        let output = bind(source).to_string().to_lowercase();
        assert!(output.contains("sum(v)"), "{output}");
    }
}
