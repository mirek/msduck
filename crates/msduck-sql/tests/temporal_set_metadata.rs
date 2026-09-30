use msduck_core::catalog::TypeMetadata;
use msduck_sql::{
    binding_scope::Scope, catalog_snapshot::CatalogSnapshot, expression_metadata::arithmetic,
    projection,
};
use sqlparser::ast::{BinaryOperator, DataType, Ident, ObjectName, Statement};

fn kind(id: u8, scale: u8) -> DataType {
    DataType::Custom(
        ObjectName::from(vec![Ident::new(if id == 42 {
            "datetime2"
        } else {
            "datetimeoffset"
        })]),
        vec![scale.to_string()],
    )
}
fn catalog() -> CatalogSnapshot {
    let mut catalog = CatalogSnapshot::default();
    for (name, id) in [("datetime2", 42), ("datetimeoffset", 43), ("int", 56)] {
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
fn captured_preparations_have_the_same_common_logical_declaration() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../reference/prepared-temporal-sets.json"
    ))
    .unwrap();
    let catalog = catalog();
    for run in fixture["runs"].as_array().unwrap() {
        for record in &run.as_array().unwrap()[2..] {
            let sql = record["sql"].as_str().unwrap();
            let Statement::Query(query) = msduck_sql::batch::parse(sql).unwrap().remove(0) else {
                panic!("query");
            };
            let fields = projection::query_fields(&catalog, &query, &Scope::default()).unwrap();
            let columns = record["preparation"]["sets"][0]["columns"]
                .as_array()
                .unwrap();
            assert_eq!(fields.len(), columns.len(), "{sql}");
            for (field, column) in fields.iter().zip(columns) {
                let info = field.info.as_ref().unwrap();
                assert_eq!(
                    info.system_type_id,
                    Some(match column["type"].as_str().unwrap() {
                        "DateTime2" => 42,
                        "DateTimeOffset" => 43,
                        other => panic!("{other}"),
                    }),
                    "{sql}"
                );
                assert_eq!(
                    u64::from(info.scale.unwrap()),
                    column["scale"].as_u64().unwrap(),
                    "{sql}"
                );
            }
        }
    }
}

#[test]
fn every_temporal_scale_contributes_in_both_operand_orders_and_values() {
    let catalog = catalog();
    for a in [42, 43] {
        for b in [42, 43] {
            for sa in 0..=7 {
                for sb in 0..=7 {
                    let left = kind(a, sa);
                    let right = kind(b, sb);
                    let expected = kind(a.max(b), sa.max(sb));
                    assert_eq!(arithmetic::set_type(&left, &right), Some(expected.clone()));
                    let info = arithmetic::set_info(
                        &catalog,
                        &catalog.cast_info(&left).unwrap(),
                        &catalog.cast_info(&right).unwrap(),
                    )
                    .unwrap();
                    assert_eq!(info, catalog.cast_info(&expected).unwrap());
                    // This route previously required the root RPC row-collapse workaround.
                    let sql = format!("VALUES (CAST(NULL AS {left}),1), (CAST(NULL AS {right}),2)");
                    let Statement::Query(query) = msduck_sql::batch::parse(&sql).unwrap().remove(0)
                    else {
                        panic!("query");
                    };
                    let fields =
                        projection::query_fields(&catalog, &query, &Scope::default()).unwrap();
                    assert_eq!(fields[0].info, Some(info));
                    assert_eq!(fields[1].info.as_ref().unwrap().system_type_id, Some(56));
                }
            }
        }
    }
}

#[test]
fn unknown_alias_invalid_scale_and_arithmetic_barriers_remain() {
    let catalog = catalog();
    let known = catalog.cast_info(&kind(42, 2)).unwrap();
    for invalid in [
        TypeMetadata::default(),
        TypeMetadata {
            user_type_id: Some(5000),
            ..known.clone()
        },
        TypeMetadata {
            scale: None,
            ..known.clone()
        },
        TypeMetadata {
            scale: Some(8),
            ..known.clone()
        },
        TypeMetadata {
            system_type_id: Some(56),
            user_type_id: Some(56),
            ..known.clone()
        },
    ] {
        assert!(arithmetic::set_info(&catalog, &known, &invalid).is_none());
        assert!(arithmetic::set_info(&catalog, &invalid, &known).is_none());
    }
    assert!(arithmetic::set_type(&kind(42, 8), &kind(43, 2)).is_none());
    for op in [
        BinaryOperator::Plus,
        BinaryOperator::Minus,
        BinaryOperator::Multiply,
        BinaryOperator::Divide,
    ] {
        assert!(arithmetic::result(&catalog, &op, &known, &known).is_none());
    }
    assert_eq!(
        arithmetic::set_type(&DataType::SmallInt(None), &DataType::Int(None)),
        Some(DataType::Int(None))
    );
    assert!(arithmetic::set_info(&CatalogSnapshot::default(), &known, &known).is_none());
}
