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
        system_type_id: Some(165),
        user_type_id: Some(165),
        max_length: Some(8),
        ..Default::default()
    };
    c.types.insert("varbinary".into(), info.clone());
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
fn binary_unicode_binding_uses_scoped_binary_declarations_and_cast_widths() {
    let c = catalog();
    for (sql, adapter, width) in [
        (
            "SELECT CONVERT(NVARCHAR(MAX),s) FROM dbo.t",
            "__msduck_binary_nvarchar",
            "-1",
        ),
        (
            "SELECT CAST(s AS NCHAR) FROM dbo.t",
            "__msduck_binary_nchar",
            "30",
        ),
        (
            "WITH q(v) AS (SELECT s FROM dbo.t) SELECT TRY_CONVERT(NVARCHAR(5),v,1) FROM q",
            "__msduck_try_binary_nvarchar",
            "5",
        ),
        (
            "SELECT (SELECT CAST(t.s AS NVARCHAR(3))) FROM dbo.t t",
            "__msduck_binary_nvarchar",
            "3",
        ),
        (
            "SELECT q.v FROM dbo.t t CROSS APPLY (SELECT TRY_CAST(t.s AS NCHAR(4)) v) q",
            "__msduck_try_binary_nchar",
            "4",
        ),
        (
            "SELECT CONVERT(NVARCHAR(MAX),0x410042)",
            "__msduck_binary_nvarchar",
            "-1",
        ),
    ] {
        let mut q = query(sql);
        projection::lower_unicode_binary_conversions(&c, &mut q, &Scope::default()).unwrap();
        let rendered = q.to_string();
        assert!(
            rendered.contains(adapter) && rendered.contains(&format!(", {width},")),
            "{sql}: {rendered}"
        );
        projection::lower_unicode_binary_conversions(&c, &mut q, &Scope::default()).unwrap();
        assert_eq!(q.to_string(), rendered);
    }
}
#[test]
fn binary_unicode_binding_keeps_unknown_and_character_sources_unresolved() {
    let c = catalog();
    for sql in [
        "SELECT CONVERT(NVARCHAR(MAX),unknown)",
        "SELECT CONVERT(NVARCHAR(MAX),'abc')",
        "SELECT (SELECT CONVERT(NVARCHAR(MAX),t.s) FROM missing t) FROM dbo.t t",
    ] {
        let mut q = query(sql);
        let before = q.to_string();
        projection::lower_unicode_binary_conversions(&c, &mut q, &Scope::default()).unwrap();
        assert_eq!(q.to_string(), before);
    }
}
