use msduck_sql::{aggregate, aggregate_diagnostics, dialect::ServerDialect};
use sqlparser::{ast::*, parser::Parser};

#[test]
fn float_lowering_keeps_typed_distinct_windows_and_one_observed_operand() {
    for name in ["SUM", "AVG"] {
        for suffix in [
            "",
            " OVER(ORDER BY id ROWS BETWEEN 1 PRECEDING AND CURRENT ROW)",
        ] {
            for distinct in if suffix.is_empty() {
                vec!["", "DISTINCT "]
            } else {
                vec![""]
            } {
                let mut statement = Parser::parse_sql(
                    &ServerDialect,
                    &format!(
                        "SELECT {name}({distinct}CAST(nextval('calls') AS REAL)){suffix} FROM t"
                    ),
                )
                .unwrap()
                .remove(0);
                let Statement::Query(query) = &mut statement else {
                    panic!("query")
                };
                let SetExpr::Select(select) = query.body.as_mut() else {
                    panic!("select")
                };
                let SelectItem::UnnamedExpr(expr) = &mut select.projection[0] else {
                    panic!("expression")
                };
                aggregate::mark(expr, &Default::default()).unwrap();
                let Expr::Function(statistic) = expr else {
                    panic!("scalar")
                };
                assert!(statistic.over.is_none());
                let FunctionArguments::List(args) = &statistic.args else {
                    panic!("arguments")
                };
                let FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Function(values))) =
                    &args.args[0]
                else {
                    panic!("typed list")
                };
                assert_eq!(values.name.to_string(), "list");
                assert_eq!(values.over.is_some(), !suffix.is_empty());
                let FunctionArguments::List(args) = &values.args else {
                    panic!("arguments")
                };
                assert_eq!(
                    args.duplicate_treatment == Some(DuplicateTreatment::Distinct),
                    !distinct.is_empty()
                );
                let count = aggregate_diagnostics::instrument(
                    &mut statement,
                    &Expr::Value(Value::Placeholder("$1".into()).into()),
                    |_| false,
                );
                let rendered = statement.to_string().to_ascii_lowercase();
                assert_eq!(count, 1, "{rendered}");
                assert_eq!(
                    rendered.matches("nextval('calls')").count(),
                    1,
                    "{rendered}"
                );
                assert_eq!(
                    rendered.matches("__msduck_observe_null").count(),
                    1,
                    "{rendered}"
                );
                if !suffix.is_empty() {
                    assert!(
                        rendered.contains("list_count(__msduck_frame)"),
                        "{rendered}"
                    );
                    assert!(
                        rendered.contains("rows between 1 preceding and current row"),
                        "{rendered}"
                    );
                }
            }
        }
    }
}
