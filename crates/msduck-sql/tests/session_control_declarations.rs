use msduck_core::{catalog::TypeMetadata, result::Origin};
use msduck_sql::{
    binding_scope::{Field, Scope},
    catalog_snapshot::CatalogSnapshot,
    projection,
};
use serde_json::Value;
use sqlparser::{ast::Statement, parser::Parser};

fn catalog() -> CatalogSnapshot {
    let mut catalog = CatalogSnapshot::default();
    for (name, id, width) in [
        ("int", 56, 4),
        ("smallint", 52, 2),
        ("sql_variant", 98, 8016),
    ] {
        catalog.types.insert(
            name.into(),
            TypeMetadata {
                system_type_id: Some(id),
                user_type_id: Some(i32::from(id)),
                max_length: Some(width),
                ..Default::default()
            },
        );
    }
    catalog
}

#[test]
fn complete_control_declarations_match_both_fresh_reference_runs() {
    let session: Value = serde_json::from_str(include_str!(
        "../../../reference/session-property-context.json"
    ))
    .unwrap();
    let guid: Value =
        serde_json::from_str(include_str!("../../../reference/guid-assignment.json")).unwrap();
    for fixture in [&session, &guid] {
        assert_eq!(fixture["runs"][0], fixture["runs"][1]);
        for run in fixture["runs"].as_array().unwrap() {
            for record in run.as_array().unwrap().iter().filter(|r| {
                r["sql"]
                    .as_str()
                    .is_some_and(|sql| sql.contains("@@OPTIONS"))
                    && r["result"]["sets"]
                        .as_array()
                        .is_some_and(|sets| sets.len() == 1)
            }) {
                let sql = record["sql"].as_str().unwrap();
                let Statement::Query(query) =
                    Parser::parse_sql(&msduck_sql::dialect::ServerDialect, sql)
                        .unwrap()
                        .into_iter()
                        .filter(|statement| matches!(statement, Statement::Query(_)))
                        .last()
                        .unwrap()
                else {
                    panic!("not a query: {sql}")
                };
                let fields =
                    projection::query_fields(&catalog(), &query, &Scope::default()).unwrap();
                let columns = record["result"]["sets"][0]["columns"].as_array().unwrap();
                assert_eq!(fields.len(), columns.len());
                for (field, column) in fields.iter().zip(columns) {
                    assert_eq!(field.name, column["name"].as_str().unwrap());
                    let kind = field.info.as_ref().unwrap_or_else(|| {
                        panic!("missing declaration for {} in {sql}", field.name)
                    });
                    let expected =
                        match (column["type"].as_str().unwrap(), column["length"].as_u64()) {
                            ("Int", _) | ("IntN", Some(4)) => 56,
                            ("IntN", Some(2)) => 52,
                            ("Variant", _) => 98,
                            other => panic!("unexpected captured type: {other:?}"),
                        };
                    assert_eq!(kind.system_type_id, Some(expected), "{}: {sql}", field.name);
                    let flags = u16::from(field.properties.nullable != Some(false))
                        | if field.properties.origin == Origin::Expression {
                            32
                        } else {
                            0
                        };
                    assert_eq!(
                        u64::from(flags),
                        column["flags"].as_u64().unwrap(),
                        "{}: {sql}",
                        field.name
                    );
                }
            }
        }
    }
}

fn fields(sql: &str, catalog: &CatalogSnapshot, scope: &Scope) -> Vec<Field> {
    let Statement::Query(query) = msduck_sql::batch::parse(sql).unwrap().remove(0) else {
        panic!("not a query: {sql}")
    };
    projection::query_fields(catalog, &query, scope).unwrap()
}

#[test]
fn declarations_survive_case_parentheses_empty_and_derived_sources() {
    let catalog = catalog();
    for sql in [
        "SELECT ((@@oPtIoNs)) AS o,SESSIONPROPERTY('ANSI_WARNINGS') AS s WHERE 1=0",
        "WITH c(o,s) AS (SELECT @@OPTIONS,SESSIONPROPERTY('ANSI_WARNINGS')) SELECT o,s FROM c WHERE 1=0",
        "SELECT o,s FROM (SELECT @@OPTIONS AS o,SESSIONPROPERTY('ANSI_WARNINGS') AS s) d WHERE 1=0",
    ] {
        let fields = fields(sql, &catalog, &Scope::default());
        assert_eq!(
            fields
                .iter()
                .map(|field| field.info.as_ref().and_then(|info| info.system_type_id))
                .collect::<Vec<_>>(),
            vec![Some(56), Some(98)],
            "{sql}"
        );
        assert_eq!(
            fields
                .iter()
                .map(|field| field.properties.nullable)
                .collect::<Vec<_>>(),
            vec![Some(false), Some(true)],
            "{sql}"
        );
    }
}

#[test]
fn option_mask_proof_does_not_use_nullable_parameters_or_quoted_columns() {
    let mut catalog = catalog();
    let mut scope = Scope::default();
    scope
        .parameters
        .insert("@mask".into(), catalog.types["int"].clone());
    for sql in [
        "SELECT @@OPTIONS & 16384 AS m WHERE 1=0",
        "SELECT (16384) & (@@OPTIONS) AS m",
        "SELECT (@@OPTIONS & 16384) & 64 AS m",
    ] {
        let fields = fields(sql, &catalog, &scope);
        assert_eq!(
            fields[0].info.as_ref().and_then(|info| info.system_type_id),
            Some(56),
            "{sql}"
        );
        assert_eq!(
            fields[0].properties,
            msduck_core::result::Properties::expression(false),
            "{sql}"
        );
    }
    for sql in [
        "SELECT @@OPTIONS & @mask AS m",
        "SELECT @@OPTIONS & NULL AS m",
        "SELECT @@OPTIONS & 2147483648 AS m",
    ] {
        let fields = fields(sql, &catalog, &scope);
        assert_eq!(fields[0].properties.nullable, Some(true), "{sql}");
        assert!(
            fields[0].info.is_none(),
            "unsupported bitwise operands must remain unknown: {sql}"
        );
    }
    let declared = Field {
        name: "@@OPTIONS".into(),
        info: Some(TypeMetadata {
            system_type_id: Some(231),
            ..Default::default()
        }),
        collation: None,
        json_fragment: false,
        properties: msduck_core::result::Properties {
            nullable: Some(true),
            origin: Origin::Stored,
        },
    };
    catalog
        .tables
        .insert("dbo.t".into(), vec![declared.clone()]);
    let fields = fields("SELECT [@@OPTIONS] FROM dbo.t WHERE 1=0", &catalog, &scope);
    assert_eq!(fields[0].name, declared.name);
    assert_eq!(fields[0].info, declared.info);
    assert_eq!(fields[0].properties, declared.properties);
}
