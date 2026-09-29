use msduck_core::{catalog::TypeMetadata, types::Type, value::Value as BoundValue};
use msduck_sql::{
    binding_scope::Scope, catalog_snapshot::CatalogSnapshot, decimal_division,
    dialect::ServerDialect, parameter::Parameter, projection,
};
use sqlparser::{ast::*, parser::Parser};
use std::collections::HashMap;

fn query(sql: &str) -> Box<Query> {
    let Statement::Query(query) = Parser::parse_sql(&ServerDialect, sql).unwrap().remove(0) else {
        panic!("query");
    };
    query
}
fn expression(sql: &str) -> Expr {
    let SetExpr::Select(select) = *query(&format!("SELECT {sql}")).body else {
        panic!("select");
    };
    let SelectItem::UnnamedExpr(value) = select.projection.into_iter().next().unwrap() else {
        panic!("expression");
    };
    value
}
fn catalog() -> CatalogSnapshot {
    let mut catalog = CatalogSnapshot::default();
    for (name, id, precision) in [("decimal", 106, 18), ("numeric", 108, 18), ("int", 56, 10)] {
        catalog.types.insert(
            name.into(),
            TypeMetadata {
                system_type_id: Some(id),
                user_type_id: Some(i32::from(id)),
                precision: Some(precision),
                scale: Some(0),
                ..Default::default()
            },
        );
    }
    catalog
}

#[test]
fn direct_division_projection_uses_contextual_literal_precision() {
    let catalog = catalog();
    for (sql, expected) in [
        ("2147483649/2", (106, 16, 6)),
        ("2147483649/0002", (106, 16, 6)),
        ("2147483649/1000000", (106, 18, 8)),
        ("2/2147483649", (106, 12, 11)),
        ("CAST(2147483649 AS DECIMAL(10,0))/2", (106, 16, 6)),
        ("CAST(2147483649 AS NUMERIC(10,0))/2", (108, 16, 6)),
        ("CAST(2 AS DECIMAL(5,2))/3", (106, 9, 6)),
        ("CAST(2 AS DECIMAL(5,2))/CAST(3 AS INT)", (106, 16, 13)),
        (
            "CAST(2147483649 AS DECIMAL(10,0))/CAST(2 AS INT)",
            (106, 21, 11),
        ),
    ] {
        let fields = projection::query_fields(
            &catalog,
            &query(&format!("SELECT {sql} AS value WHERE 1=0")),
            &Scope::default(),
        )
        .unwrap();
        let info = fields[0].info.as_ref().unwrap();
        assert_eq!(
            (info.system_type_id, info.precision, info.scale),
            (Some(expected.0), Some(expected.1), Some(expected.2)),
            "{sql}"
        );
    }
    let fields = projection::query_fields(
        &catalog,
        &query("SELECT 2147483647/2 AS value"),
        &Scope::default(),
    )
    .unwrap();
    assert_eq!(fields[0].info.as_ref().unwrap().system_type_id, Some(56));
}

#[test]
fn direct_division_lowering_uses_operand_declarations_not_parameter_values() {
    let parameters = HashMap::from([(
        "@divisor".into(),
        Parameter {
            value: BoundValue::Null,
            data_type: Type::Int,
        },
    )]);
    for (sql, expected) in [
        ("2147483649/2", (16, 6)),
        ("2147483649/0002", (16, 6)),
        ("2147483649/1000000", (18, 8)),
        ("2/2147483649", (12, 11)),
        ("CAST(2 AS DECIMAL(5,2))/3", (9, 6)),
        ("CAST(2 AS DECIMAL(5,2))/CAST(3 AS INT)", (16, 13)),
        ("CAST(2147483649 AS DECIMAL(10,0))/@divisor", (21, 11)),
    ] {
        let mut expr = expression(sql);
        decimal_division::lower(&mut expr, &parameters, &|_| None);
        let Expr::Cast { data_type, .. } = &expr else {
            panic!("not lowered: {sql}: {expr}")
        };
        assert_eq!(
            data_type,
            &DataType::Decimal(ExactNumberInfo::PrecisionAndScale(expected.0, expected.1)),
            "{sql}: {expr}"
        );
    }
    let mut integer = expression("2147483647/2");
    let original = integer.clone();
    decimal_division::lower(&mut integer, &parameters, &|_| None);
    assert_eq!(integer, original);
}
