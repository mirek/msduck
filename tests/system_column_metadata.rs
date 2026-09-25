#[path = "../src/system_column_metadata.rs"]
mod system_column_metadata;

use msduck_core::{catalog::TypeMetadata, collation::Label, result::Origin};
use serde_json::{Value, json};
use system_column_metadata::{CollationRole, captured, fields};

const REFERENCE: &str = include_str!("../reference/system-all-columns.json");
const DATABASE_COLLATION: &str = "SQL_Latin1_General_CP1_CI_AS";
const RESOURCE_COLLATION: &str = "Latin1_General_CI_AS_KS_WS";

fn observation<'a>(run: &'a Value, name: &str) -> &'a Value {
    run.as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("missing {name}"))
}

fn collation(role: CollationRole) -> Value {
    match role {
        CollationRole::Database => json!({
            "buffer": {"kind": "missing"}, "lcid": 1033, "flags": 13,
            "version": 0, "sortId": 52, "codepage": "CP1252"
        }),
        CollationRole::Resource => json!({
            "buffer": {"kind": "missing"}, "lcid": 1033, "flags": 1,
            "version": 0, "sortId": 0, "codepage": "CP1252"
        }),
    }
}

#[test]
fn both_reference_runs_match_every_captured_field_and_wire_descriptor() {
    let reference: Value = serde_json::from_str(REFERENCE).unwrap();
    assert_eq!(reference["runs"].as_array().unwrap().len(), 2);
    for run in reference["runs"].as_array().unwrap() {
        for view in ["columns", "system_columns", "all_columns"] {
            let actual = captured(view, DATABASE_COLLATION).unwrap();
            let descriptor_name = format!("{view} descriptor");
            let descriptors = observation(run, &descriptor_name)["result"]["sets"][0]["columns"]
                .as_array()
                .unwrap();
            let declaration_name = format!("{view} declarations");
            let declarations = &observation(run, &declaration_name)["result"]["sets"][0];
            let names = declarations["columns"].as_array().unwrap();
            let rows = declarations["rows"].as_array().unwrap();
            assert_eq!(actual.len(), 43, "{view}");
            assert_eq!(descriptors.len(), actual.len(), "{view}");
            assert_eq!(rows.len(), actual.len(), "{view}");
            let at = |name: &str| {
                names
                    .iter()
                    .position(|column| column["name"].as_str() == Some(name))
                    .unwrap()
            };
            for (index, column) in actual.iter().enumerate() {
                let expected = &descriptors[index];
                let declared = &rows[index];
                let name = expected["name"].as_str().unwrap();
                assert_eq!(column.wire.name, name, "{view}[{index}] name");
                assert_eq!(column.field.name, name, "{view}[{index}] field name");
                assert_eq!(
                    declared[at("name")],
                    name,
                    "{view}[{index}] declaration name"
                );
                assert_eq!(
                    column.wire.type_name, expected["type"],
                    "{view}[{index}] type"
                );
                assert_eq!(
                    column.wire.length.map(u64::from),
                    expected["length"].as_u64(),
                    "{view}[{index}] length"
                );
                assert_eq!(
                    column.wire.precision,
                    expected["precision"].as_u64().map(|x| x as u8)
                );
                assert_eq!(
                    column.wire.scale,
                    expected["scale"].as_u64().map(|x| x as u8)
                );
                assert_eq!(
                    u64::from(column.wire.flags),
                    expected["flags"].as_u64().unwrap(),
                    "{view}[{index}] flags"
                );
                let expected_collation =
                    column.wire.collation.map(collation).unwrap_or(Value::Null);
                assert_eq!(
                    expected["collation"], expected_collation,
                    "{view}[{index}] collation"
                );

                let declared_collation = declared[at("collation_name")].as_str();
                let info = column.field.info.as_ref().unwrap();
                assert_eq!(
                    info,
                    &TypeMetadata {
                        system_type_id: Some(declared[at("system_type_id")].as_u64().unwrap() as u8),
                        user_type_id: Some(declared[at("user_type_id")].as_i64().unwrap() as i32),
                        max_length: Some(declared[at("max_length")].as_i64().unwrap() as i16),
                        precision: Some(declared[at("precision")].as_u64().unwrap() as u8),
                        scale: Some(declared[at("scale")].as_u64().unwrap() as u8),
                        collation_name: declared_collation.map(str::to_owned),
                    },
                    "{view}[{index}] declaration"
                );
                assert_eq!(
                    column.field.properties.nullable,
                    declared[at("is_nullable")].as_bool(),
                    "{view}[{index}] nullable"
                );
                assert_eq!(
                    column.field.properties.origin,
                    if column.wire.flags & 32 != 0 {
                        Origin::Expression
                    } else {
                        Origin::Stored
                    },
                    "{view}[{index}] origin"
                );
                assert_eq!(
                    column.field.collation,
                    declared_collation.map(|name| Ok(Label::Implicit(name.to_owned()))),
                    "{view}[{index}] label"
                );
                assert!(!column.field.json_fragment);
            }
        }
    }
}

#[test]
fn view_origins_and_database_collation_remain_explicit_inputs() {
    assert!(fields("missing", DATABASE_COLLATION).is_none());
    let database = fields("COLUMNS", "Latin1_General_100_CI_AS").unwrap();
    let resource = database
        .iter()
        .find(|field| field.name == "graph_type_desc")
        .unwrap();
    let name = database.iter().find(|field| field.name == "name").unwrap();
    assert_eq!(
        name.info.as_ref().unwrap().collation_name.as_deref(),
        Some("Latin1_General_100_CI_AS")
    );
    assert_eq!(
        resource.info.as_ref().unwrap().collation_name.as_deref(),
        Some(RESOURCE_COLLATION)
    );
    assert_eq!(database.len(), 43);
    assert_eq!(
        captured("columns", DATABASE_COLLATION).unwrap()[8]
            .field
            .properties
            .origin,
        Origin::Expression
    );
    assert_eq!(
        captured("all_columns", DATABASE_COLLATION).unwrap()[8]
            .field
            .properties
            .origin,
        Origin::Stored
    );
    assert_eq!(
        captured("system_columns", DATABASE_COLLATION).unwrap()[20]
            .field
            .properties
            .origin,
        Origin::Expression
    );
}
