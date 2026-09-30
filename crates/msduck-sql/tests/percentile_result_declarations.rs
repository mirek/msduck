use msduck_core::catalog::TypeMetadata;
use msduck_sql::{
    binding_scope::{Field, Scope},
    catalog_snapshot::CatalogSnapshot,
    projection,
};
use sqlparser::{ast::Statement, parser::Parser};

fn catalog(source: Option<TypeMetadata>) -> CatalogSnapshot {
    let mut catalog = CatalogSnapshot::default();
    catalog.types.insert(
        "float".into(),
        TypeMetadata {
            system_type_id: Some(62),
            user_type_id: Some(62),
            precision: Some(53),
            scale: Some(0),
            max_length: Some(8),
            ..Default::default()
        },
    );
    catalog.tables.insert(
        "dbo.input".into(),
        vec![Field {
            name: "n".into(),
            info: source,
            collation: None,
            json_fragment: false,
            properties: Default::default(),
        }],
    );
    catalog
}
fn metadata(catalog: &CatalogSnapshot, kind: &str, fraction: &str) -> Option<TypeMetadata> {
    let sql = format!(
        "SELECT PERCENTILE_{kind}({fraction}) WITHIN GROUP(ORDER BY n) OVER() AS p FROM dbo.input"
    );
    let Statement::Query(query) = Parser::parse_sql(&msduck_sql::dialect::ServerDialect, &sql)
        .unwrap()
        .remove(0)
    else {
        panic!("query")
    };
    let mut scope = Scope::default();
    scope.parameters.insert("@p".into(), Default::default());
    let fields = projection::query_fields(catalog, &query, &scope).unwrap();
    assert_eq!(fields[0].name, "p");
    fields[0].info.clone()
}
#[test]
fn fraction_values_do_not_change_continuous_or_discrete_declarations() {
    for source in [
        TypeMetadata {
            system_type_id: Some(56),
            max_length: Some(4),
            ..Default::default()
        },
        TypeMetadata {
            system_type_id: Some(106),
            user_type_id: Some(108),
            precision: Some(28),
            scale: Some(9),
            max_length: Some(13),
            ..Default::default()
        },
    ] {
        let catalog = catalog(Some(source.clone()));
        for fraction in [
            ".5",
            "NULL",
            "'abc'",
            "1.1",
            "@p",
            "CASE WHEN 1=0 THEN .5 ELSE 1/0 END",
        ] {
            assert_eq!(metadata(&catalog, "DISC", fraction), Some(source.clone()));
            assert_eq!(
                metadata(&catalog, "CONT", fraction),
                catalog.types.get("float").cloned()
            );
        }
    }
}
#[test]
fn discrete_retains_character_source_width_and_unknowns_remain_barriers() {
    let source = TypeMetadata {
        system_type_id: Some(231),
        max_length: Some(32),
        collation_name: Some("example".into()),
        ..Default::default()
    };
    let known = catalog(Some(source.clone()));
    assert_eq!(metadata(&known, "DISC", "@p"), Some(source));
    assert!(metadata(&known, "CONT", "@p").is_none());
    let unknown = catalog(None);
    for kind in ["CONT", "DISC"] {
        assert!(metadata(&unknown, kind, "@p").is_none())
    }
}
