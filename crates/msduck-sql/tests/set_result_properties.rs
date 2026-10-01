use msduck_core::{
    catalog::TypeMetadata,
    result::{Origin, Properties},
};
use msduck_sql::{
    binding_scope::{Field, Scope},
    catalog_snapshot::CatalogSnapshot,
    projection,
};
use sqlparser::ast::Statement;

fn catalog() -> CatalogSnapshot {
    let mut catalog = CatalogSnapshot::default();
    for (name, id, len) in [
        ("int", 56, 4),
        ("datetime2", 42, 8),
        ("datetimeoffset", 43, 10),
    ] {
        catalog.types.insert(
            name.into(),
            TypeMetadata {
                system_type_id: Some(id),
                user_type_id: Some(i32::from(id)),
                max_length: Some(len),
                ..Default::default()
            },
        );
    }
    for (table, name, nullable) in [
        ("dbo.prepare_heap", "a", Some(true)),
        ("dbo.prepare_required", "n", Some(false)),
        ("dbo.unknown", "n", None),
    ] {
        catalog.tables.insert(
            table.into(),
            vec![Field {
                name: name.into(),
                info: catalog.types.get("int").cloned(),
                collation: None,
                json_fragment: false,
                properties: Properties {
                    nullable,
                    origin: if nullable.is_none() {
                        Origin::Unknown
                    } else {
                        Origin::Stored
                    },
                },
            }],
        );
    }
    catalog
}
fn properties(catalog: &CatalogSnapshot, sql: &str) -> Properties {
    let Statement::Query(query) = msduck_sql::batch::parse(sql).unwrap().remove(0) else {
        panic!("query")
    };
    projection::query_fields(catalog, &query, &Scope::default()).unwrap()[0].properties
}

#[test]
fn captured_set_descriptors_preserve_nullability_and_left_origin() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../reference/prepared-set-properties.json"
    ))
    .unwrap();
    let catalog = catalog();
    for run in fixture["runs"].as_array().unwrap() {
        for record in &run.as_array().unwrap()[2..] {
            let sql = record["sql"].as_str().unwrap();
            let flags = record["preparation"]["sets"][0]["columns"][0]["flags"]
                .as_u64()
                .unwrap();
            let actual = properties(&catalog, sql);
            assert_eq!(actual.nullable, Some(flags & 1 != 0), "{sql}");
            assert_eq!(
                actual.origin,
                if flags & 32 != 0 {
                    Origin::Expression
                } else if flags & 8 != 0 {
                    Origin::Stored
                } else {
                    Origin::Derived
                },
                "{sql}"
            );
        }
    }
}

#[test]
fn unknown_properties_remain_unknown_except_proven_nonnull_intersection() {
    let catalog = catalog();
    for op in ["EXCEPT", "INTERSECT", "UNION ALL"] {
        let sql = format!("SELECT n FROM dbo.unknown {op} SELECT a FROM dbo.prepare_heap");
        let actual = properties(&catalog, &sql);
        assert_eq!(
            actual.nullable,
            if op == "UNION ALL" { Some(true) } else { None }
        );
        assert_eq!(
            actual.origin,
            if op == "UNION ALL" {
                Origin::Derived
            } else {
                Origin::Unknown
            }
        );
    }
    assert_eq!(
        properties(
            &catalog,
            "SELECT n FROM dbo.unknown INTERSECT SELECT n FROM dbo.prepare_required"
        )
        .nullable,
        Some(false)
    );
    assert_eq!(
        properties(
            &catalog,
            "SELECT n FROM dbo.unknown EXCEPT SELECT n FROM dbo.prepare_required"
        )
        .nullable,
        None
    );
}
