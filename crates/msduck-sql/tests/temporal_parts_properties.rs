use msduck_core::{catalog::TypeMetadata, result::Origin};
use msduck_sql::{
    binding_scope::Scope, catalog_snapshot::CatalogSnapshot, dialect::ServerDialect, projection,
};
use serde_json::Value;
use sqlparser::{ast::Statement, parser::Parser};

fn catalog() -> CatalogSnapshot {
    let mut catalog = CatalogSnapshot::default();
    for (name, id) in [("time", 41), ("datetime2", 42), ("datetimeoffset", 43)] {
        catalog.types.insert(
            name.into(),
            TypeMetadata {
                system_type_id: Some(id),
                user_type_id: Some(i32::from(id)),
                ..Default::default()
            },
        );
    }
    catalog
}

fn projected(sql: &str) -> msduck_sql::binding_scope::Field {
    let Statement::Query(query) = Parser::parse_sql(&ServerDialect, sql).unwrap().remove(0) else {
        panic!("expected SELECT: {sql}");
    };
    let fields = projection::query_fields(&catalog(), &query, &Scope::default()).unwrap();
    assert_eq!(fields.len(), 1, "{sql}");
    fields.into_iter().next().unwrap()
}

fn property_flags(field: &msduck_sql::binding_scope::Field) -> u64 {
    (if field.properties.origin == Origin::Expression {
        32
    } else {
        0
    }) + u64::from(field.properties.nullable != Some(false))
}

#[test]
fn fromparts_properties_match_retained_sql_server_descriptors() {
    let reference: Value =
        serde_json::from_str(include_str!("../../../reference/temporal-parts.json")).unwrap();
    let cases = reference["containers"][0]["runs"][0].as_array().unwrap();
    let mut compared = 0;
    for family in ["time", "datetime2", "datetimeoffset"] {
        for suffix in [
            "scale 3 maximum fraction",
            "scale 3 empty metadata",
            "precision arithmetic",
            "precision division",
            "precision bitwise",
            "precision parentheses",
            "precision cast",
            "bound first",
            "prepared first",
        ] {
            let name = format!("{family} {suffix}");
            let case = cases.iter().find(|case| case["name"] == name).unwrap();
            let expected = case["result"]["sets"][0]["columns"][0]["flags"]
                .as_u64()
                .unwrap();
            let field = projected(case["sql"].as_str().unwrap());
            assert_eq!(property_flags(&field), expected, "{name}");
            compared += 1;
        }
    }
    assert_eq!(compared, 27);
}

#[test]
fn lowered_constructors_keep_the_same_property_rule() {
    for (sql, expected) in [
        ("SELECT __msduck_timefromparts_3(1,2,3,0)", 32),
        ("SELECT __msduck_timefromparts_3(1,2,3,1+2)", 33),
        ("SELECT __msduck_datetime2fromparts_3(2024,1,1,1,2,3,0)", 32),
        (
            "SELECT __msduck_datetime2fromparts_3(@year,1,1,1,2,3,0)",
            33,
        ),
        (
            "SELECT __msduck_datetimeoffsetfromparts_3(2024,1,1,1,2,3,0,0,0)",
            32,
        ),
        (
            "SELECT __msduck_datetimeoffsetfromparts_3(2024,1,1,1,2,3,0,7&3,0)",
            32,
        ),
    ] {
        assert_eq!(property_flags(&projected(sql)), expected, "{sql}");
    }
}

#[test]
fn ordinary_temporal_cast_still_omits_computed_flag() {
    let field = projected("SELECT CAST('12:34' AS TIME) AS value");
    assert_eq!(field.properties.origin, Origin::Derived);
}
