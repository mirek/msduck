//! Captured logical fields and TDS shapes for the three `sys.*columns` views.
//!
//! The caller supplies the database collation. Resource-string collation and
//! per-view result origin come from the pinned SQL Server reference. No backend
//! query or result value is needed to construct these fields.
use msduck_core::{
    catalog::TypeMetadata,
    collation::Label,
    result::{Origin, Properties},
};
use msduck_sql::binding_scope::Field;

const RESOURCE_COLLATION: &str = "Latin1_General_CI_AS_KS_WS";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CollationRole {
    Database,
    Resource,
}

#[derive(Clone, Copy)]
struct Definition {
    name: &'static str,
    system_type_id: u8,
    user_type_id: i32,
    max_length: i16,
    precision: u8,
    scale: u8,
    collation: Option<CollationRole>,
}

const fn definition(
    name: &'static str,
    system_type_id: u8,
    user_type_id: i32,
    max_length: i16,
    precision: u8,
    scale: u8,
    collation: Option<CollationRole>,
) -> Definition {
    Definition {
        name,
        system_type_id,
        user_type_id,
        max_length,
        precision,
        scale,
        collation,
    }
}

// SQL Server's sys.columns and sys.all_columns declarations agree. The one
// system_columns user-type exception is applied below; nullability and result
// origin are taken from the per-view TDS flags, not from stored row values.
const DEFINITIONS: [Definition; 43] = [
    definition("object_id", 56, 56, 4, 10, 0, None),
    definition("name", 231, 256, 256, 0, 0, Some(CollationRole::Database)),
    definition("column_id", 56, 56, 4, 10, 0, None),
    definition("system_type_id", 48, 48, 1, 3, 0, None),
    definition("user_type_id", 56, 56, 4, 10, 0, None),
    definition("max_length", 52, 52, 2, 5, 0, None),
    definition("precision", 48, 48, 1, 3, 0, None),
    definition("scale", 48, 48, 1, 3, 0, None),
    definition(
        "collation_name",
        231,
        256,
        256,
        0,
        0,
        Some(CollationRole::Database),
    ),
    definition("is_nullable", 104, 104, 1, 1, 0, None),
    definition("is_ansi_padded", 104, 104, 1, 1, 0, None),
    definition("is_rowguidcol", 104, 104, 1, 1, 0, None),
    definition("is_identity", 104, 104, 1, 1, 0, None),
    definition("is_computed", 104, 104, 1, 1, 0, None),
    definition("is_filestream", 104, 104, 1, 1, 0, None),
    definition("is_replicated", 104, 104, 1, 1, 0, None),
    definition("is_non_sql_subscribed", 104, 104, 1, 1, 0, None),
    definition("is_merge_published", 104, 104, 1, 1, 0, None),
    definition("is_dts_replicated", 104, 104, 1, 1, 0, None),
    definition("is_xml_document", 104, 104, 1, 1, 0, None),
    definition("xml_collection_id", 56, 56, 4, 10, 0, None),
    definition("default_object_id", 56, 56, 4, 10, 0, None),
    definition("rule_object_id", 56, 56, 4, 10, 0, None),
    definition("is_sparse", 104, 104, 1, 1, 0, None),
    definition("is_column_set", 104, 104, 1, 1, 0, None),
    definition("generated_always_type", 48, 48, 1, 3, 0, None),
    definition(
        "generated_always_type_desc",
        231,
        231,
        120,
        0,
        0,
        Some(CollationRole::Resource),
    ),
    definition("encryption_type", 56, 56, 4, 10, 0, None),
    definition(
        "encryption_type_desc",
        231,
        231,
        128,
        0,
        0,
        Some(CollationRole::Resource),
    ),
    definition(
        "encryption_algorithm_name",
        231,
        256,
        256,
        0,
        0,
        Some(CollationRole::Resource),
    ),
    definition("column_encryption_key_id", 56, 56, 4, 10, 0, None),
    definition(
        "column_encryption_key_database_name",
        231,
        256,
        256,
        0,
        0,
        Some(CollationRole::Database),
    ),
    definition("is_hidden", 104, 104, 1, 1, 0, None),
    definition("is_masked", 104, 104, 1, 1, 0, None),
    definition("graph_type", 56, 56, 4, 10, 0, None),
    definition(
        "graph_type_desc",
        231,
        231,
        120,
        0,
        0,
        Some(CollationRole::Resource),
    ),
    definition("is_data_deletion_filter_column", 104, 104, 1, 1, 0, None),
    definition("ledger_view_column_type", 56, 56, 4, 10, 0, None),
    definition(
        "ledger_view_column_type_desc",
        231,
        231,
        120,
        0,
        0,
        Some(CollationRole::Resource),
    ),
    definition("is_dropped_ledger_column", 104, 104, 1, 1, 0, None),
    definition("vector_dimensions", 56, 56, 4, 10, 0, None),
    definition("vector_base_type", 48, 48, 1, 3, 0, None),
    definition(
        "vector_base_type_desc",
        231,
        231,
        20,
        0,
        0,
        Some(CollationRole::Resource),
    ),
];

