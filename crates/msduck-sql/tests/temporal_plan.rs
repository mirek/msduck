use msduck_core::{
    types::{Scale, Type},
    value::Value as BoundValue,
};
use msduck_sql::{
    datetime2_cast as dt2, datetime2_compare as dt2_compare, datetimeoffset_cast as offset,
    datetimeoffset_compare as offset_compare, dialect::ServerDialect, parameter::Parameter,
};
use sqlparser::{ast::*, parser::Parser};
use std::collections::HashMap;

fn query(sql: &str) -> Box<Query> {
    let Statement::Query(query) = Parser::parse_sql(&ServerDialect, sql).unwrap().remove(0) else {
        panic!("query")
    };
    query
}
fn expression(sql: &str) -> Expr {
    let SetExpr::Select(select) = *query(&format!("SELECT {sql}")).body else {
        panic!("select")
    };
    let SelectItem::UnnamedExpr(value) = select.projection.into_iter().next().unwrap() else {
        panic!("expression")
    };
    value
}

#[test]
fn scale_inference_uses_explicit_parameters_and_temporal_operations() {
    let parameters = HashMap::from([
        (
            "@local".into(),
            Parameter {
                value: BoundValue::Null,
                data_type: Type::DateTime2(Scale::new(2).unwrap()),
            },
        ),
        (
            "@offset".into(),
            Parameter {
                value: BoundValue::Null,
                data_type: Type::DateTimeOffset(Scale::new(5).unwrap()),
            },
        ),
    ]);
    for (sql, local, zoned) in [
        ("@local", Some(2), None),
        ("@offset", None, Some(5)),
        ("DATEADD(second,1,@local)", Some(2), None),
        ("MIN(@offset)", None, Some(5)),
        ("TODATETIMEOFFSET(@local,'+01:00')", None, Some(2)),
        ("SWITCHOFFSET(@offset,'+00:00')", None, Some(5)),
        ("COALESCE(@local,CAST(NULL AS DATETIME2(7)))", Some(7), None),
        ("unbound_column", None, None),
    ] {
        let value = expression(sql);
        assert_eq!(dt2_compare::scale(&value, &parameters), local, "{sql}");
        assert_eq!(offset_compare::scale(&value, &parameters), zoned, "{sql}");
    }
}

#[test]
fn tagged_storage_declarations_round_trip_and_reject_other_structs() {
    for scale in 0..=7 {
        let local = dt2::storage_type(scale);
        let zoned = offset::storage_type(scale);
        assert_eq!(dt2::storage_kind(&local.to_string()), Some(local.clone()));
        assert_eq!(
            offset::storage_kind(&zoned.to_string()),
            Some(zoned.clone())
        );
        assert_eq!(dt2::storage_scale(&local), Some(scale));
        assert_eq!(offset::storage_scale(&zoned), Some(scale));
        assert_eq!(dt2::storage_scale(&zoned), None);
        assert_eq!(offset::storage_scale(&local), None);
        let DataType::Struct(mut fields, bracket) = zoned else {
            panic!("struct")
        };
        fields[1].field_type = DataType::Int(None);
        assert_eq!(
            offset::storage_scale(&DataType::Struct(fields, bracket)),
            None
        );
    }
    for name in [
        "__msduck_datetime2_8",
        "__msduck_datetime2_07",
        "user_field",
    ] {
        assert_eq!(dt2::field_scale(name), None);
    }
    assert_eq!(offset::storage_kind("STRUCT(a BIGINT,b SMALLINT)"), None);
}

#[test]
fn casts_keep_try_mode_scale_and_one_producer_expression() {
    for (name, prefix, lower) in [
        (
            "DATETIME2",
            dt2::PREFIX,
            dt2::lower as fn(&mut Expr) -> Result<(), String>,
        ),
        (
            "DATETIMEOFFSET",
            offset::PREFIX,
            offset::lower as fn(&mut Expr) -> Result<(), String>,
        ),
    ] {
        for (cast, mode) in [("CAST", "cast"), ("TRY_CAST", "try")] {
            let mut value = expression(&format!("{cast}(producer() AS {name}(4))"));
            lower(&mut value).unwrap();
            let Expr::Function(function) = &value else {
                panic!("conversion call")
            };
            assert_eq!(function.name.to_string(), format!("{prefix}{mode}_4"));
            assert_eq!(value.to_string().matches("producer()").count(), 1);
        }
        let mut invalid = expression(&format!("CAST(producer() AS {name}(8))"));
        let before = invalid.clone();
        assert!(lower(&mut invalid).is_err());
        assert_eq!(invalid, before);
    }
    // Repeated equal-scale offset conversion can collapse, but a scale change
    // must retain the intermediate rounding conversion.
    let source = expression("producer()");
    let mut equal = offset::convert(offset::convert(source.clone(), 3), 3);
    offset::lower(&mut equal).unwrap();
    assert_eq!(equal, offset::convert(source.clone(), 3));
    let mut mixed = offset::convert(offset::convert(source, 3), 7);
    let before = mixed.clone();
    offset::lower(&mut mixed).unwrap();
    assert_eq!(mixed, before);
}

#[test]
fn comparison_subquery_wraps_the_complete_relational_input() {
    for wrap in [dt2_compare::subquery, offset_compare::subquery] {
        let mut value = expression("producer()");
        let mut source =
            query("SELECT DISTINCT TOP (1) n FROM t WHERE n IS NOT NULL ORDER BY n DESC");
        let original = source.clone();
        wrap(&mut value, &mut source);
        let SetExpr::Select(select) = source.body.as_ref() else {
            panic!("select")
        };
        let TableFactor::Derived { subquery, .. } = &select.from[0].relation else {
            panic!("derived")
        };
        assert_eq!(subquery, &original);
        assert_eq!(value.to_string().matches("producer()").count(), 1);
    }
}
