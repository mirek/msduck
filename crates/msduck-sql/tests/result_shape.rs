use msduck_core::{
    character::{Family, Length},
    money::MoneyType,
};
use msduck_sql::{
    dialect::ServerDialect,
    result_types::{self, ResultType},
};
use sqlparser::{ast::*, parser::Parser};

fn parse(sql: &str) -> Statement {
    Parser::parse_sql(&ServerDialect, sql).unwrap().remove(0)
}
fn character(family: Family, length: Length) -> Option<ResultType> {
    Some(ResultType::Character { family, length })
}

#[test]
fn result_shapes_preserve_zero_max_currency_and_time_without_wire_types() {
    for (expr, expected) in [
        ("SPACE(0)", character(Family::Varchar, Length::Bounded(0))),
        (
            "SPACE(99999)",
            character(Family::Varchar, Length::Bounded(8000)),
        ),
        (
            "CAST(NULL AS VARCHAR(MAX))",
            character(Family::Varchar, Length::Max),
        ),
        (
            "CAST(NULL AS NVARCHAR(MAX))",
            character(Family::Nvarchar, Length::Max),
        ),
        (
            "CAST(NULL AS NCHAR)",
            character(Family::Nchar, Length::Bounded(30)),
        ),
        (
            "CAST(NULL AS CHAR(7))",
            character(Family::Char, Length::Bounded(7)),
        ),
        (
            "CAST(NULL AS MONEY)",
            Some(ResultType::Money(MoneyType::Money)),
        ),
        (
            "CAST(NULL AS SMALLMONEY)",
            Some(ResultType::Money(MoneyType::SmallMoney)),
        ),
        ("CAST(NULL AS TIME(2))", Some(ResultType::Time(2))),
        (
            "JSON_VALUE('{}','$.x')",
            character(Family::Nvarchar, Length::Bounded(4000)),
        ),
        (
            "STRING_ESCAPE('x','json')",
            character(Family::Nvarchar, Length::Max),
        ),
    ] {
        let statement = parse(&format!("SELECT {expr} WHERE 1=0"));
        assert_eq!(
            result_types::projection(&statement),
            vec![expected],
            "{expr}"
        );
    }
}

#[test]
fn conditional_and_set_inference_preserve_unknown_barriers() {
    for (sql, expected) in [
        (
            "SELECT CAST('a' AS CHAR(2)) UNION ALL SELECT CAST('b' AS CHAR(5))",
            character(Family::Char, Length::Bounded(5)),
        ),
        (
            "SELECT CAST('a' AS NCHAR(2)) UNION ALL SELECT CAST('b' AS NVARCHAR(5))",
            character(Family::Nvarchar, Length::Bounded(5)),
        ),
        (
            "SELECT CAST('a' AS VARCHAR(2)) UNION ALL SELECT CAST('b' AS VARCHAR(MAX))",
            character(Family::Varchar, Length::Max),
        ),
        (
            "SELECT CAST(NULL AS MONEY) UNION ALL SELECT CAST(NULL AS SMALLMONEY)",
            Some(ResultType::Money(MoneyType::Money)),
        ),
        (
            "SELECT ISNULL(CAST(NULL AS NCHAR(3)),unbound)",
            character(Family::Nchar, Length::Bounded(3)),
        ),
        ("SELECT ISNULL(CAST(NULL AS VARCHAR(3)),unbound)", None),
        ("SELECT COALESCE(CAST(NULL AS CHAR(3)),unbound)", None),
        (
            "SELECT CAST(NULL AS CHAR(3)) UNION ALL SELECT unbound",
            None,
        ),
        // Compatible Unicode operands retain MAX through conditional inference.
        (
            "SELECT COALESCE(CAST(NULL AS NVARCHAR(MAX)),CAST(NULL AS NVARCHAR(3)))",
            character(Family::Nvarchar, Length::Max),
        ),
        (
            "SELECT CASE WHEN 1=1 THEN CAST(NULL AS NCHAR(3)) ELSE NULL END",
            character(Family::Nchar, Length::Bounded(3)),
        ),
    ] {
        assert_eq!(
            result_types::projection(&parse(sql)),
            vec![expected],
            "{sql}"
        );
    }
    assert!(result_types::projection(&parse("SELECT *,CAST(NULL AS CHAR(3)) FROM t")).is_empty());
}

#[test]
fn fixed_padding_preserves_conditions_and_producer_count() {
    let mut statement = parse(
        "SELECT CASE WHEN selector()=1 THEN CAST(producer() AS NCHAR(2)) ELSE CAST(fallback() AS NCHAR(5)) END",
    );
    let original = statement.to_string();
    struct Lower;
    impl VisitorMut for Lower {
        type Break = ();
        fn pre_visit_expr(&mut self, value: &mut Expr) -> std::ops::ControlFlow<()> {
            result_types::lower_fixed_results(value);
            std::ops::ControlFlow::Continue(())
        }
    }
    let _ = VisitMut::visit(&mut statement, &mut Lower);
    for name in ["selector()", "producer()", "fallback()"] {
        assert_eq!(
            statement.to_string().matches(name).count(),
            original.matches(name).count()
        );
    }
    let Statement::Query(query) = statement else {
        panic!("query")
    };
    let SetExpr::Select(select) = *query.body else {
        panic!("select")
    };
    let SelectItem::UnnamedExpr(Expr::Case { conditions, .. }) = &select.projection[0] else {
        panic!("case")
    };
    assert_eq!(conditions[0].condition.to_string(), "selector() = 1");
}
