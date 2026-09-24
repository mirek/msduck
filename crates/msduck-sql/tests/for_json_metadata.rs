use msduck_core::for_json::Options;
use msduck_sql::{datalength, dialect::ServerDialect, for_json};
use sqlparser::{ast::*, parser::Parser};

fn query(sql: &str) -> Box<Query> {
    let Statement::Query(query) = Parser::parse_sql(&ServerDialect, sql).unwrap().remove(0) else {
        panic!("query")
    };
    query
}

#[test]
fn original_and_lowered_json_types_are_independent_of_fragment_promotion() {
    for suffix in ["", " ,WITHOUT_ARRAY_WRAPPER", ",ROOT('r')"] {
        let mut statement = Statement::Query(query(&format!(
            "SELECT 1 AS n WHERE 1=0 FOR JSON PATH{suffix}"
        )));
        let Statement::Query(original) = &statement else {
            unreachable!()
        };
        let expression = Expr::Nested(Box::new(Expr::Subquery(original.clone())));
        assert_eq!(
            datalength::kind(&expression, &Default::default(), &|_| None),
            Some(DataType::Nvarchar(Some(CharacterLength::Max)))
        );
        let promoted = for_json::fragment(&expression);
        let options = for_json::take(&mut statement).unwrap().unwrap();
        let Statement::Query(mut lowered) = statement else {
            unreachable!()
        };
        for_json::lower(&mut lowered, vec!["n".into()], "spec".into(), &options).unwrap();
        let expression = Expr::Nested(Box::new(Expr::Subquery(lowered)));
        assert_eq!(
            datalength::kind(&expression, &Default::default(), &|_| None),
            Some(DataType::Nvarchar(Some(CharacterLength::Max)))
        );
        assert_eq!(for_json::fragment(&expression), promoted);
        let mut length = msduck_sql::expr::unary_function("DATALENGTH", expression);
        datalength::lower(&mut length, &Default::default(), &|_| None).unwrap();
        assert!(matches!(
            length,
            Expr::Cast {
                data_type: DataType::BigInt(_),
                ..
            }
        ));
        assert!(length.to_string().contains("__msduck_carrier_datalength"));
    }
}

#[test]
fn unrelated_scalar_queries_remain_unknown() {
    for sql in [
        "SELECT 1 AS n",
        "SELECT N'text' AS n",
        "SELECT missing AS n FROM t",
    ] {
        let expression = Expr::Subquery(query(sql));
        assert_eq!(
            datalength::kind(&expression, &Default::default(), &|_| None),
            None
        );
        assert!(!for_json::unicode_result(&expression));
    }
    let mut lowered = query("SELECT 1 AS n");
    for_json::lower(
        &mut lowered,
        vec!["n".into()],
        "spec".into(),
        &Options {
            without_array_wrapper: true,
            ..Options::default()
        },
    )
    .unwrap();
    assert!(for_json::unicode_result(&Expr::Subquery(lowered.clone())));
    assert!(!for_json::fragment(&Expr::Subquery(lowered)));
}
