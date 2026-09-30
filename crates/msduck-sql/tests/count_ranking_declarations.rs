use msduck_core::catalog::TypeMetadata;
use msduck_sql::{
    binding_scope::{Field, Scope},
    catalog_snapshot::CatalogSnapshot,
    projection::query_fields,
};
use serde_json::Value;
use sqlparser::ast::{Query, Statement};
fn catalog() -> CatalogSnapshot {
    let mut catalog = CatalogSnapshot::default();
    for (name, id, width, precision) in [
        ("int", 56, 4, 10),
        ("bigint", 127, 8, 19),
        ("nvarchar", 231, 16, 0),
        ("decimal", 106, 17, 38),
    ] {
        catalog.types.insert(
            name.into(),
            TypeMetadata {
                system_type_id: Some(id),
                user_type_id: Some(i32::from(id)),
                max_length: Some(width),
                precision: Some(precision),
                scale: Some(0),
                collation_name: None,
            },
        );
    }
    let fields = [("a", "int"), ("b", "int"), ("label", "nvarchar")]
        .into_iter()
        .map(|(name, kind)| Field {
            name: name.into(),
            info: Some(catalog.types[kind].clone()),
            collation: None,
            json_fragment: false,
            properties: Default::default(),
        })
        .collect();
    catalog.tables.insert("dbo.order_heap".into(), fields);
    catalog
}
fn query(sql: &str) -> Query {
    msduck_sql::batch::parse(sql)
        .unwrap()
        .into_iter()
        .find_map(|statement| match statement {
            Statement::Query(query) => Some(*query),
            _ => None,
        })
        .unwrap()
}
#[test]
fn retained_success_descriptors_are_inferred_without_rows() {
    let reference: Value = serde_json::from_str(include_str!(
        "../../../reference/count-ranking-declarations.json"
    ))
    .unwrap();
    let catalog = catalog();
    let mut checked = 0;
    for record in reference["runs"][0].as_array().unwrap().iter().skip(2) {
        if record["name"].as_str().unwrap().starts_with("prepared ") {
            continue;
        }
        let sql = record["sql"].as_str().unwrap();
        let Statement::Query(raw) =
            sqlparser::parser::Parser::parse_sql(&msduck_sql::dialect::ServerDialect, sql)
                .unwrap()
                .remove(0)
        else {
            panic!("query")
        };
        for ast in [*raw, query(sql)] {
            let original = ast.clone();
            let fields = query_fields(&catalog, &ast, &Scope::default());
            assert_eq!(ast, original);
            if !record["result"]["errors"].as_array().unwrap().is_empty() {
                assert!(
                    fields.is_none_or(|fields| fields.iter().all(|field| field.info.is_none())),
                    "error shape {sql}"
                );
                continue;
            }
            let fields = fields.unwrap();
            let columns = record["result"]["sets"][0]["columns"].as_array().unwrap();
            assert_eq!(fields.len(), columns.len(), "{sql}");
            for (field, column) in fields.iter().zip(columns) {
                let info = field
                    .info
                    .as_ref()
                    .unwrap_or_else(|| panic!("unknown {} in {sql}: {ast:#?}", field.name));
                if column["type"] == "IntN" {
                    assert_eq!(
                        info.max_length.map(i64::from),
                        column["length"].as_i64(),
                        "width {} in {sql}",
                        field.name
                    );
                    assert_eq!(
                        info.system_type_id,
                        Some(if column["length"] == 8 { 127 } else { 56 }),
                        "{sql}"
                    );
                }
                if column["type"] == "DecimalN" {
                    assert!(matches!(info.system_type_id, Some(106 | 108)));
                    assert_eq!(
                        info.precision.map(u64::from),
                        column["precision"].as_u64(),
                        "precision {sql}"
                    );
                    assert_eq!(
                        info.scale.map(u64::from),
                        column["scale"].as_u64(),
                        "scale {sql}"
                    );
                }
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 88);
}
#[test]
fn explicit_parameters_and_unknown_arguments_remain_separate() {
    let catalog = catalog();
    let mut scope = Scope::default();
    scope
        .parameters
        .insert("@p".into(), catalog.types["int"].clone());
    for sql in [
        "SELECT COUNT(@p) AS c,COUNT_BIG(@p) AS d FROM dbo.order_heap WHERE a>@p",
        "SELECT ROW_NUMBER() OVER(ORDER BY a) AS r FROM dbo.order_heap WHERE a>@p ORDER BY r",
    ] {
        let ast = query(sql);
        let fields = query_fields(&catalog, &ast, &scope).unwrap();
        assert!(fields.iter().all(|field| matches!(
            field.info.as_ref().and_then(|info| info.system_type_id),
            Some(56 | 127)
        )));
    }
    for sql in [
        "SELECT COUNT(@missing) FROM dbo.order_heap",
        "SELECT COUNT(CAST(unknown_function(a) AS INT)) FROM dbo.order_heap",
        "SELECT ROW_NUMBER() OVER(ORDER BY 1) FROM dbo.order_heap",
        "SELECT ROW_NUMBER() OVER(ORDER BY CAST(NULL AS INT)) FROM dbo.order_heap",
        "SELECT COUNT(CAST(missing AS INT)) FROM dbo.order_heap",
        "SELECT COUNT((SELECT 1)) FROM dbo.order_heap",
        "SELECT COUNT(COUNT(a)) FROM dbo.order_heap",
        "SELECT COUNT(DISTINCT b) OVER() FROM dbo.order_heap",
        "SELECT COUNT(ROW_NUMBER() OVER(ORDER BY a)) FROM dbo.order_heap",
        "SELECT COUNT(*) OVER(ORDER BY a ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM dbo.order_heap",
        "SELECT ROW_NUMBER() OVER(ORDER BY CAST(missing AS INT)) FROM dbo.order_heap",
    ] {
        let fields = query_fields(&catalog, &query(sql), &scope).unwrap();
        assert!(fields.iter().all(|field| field.info.is_none()), "{sql}");
    }
    let mut missing = catalog.clone();
    missing.types.remove("bigint");
    assert!(
        query_fields(
            &missing,
            &query("SELECT COUNT_BIG(*) FROM dbo.order_heap"),
            &scope
        )
        .unwrap()[0]
            .info
            .is_none()
    );
}

#[test]
fn prepared_metadata_uses_declarations_in_every_captured_phase() {
    let reference: Value = serde_json::from_str(include_str!(
        "../../../reference/count-ranking-declarations.json"
    ))
    .unwrap();
    let catalog = catalog();
    let mut scope = Scope::default();
    scope
        .parameters
        .insert("@p".into(), catalog.types["int"].clone());
    for (name, sql) in [
        (
            "prepared count",
            "SELECT COUNT(@p) AS c,COUNT_BIG(@p) AS d FROM dbo.order_heap WHERE a>@p",
        ),
        (
            "prepared row number",
            "SELECT ROW_NUMBER() OVER(ORDER BY a) AS r FROM dbo.order_heap WHERE a>@p ORDER BY r",
        ),
    ] {
        let Statement::Query(raw) =
            sqlparser::parser::Parser::parse_sql(&msduck_sql::dialect::ServerDialect, sql)
                .unwrap()
                .remove(0)
        else {
            panic!("query")
        };
        for ast in [*raw, query(sql)] {
            let before = ast.clone();
            let fields = query_fields(&catalog, &ast, &scope).unwrap();
            assert_eq!(ast, before);
            for record in reference["runs"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|run| run.as_array().unwrap())
                .filter(|record| record["name"] == name)
            {
                let sets = record["result"]["sets"].as_array().unwrap();
                assert_eq!(sets.len(), 4);
                for set in sets {
                    let columns = set["columns"].as_array().unwrap();
                    assert_eq!(fields.len(), columns.len());
                    for (field, column) in fields.iter().zip(columns) {
                        let info = field.info.as_ref().unwrap();
                        assert_eq!(info.max_length.map(i64::from), column["length"].as_i64());
                        assert_eq!(
                            info.system_type_id,
                            Some(if column["length"] == 8 { 127 } else { 56 })
                        );
                    }
                }
            }
        }
    }
}
