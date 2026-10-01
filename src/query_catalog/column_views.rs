//! Captured column-view declarations; no backend row types establish these facts.
use super::Field;
use msduck_core::{
    catalog::TypeMetadata,
    collation::Label,
    result::{Origin, Properties},
};

pub(super) fn fields(view: &str, catalog_collation: &str) -> Option<Vec<Field>> {
    // name, system/user type, bytes, precision, scale, nullable, computed,
    // database-owned sysname versus fixed resource collation.
    let definitions = match view.to_ascii_lowercase().as_str() {
        "columns" => vec![
            ("object_id", 56, 56, 4, 10, 0, false, false, None),
            ("name", 231, 256, 256, 0, 0, true, false, Some("catalog")),
            ("column_id", 56, 56, 4, 10, 0, false, false, None),
            ("system_type_id", 48, 48, 1, 3, 0, false, false, None),
            ("user_type_id", 56, 56, 4, 10, 0, false, false, None),
            ("max_length", 52, 52, 2, 5, 0, false, false, None),
            ("precision", 48, 48, 1, 3, 0, false, false, None),
            ("scale", 48, 48, 1, 3, 0, false, false, None),
            (
                "collation_name",
                231,
                256,
                256,
                0,
                0,
                true,
                true,
                Some("catalog"),
            ),
            ("is_nullable", 104, 104, 1, 1, 0, true, true, None),
            ("is_ansi_padded", 104, 104, 1, 1, 0, false, true, None),
            ("is_rowguidcol", 104, 104, 1, 1, 0, false, true, None),
            ("is_identity", 104, 104, 1, 1, 0, false, true, None),
            ("is_computed", 104, 104, 1, 1, 0, true, true, None),
            ("is_filestream", 104, 104, 1, 1, 0, false, true, None),
            ("is_replicated", 104, 104, 1, 1, 0, true, true, None),
            ("is_non_sql_subscribed", 104, 104, 1, 1, 0, true, true, None),
            ("is_merge_published", 104, 104, 1, 1, 0, true, true, None),
            ("is_dts_replicated", 104, 104, 1, 1, 0, true, true, None),
            ("is_xml_document", 104, 104, 1, 1, 0, false, true, None),
            ("xml_collection_id", 56, 56, 4, 10, 0, false, false, None),
            ("default_object_id", 56, 56, 4, 10, 0, false, false, None),
            ("rule_object_id", 56, 56, 4, 10, 0, false, false, None),
            ("is_sparse", 104, 104, 1, 1, 0, true, true, None),
            ("is_column_set", 104, 104, 1, 1, 0, true, true, None),
            ("generated_always_type", 48, 48, 1, 3, 0, true, true, None),
            (
                "generated_always_type_desc",
                231,
                231,
                120,
                0,
                0,
                true,
                true,
                Some("Latin1_General_CI_AS_KS_WS"),
            ),
            ("encryption_type", 56, 56, 4, 10, 0, true, true, None),
            (
                "encryption_type_desc",
                231,
                231,
                128,
                0,
                0,
                true,
                true,
                Some("Latin1_General_CI_AS_KS_WS"),
            ),
            (
                "encryption_algorithm_name",
                231,
                256,
                256,
                0,
                0,
                true,
                true,
                Some("Latin1_General_CI_AS_KS_WS"),
            ),
            (
                "column_encryption_key_id",
                56,
                56,
                4,
                10,
                0,
                true,
                false,
                None,
            ),
            (
                "column_encryption_key_database_name",
                231,
                256,
                256,
                0,
                0,
                true,
                true,
                Some("catalog"),
            ),
            ("is_hidden", 104, 104, 1, 1, 0, true, true, None),
            ("is_masked", 104, 104, 1, 1, 0, false, true, None),
            ("graph_type", 56, 56, 4, 10, 0, true, true, None),
            (
                "graph_type_desc",
                231,
                231,
                120,
                0,
                0,
                true,
                false,
                Some("Latin1_General_CI_AS_KS_WS"),
            ),
            (
                "is_data_deletion_filter_column",
                104,
                104,
                1,
                1,
                0,
                true,
                true,
                None,
            ),
            (
                "ledger_view_column_type",
                56,
                56,
                4,
                10,
                0,
                true,
                true,
                None,
            ),
            (
                "ledger_view_column_type_desc",
                231,
                231,
                120,
                0,
                0,
                true,
                false,
                Some("Latin1_General_CI_AS_KS_WS"),
            ),
            (
                "is_dropped_ledger_column",
                104,
                104,
                1,
                1,
                0,
                true,
                true,
                None,
            ),
            ("vector_dimensions", 56, 56, 4, 10, 0, true, true, None),
            ("vector_base_type", 48, 48, 1, 3, 0, true, true, None),
            (
                "vector_base_type_desc",
                231,
                231,
                20,
                0,
                0,
                true,
                true,
                Some("Latin1_General_CI_AS_KS_WS"),
            ),
        ],
        "identity_columns" => vec![
            ("object_id", 56, 56, 4, 10, 0, false, false, None),
            ("name", 231, 256, 256, 0, 0, true, false, Some("catalog")),
            ("column_id", 56, 56, 4, 10, 0, false, false, None),
            ("system_type_id", 48, 48, 1, 3, 0, false, false, None),
            ("user_type_id", 56, 56, 4, 10, 0, false, false, None),
            ("max_length", 52, 52, 2, 5, 0, false, false, None),
            ("precision", 48, 48, 1, 3, 0, false, false, None),
            ("scale", 48, 48, 1, 3, 0, false, false, None),
            (
                "collation_name",
                231,
                256,
                256,
                0,
                0,
                true,
                true,
                Some("catalog"),
            ),
            ("is_nullable", 104, 104, 1, 1, 0, true, true, None),
            ("is_ansi_padded", 104, 104, 1, 1, 0, false, true, None),
            ("is_rowguidcol", 104, 104, 1, 1, 0, false, true, None),
            ("is_identity", 104, 104, 1, 1, 0, false, true, None),
            ("is_filestream", 104, 104, 1, 1, 0, false, true, None),
            ("is_replicated", 104, 104, 1, 1, 0, true, true, None),
            ("is_non_sql_subscribed", 104, 104, 1, 1, 0, true, true, None),
            ("is_merge_published", 104, 104, 1, 1, 0, true, true, None),
            ("is_dts_replicated", 104, 104, 1, 1, 0, true, true, None),
            ("is_xml_document", 104, 104, 1, 1, 0, false, true, None),
            ("xml_collection_id", 56, 56, 4, 10, 0, false, true, None),
            ("default_object_id", 56, 56, 4, 10, 0, false, true, None),
            ("rule_object_id", 56, 56, 4, 10, 0, false, true, None),
            ("seed_value", 98, 98, 8016, 0, 0, true, true, None),
            ("increment_value", 98, 98, 8016, 0, 0, true, true, None),
            ("last_value", 98, 98, 8016, 0, 0, true, true, None),
            (
                "is_not_for_replication",
                104,
                104,
                1,
                1,
                0,
                true,
                true,
                None,
            ),
            ("is_computed", 104, 104, 1, 1, 0, false, true, None),
            ("is_sparse", 104, 104, 1, 1, 0, false, true, None),
            ("is_column_set", 104, 104, 1, 1, 0, false, true, None),
            ("generated_always_type", 48, 48, 1, 3, 0, true, true, None),
            (
                "generated_always_type_desc",
                231,
                231,
                120,
                0,
                0,
                true,
                true,
                Some("Latin1_General_CI_AS_KS_WS"),
            ),
            ("encryption_type", 56, 56, 4, 10, 0, true, true, None),
            (
                "encryption_type_desc",
                231,
                231,
                128,
                0,
                0,
                true,
                true,
                Some("Latin1_General_CI_AS_KS_WS"),
            ),
            (
                "encryption_algorithm_name",
                231,
                231,
                256,
                0,
                0,
                true,
                true,
                Some("Latin1_General_CI_AS_KS_WS"),
            ),
            (
                "column_encryption_key_id",
                56,
                56,
                4,
                10,
                0,
                true,
                true,
                None,
            ),
            (
                "column_encryption_key_database_name",
                231,
                256,
                256,
                0,
                0,
                true,
                true,
                Some("catalog"),
            ),
            ("is_hidden", 104, 104, 1, 1, 0, false, true, None),
            ("is_masked", 104, 104, 1, 1, 0, false, true, None),
            ("graph_type", 56, 56, 4, 10, 0, true, true, None),
            (
                "graph_type_desc",
                231,
                231,
                120,
                0,
                0,
                true,
                false,
                Some("Latin1_General_CI_AS_KS_WS"),
            ),
            (
                "is_data_deletion_filter_column",
                104,
                104,
                1,
                1,
                0,
                true,
                true,
                None,
            ),
            (
                "ledger_view_column_type",
                56,
                56,
                4,
                10,
                0,
                true,
                true,
                None,
            ),
            (
                "ledger_view_column_type_desc",
                231,
                231,
                120,
                0,
                0,
                true,
                true,
                Some("Latin1_General_CI_AS_KS_WS"),
            ),
            (
                "is_dropped_ledger_column",
                104,
                104,
                1,
                1,
                0,
                true,
                true,
                None,
            ),
        ],
        _ => return None,
    };
    Some(
        definitions
            .iter()
            .map(
                |&(name, system, user, length, precision, scale, nullable, computed, collation)| {
                    let collation = collation
                        .map(|name| {
                            if name == "catalog" {
                                catalog_collation
                            } else {
                                name
                            }
                        })
                        .map(str::to_owned);
                    Field {
                        name: name.into(),
                        info: Some(TypeMetadata {
                            system_type_id: Some(system),
                            user_type_id: Some(user),
                            max_length: Some(length),
                            precision: Some(precision),
                            scale: Some(scale),
                            collation_name: collation.clone(),
                        }),
                        collation: collation.map(|name| Ok(Label::Implicit(name))),
                        properties: Properties {
                            nullable: Some(nullable),
                            origin: if computed {
                                Origin::Expression
                            } else {
                                Origin::Stored
                            },
                        },
                        json_fragment: false,
                    }
                },
            )
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations_preserve_independent_catalog_rows_and_wire_properties() {
        let builtin: serde_json::Value =
            serde_json::from_str(include_str!("../column_catalog/system_columns.json")).unwrap();
        let capture: serde_json::Value = serde_json::from_str(include_str!(
            "../../reference/column-catalog-declarations.json"
        ))
        .unwrap();
        assert_eq!(capture["runs"][0], capture["runs"][1]);
        for (view, id, profile) in [
            ("columns", -391, "columns empty shape"),
            ("identity_columns", -396, "identity empty shape"),
        ] {
            let fields = fields(view, "SQL_Latin1_General_CP1_CI_AS").unwrap();
            let rows = builtin["rows"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| r[0].as_i64() == Some(id))
                .collect::<Vec<_>>();
            let record = capture["runs"][0]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["name"] == profile)
                .unwrap();
            let wire = record["preparation"]["sets"][0]["columns"]
                .as_array()
                .unwrap();
            assert_eq!(fields.len(), rows.len());
            assert_eq!(fields.len(), wire.len());
            for (index, field) in fields.iter().enumerate() {
                let row = rows
                    .iter()
                    .find(|r| r[2].as_u64() == Some(index as u64 + 1))
                    .unwrap();
                let info = field.info.as_ref().unwrap();
                assert_eq!(field.name, row[1].as_str().unwrap());
                assert_eq!(info.system_type_id.map(u64::from), row[3].as_u64());
                assert_eq!(info.user_type_id.map(i64::from), row[4].as_i64());
                assert_eq!(info.max_length.map(i64::from), row[5].as_i64());
                assert_eq!(info.precision.map(u64::from), row[6].as_u64());
                assert_eq!(info.scale.map(u64::from), row[7].as_u64());
                assert_eq!(info.collation_name.as_deref(), row[8].as_str());
                assert_eq!(field.properties.nullable, row[9].as_bool());
                let flags = wire[index]["flags"].as_u64().unwrap();
                assert_eq!(
                    field.properties.origin,
                    if flags & 32 != 0 {
                        Origin::Expression
                    } else {
                        Origin::Stored
                    }
                );
                assert_eq!(field.name, wire[index]["name"].as_str().unwrap());
            }
        }
        assert!(fields("unknown_view", "Latin1_General_100_BIN2").is_none());
        let changed = fields("columns", "Latin1_General_100_BIN2").unwrap();
        assert_eq!(
            changed[1].info.as_ref().unwrap().collation_name.as_deref(),
            Some("Latin1_General_100_BIN2")
        );
        assert_eq!(
            changed[26].info.as_ref().unwrap().collation_name.as_deref(),
            Some("Latin1_General_CI_AS_KS_WS")
        );
    }

    #[test]
    fn native_and_snapshot_catalog_shapes_align_for_empty_and_populated_sources() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        for populated in [false, true] {
            if populated {
                let (_,ok)=session.batch_response("CREATE TABLE dbo.catalog_shape(id BIGINT IDENTITY(2147483648,3),v INT); INSERT INTO dbo.catalog_shape(v) VALUES(1)",&Default::default(),false,None);
                assert!(ok);
            }
            for view in ["columns", "identity_columns"] {
                let sql = format!("SELECT * FROM sys.{view} WHERE 1=0");
                let native = session.db.prepare(&sql).unwrap().column_names();
                let statements = msduck_sql::batch::parse(&sql).unwrap();
                let sqlparser::ast::Statement::Query(query) = &statements[0] else {
                    panic!()
                };
                let logical = crate::query_catalog::projection(&session.db, query)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    native,
                    logical.iter().map(|f| f.name.clone()).collect::<Vec<_>>()
                );
                assert!(logical.iter().all(|f| f.info.is_some()));
            }
        }
    }
}
