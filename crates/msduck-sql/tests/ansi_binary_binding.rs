use msduck_core::{catalog::TypeMetadata, collation::Label};
use msduck_sql::{
    binding_scope::{Field, Scope},
    catalog_snapshot::CatalogSnapshot,
    dialect::ServerDialect,
    projection,
};
use sqlparser::{ast::*, parser::Parser};

fn query(sql: &str) -> Box<Query> {
    let Statement::Query(q) = Parser::parse_sql(&ServerDialect, sql).unwrap().remove(0) else {
        panic!("query")
    };
    q
}
fn catalog() -> CatalogSnapshot {
    let mut c = CatalogSnapshot {
        default_collation: Some("SQL_Latin1_General_CP1_CI_AS".into()),
        ..Default::default()
    };
    let info = TypeMetadata {
        system_type_id: Some(167),
        user_type_id: Some(167),
        max_length: Some(8),
        ..Default::default()
    };
    c.types.insert("varchar".into(), info.clone());
    c.tables.insert(
        "dbo.t".into(),
        vec![Field {
            name: "s".into(),
            info: Some(info),
            collation: Some(Ok(Label::Implicit("Latin1_General_100_BIN2".into()))),
            properties: Default::default(),
            json_fragment: false,
        }],
    );
    c
}
#[test]
fn ansi_binary_binding_tracks_declarations_through_nested_scopes() {
    let c = catalog();
    for sql in [
        "SELECT CONVERT(VARBINARY(MAX),s) FROM dbo.t",
        "SELECT CAST(s AS BINARY(3)) FROM dbo.t",
        "WITH q(v) AS (SELECT s FROM dbo.t) SELECT CONVERT(VARBINARY(2),v,0) FROM q",
        "SELECT CONVERT(VARBINARY(MAX),MAX(s)) FROM dbo.t",
        "SELECT (SELECT CAST(t.s AS BINARY)) FROM dbo.t t",
        "SELECT q.b FROM dbo.t t CROSS APPLY (SELECT CAST(t.s AS VARBINARY) b) q",
        "SELECT CAST('a' AS BINARY(3))",
    ] {
        let mut q = query(sql);
        projection::lower_unicode_binary_conversions(&c, &mut q, &Scope::default()).unwrap();
        let rendered = q.to_string();
        assert!(rendered.contains("__msduck_ansi_"), "{sql}: {rendered}");
        projection::lower_unicode_binary_conversions(&c, &mut q, &Scope::default()).unwrap();
        assert_eq!(q.to_string(), rendered);
    }
}
#[test]
fn ansi_binary_does_not_guess_unknown_code_pages_or_hex_styles() {
    let c = catalog();
    for sql in [
        "SELECT CONVERT(VARBINARY(MAX),s,1) FROM dbo.t",
        "SELECT CONVERT(VARBINARY(MAX),s,2) FROM dbo.t",
        "SELECT CAST(s COLLATE Japanese_CI_AS AS BINARY(4)) FROM dbo.t",
        "SELECT (SELECT CAST(t.s AS BINARY(3)) FROM missing t) FROM dbo.t t",
        "SELECT CAST(missing AS BINARY(3))",
    ] {
        let mut q = query(sql);
        let before = q.to_string();
        projection::lower_unicode_binary_conversions(&c, &mut q, &Scope::default()).unwrap();
        assert_eq!(q.to_string(), before, "{sql}");
    }
}
