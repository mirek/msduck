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
        system_type_id: Some(231),
        user_type_id: Some(231),
        max_length: Some(16),
        ..Default::default()
    };
    c.types.insert("nvarchar".into(), info.clone());
    c.tables.insert(
        "dbo.t".into(),
        vec![Field {
            name: "s".into(),
            info: Some(info),
            collation: Some(Ok(Label::Implicit("Latin1_General_100_CI_AS".into()))),
            properties: Default::default(),
            json_fragment: false,
        }],
    );
    c
}
#[test]
fn casing_annotations_follow_scope_and_preserve_original_operands() {
    let c = catalog();
    for sql in [
        "SELECT LOWER(t.s) FROM dbo.t t",
        "SELECT LOWER(UPPER(t.s)) FROM dbo.t t",
        "WITH c(v) AS (SELECT s FROM dbo.t) SELECT UPPER(v) FROM c",
        "SELECT (SELECT LOWER(t.s)) FROM dbo.t t",
        "SELECT a.v FROM dbo.t t CROSS APPLY (SELECT UPPER(t.s) AS v) a",
        "SELECT t.s FROM dbo.t t JOIN dbo.t u ON LOWER(t.s)=UPPER(u.s)",
    ] {
        let mut q = query(sql);
        let original = projection::query_fields(&c, &q, &Scope::default()).unwrap();
        projection::annotate_unicode_case_inputs(&c, &mut q, &Scope::default()).unwrap();
        let rendered = q.to_string();
        assert!(rendered.contains("__msduck_case_input_100"), "{rendered}");
        assert!(rendered.contains("NVARCHAR(8)"), "{rendered}");
        projection::annotate_unicode_case_inputs(&c, &mut q, &Scope::default()).unwrap();
        assert_eq!(rendered, q.to_string(), "annotation must be idempotent");
        let annotated = projection::query_fields(&c, &q, &Scope::default()).unwrap();
        assert_eq!(original.len(), annotated.len());
        for (a, b) in original.iter().zip(&annotated) {
            assert_eq!(a.info, b.info, "{sql}");
        }
    }
}
#[test]
fn casing_uses_parameter_declarations_and_leaves_unknown_inputs_unbound() {
    let c = catalog();
    let mut scope = Scope::default();
    scope
        .parameters
        .insert("@p".into(), c.types["nvarchar"].clone());
    let mut q = query("SELECT LOWER(@p)");
    projection::annotate_unicode_case_inputs(&c, &mut q, &scope).unwrap();
    assert!(q.to_string().contains("__msduck_case_input_legacy(@p)"));
    for sql in [
        "SELECT LOWER(missing)",
        "SELECT (SELECT LOWER(x.s) FROM missing_table x) FROM dbo.t x",
        "SELECT 1 FROM dbo.t a JOIN dbo.t b ON LOWER(c.s)=N'a' JOIN dbo.t c ON a.s=c.s",
        "SELECT LOWER(s) FROM missing_table",
        "SELECT LOWER(N'a' COLLATE unknown_collation)",
    ] {
        let mut q = query(sql);
        let before = q.to_string();
        projection::annotate_unicode_case_inputs(&c, &mut q, &Scope::default()).unwrap();
        assert_eq!(q.to_string(), before);
    }
}
