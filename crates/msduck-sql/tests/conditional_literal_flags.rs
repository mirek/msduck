use msduck_core::{
    catalog::TypeMetadata,
    result::{Origin, Properties},
};
use msduck_sql::{
    batch, binding_scope::Scope, catalog_snapshot::CatalogSnapshot, projection::query_fields,
};
use serde_json::Value;
use sqlparser::ast::Statement;

fn catalog() -> CatalogSnapshot {
    let mut catalog = CatalogSnapshot::default();
    for (name, id) in [("varchar", 167), ("nvarchar", 231), ("int", 56)] {
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

#[test]
fn captured_folded_literal_flags_use_declarations_not_rows() {
    let catalog = catalog();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../reference/conditional-literal-folding.json"
    ))
    .unwrap();
    let names = [
        "CASE true Unicode",
        "CASE ANSI",
        "mixed family selected ANSI",
        "mixed family selected Unicode",
        "mixed family three branches",
        "mixed family over Unicode bound",
        "mixed family typed NULL",
        "CASE typed NULL",
        "IIF true and false",
        "COALESCE first nonnull",
        "COALESCE typed NULL",
    ];
    for case in fixture["results"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        if !names.contains(&name) {
            continue;
        }
        let Statement::Query(query) = batch::parse(case["query"].as_str().unwrap())
            .unwrap()
            .remove(0)
        else {
            panic!("{name}");
        };
        let fields = query_fields(&catalog, &query, &Scope::default()).unwrap();
        let columns = case["reference"]["sets"][0]["columns"].as_array().unwrap();
        assert_eq!(fields.len(), columns.len(), "{name}");
        for (field, column) in fields.iter().zip(columns) {
            let flags = column["flags"].as_u64().unwrap();
            assert_eq!(
                field.properties,
                Properties {
                    nullable: Some(flags & 1 != 0),
                    origin: if flags & 32 != 0 {
                        Origin::Expression
                    } else {
                        Origin::Stored
                    },
                },
                "{name}: {}",
                field.name
            );
        }
    }
}

#[test]
fn unicode_promotion_changes_nullable_flag_after_four_thousand_characters() {
    let catalog = catalog();
    // Pinned SQL Server 2025 boundary capture also records row lengths:
    // artifacts/compatibility/mixed-literal-boundary-sqlserver-2025.jsonl.
    for (width, nullable) in [(3999, false), (4000, false), (4001, true)] {
        let literal = "a".repeat(width);
        for (kind, sql) in [
            (
                "CASE",
                format!("SELECT CASE WHEN 1=1 THEN '{literal}' ELSE N'x' END"),
            ),
            ("COALESCE", format!("SELECT COALESCE('{literal}',N'x')")),
        ] {
            let Statement::Query(query) = batch::parse(&sql).unwrap().remove(0) else {
                panic!("{kind} {width}");
            };
            let fields = query_fields(&catalog, &query, &Scope::default()).unwrap();
            assert_eq!(fields.len(), 1);
            assert_eq!(
                fields[0].properties,
                Properties::expression(nullable),
                "{kind} {width}"
            );
        }
    }
}