const COLUMNS_FLAGS: [u8; 43] = [
    8, 9, 8, 8, 8, 8, 8, 8, 33, 33, 32, 32, 32, 33, 32, 33, 33, 33, 33, 32, 8, 8, 8, 33, 33, 33,
    33, 33, 33, 33, 9, 33, 33, 32, 33, 9, 33, 33, 9, 33, 33, 33, 33,
];
const SYSTEM_COLUMNS_FLAGS: [u8; 43] = [
    8, 33, 8, 8, 8, 8, 8, 8, 33, 33, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32,
    33, 33, 33, 33, 33, 33, 33, 32, 32, 33, 33, 32, 33, 33, 32, 33, 33, 33,
];
const ALL_COLUMNS_FLAGS: [u8; 43] = [
    8, 9, 8, 8, 8, 8, 8, 8, 9, 9, 8, 8, 8, 9, 8, 9, 9, 9, 9, 8, 8, 8, 8, 9, 9, 9, 9, 9, 9, 9, 9, 9,
    9, 8, 9, 9, 9, 9, 9, 9, 9, 9, 9,
];

/// The captured descriptor facts that later wire integration must preserve.
/// A collation role is used because the database collation is an explicit input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WireDescriptor {
    pub name: &'static str,
    pub type_name: &'static str,
    pub length: Option<u16>,
    pub precision: Option<u8>,
    pub scale: Option<u8>,
    pub flags: u8,
    pub collation: Option<CollationRole>,
}

pub(crate) struct CapturedField {
    pub field: Field,
    pub wire: WireDescriptor,
}

pub(crate) fn captured(view: &str, catalog_collation: &str) -> Option<Vec<CapturedField>> {
    let (view, flags): (&str, &[u8; 43]) = match view.to_ascii_lowercase().as_str() {
        "columns" => ("columns", &COLUMNS_FLAGS),
        "system_columns" => ("system_columns", &SYSTEM_COLUMNS_FLAGS),
        "all_columns" => ("all_columns", &ALL_COLUMNS_FLAGS),
        _ => return None,
    };
    Some(
        DEFINITIONS
            .iter()
            .zip(flags)
            .map(|(definition, &flags)| {
                let nullable = flags & 1 != 0;
                let origin = if flags & 32 != 0 {
                    Origin::Expression
                } else {
                    Origin::Stored
                };
                let user_type_id =
                    if view == "system_columns" && definition.name == "encryption_algorithm_name" {
                        231
                    } else {
                        definition.user_type_id
                    };
                let collation_name = definition.collation.map(|role| match role {
                    CollationRole::Database => catalog_collation.to_owned(),
                    CollationRole::Resource => RESOURCE_COLLATION.to_owned(),
                });
                let (type_name, length) = match (definition.system_type_id, nullable) {
                    (56, false) => ("Int", None),
                    (56, true) => ("IntN", Some(4)),
                    (48, false) => ("TinyInt", None),
                    (48, true) => ("IntN", Some(1)),
                    (52, false) => ("SmallInt", None),
                    (52, true) => ("IntN", Some(2)),
                    (104, false) => ("Bit", None),
                    (104, true) => ("BitN", Some(1)),
                    (231, _) => (
                        "NVarChar",
                        Some(u16::try_from(definition.max_length).expect("bounded sysname")),
                    ),
                    _ => unreachable!("captured system-column type"),
                };
                CapturedField {
                    field: Field {
                        name: definition.name.to_owned(),
                        info: Some(TypeMetadata {
                            system_type_id: Some(definition.system_type_id),
                            user_type_id: Some(user_type_id),
                            max_length: Some(definition.max_length),
                            precision: Some(definition.precision),
                            scale: Some(definition.scale),
                            collation_name: collation_name.clone(),
                        }),
                        collation: collation_name.map(|name| Ok(Label::Implicit(name))),
                        properties: Properties {
                            nullable: Some(nullable),
                            origin,
                        },
                        json_fragment: false,
                    },
                    wire: WireDescriptor {
                        name: definition.name,
                        type_name,
                        length,
                        precision: None,
                        scale: None,
                        flags,
                        collation: definition.collation,
                    },
                }
            })
            .collect(),
    )
}

/// Ready for a later `query_catalog::system_catalog_fields` integration once
/// that file's active claim is released.
pub(crate) fn fields(view: &str, catalog_collation: &str) -> Option<Vec<Field>> {
    captured(view, catalog_collation)
        .map(|columns| columns.into_iter().map(|column| column.field).collect())
}
