use msduck_sql::aggregate_diagnostics::instrument;
use sqlparser::{ast::*, dialect::GenericDialect, parser::Parser};

#[test]
fn observer_keeps_aggregate_modifiers_and_uses_operands_once() {
    let mut statement = Parser::parse_sql(&GenericDialect {}, "SELECT MIN(DISTINCT nextval('calls')),MAX(v) OVER(PARTITION BY g ORDER BY i ROWS BETWEEN 1 PRECEDING AND CURRENT ROW),COUNT(*),COUNT(1),LOWER(s) FROM t").unwrap().remove(0);
    let ticket = Expr::Value(Value::Placeholder("$7".into()).into());
    let count = instrument(&mut statement, &ticket, |name| {
        matches!(name, "min" | "max" | "count")
    });
    assert_eq!(count, 3);
    let rendered = statement.to_string();
    assert_eq!(rendered.matches("nextval('calls')").count(), 1);
    assert_eq!(rendered.matches("__msduck_observe_null($7").count(), 3);
    assert!(rendered.contains("MIN(DISTINCT list_extract"));
    assert!(
        rendered
            .contains("OVER (PARTITION BY g ORDER BY i ROWS BETWEEN 1 PRECEDING AND CURRENT ROW)")
    );
    assert!(rendered.contains("COUNT(*)"));
    assert!(rendered.contains("LOWER(s)"));
}

#[test]
fn original_identifiers_are_not_substituted_or_captured() {
    let value = Parser::new(&GenericDialect {})
        .try_with_sql("__msduck_ticket + __msduck_operand + __msduck_null_value")
        .unwrap()
        .parse_expr()
        .unwrap();
    let wrapped = msduck_sql::aggregate_diagnostics::operand(
        value.clone(),
        Expr::Value(Value::Placeholder("$1".into()).into()),
    );
    let sql = wrapped.to_string();
    assert!(sql.contains(&format!("[{}]", value)));
    assert_eq!(sql.matches("__msduck_ticket").count(), 1);
    assert_eq!(sql.matches("__msduck_operand").count(), 1);
}

#[test]
fn windows_observe_the_frame_once_and_keep_empty_count_zero() {
    let mut statement = Parser::parse_sql(
        &GenericDialect {},
        "SELECT MIN(nextval('calls')) OVER w,COUNT(v) OVER w,COUNT(*) OVER w FROM t WINDOW w AS (ORDER BY id ROWS BETWEEN 1 FOLLOWING AND 1 FOLLOWING)",
    ).unwrap().remove(0);
    let ticket = Expr::Value(Value::Placeholder("$9".into()).into());
    assert_eq!(
        instrument(&mut statement, &ticket, |name| matches!(
            name,
            "min" | "count"
        )),
        2
    );
    let sql = statement.to_string();
    assert_eq!(sql.matches("nextval('calls')").count(), 1);
    assert!(sql.contains("[list(nextval('calls')) OVER w]"));
    assert!(sql.contains("list_count(__msduck_frame) < len(__msduck_frame)"));
    assert!(sql.contains("list_aggregate(__msduck_frame, 'MIN')"));
    assert!(sql.contains("COALESCE(list_extract"));
    assert!(sql.contains("CAST(0 AS BIGINT)"));
    assert!(sql.contains("COUNT(*) OVER w"));
    assert!(sql.contains("ROWS BETWEEN 1 FOLLOWING AND 1 FOLLOWING"));
}
