use msduck_core::{catalog::TypeMetadata, collation::Label};
use msduck_sql::{
    binding_scope::{Field, Scope},
    catalog_snapshot::CatalogSnapshot,
    dialect::ServerDialect,
    projection::{self, character_extrema},
};
use sqlparser::{ast::*, parser::Parser};
use std::ops::ControlFlow;

fn query(sql: &str) -> Box<Query> {
    let Statement::Query(query) = Parser::parse_sql(&ServerDialect, sql).unwrap().remove(0) else {
        panic!("query");
    };
    query
}
fn catalog(id: u8, width: i16) -> CatalogSnapshot {
    let mut catalog = CatalogSnapshot {
        default_collation: Some("SQL_Latin1_General_CP1_CI_AS".into()),
        ..Default::default()
    };
    for (name, kind) in [
        ("nvarchar", 231),
        ("nchar", 239),
        ("varchar", 167),
        ("char", 175),
    ] {
        catalog.types.insert(
            name.into(),
            TypeMetadata {
                system_type_id: Some(kind),
                user_type_id: Some(i32::from(kind)),
                ..Default::default()
            },
        );
    }
    catalog.tables.insert(
        "dbo.t".into(),
        vec![Field {
            name: "s".into(),
            info: Some(TypeMetadata {
                system_type_id: Some(id),
                user_type_id: Some(i32::from(id)),
                max_length: Some(width),
                ..Default::default()
            }),
            collation: Some(Ok(Label::Implicit("Latin1_General_100_BIN2".into()))),
            properties: Default::default(),
            json_fragment: false,
        }],
    );
    catalog
}

#[test]
fn scoped_annotations_keep_declarations_and_public_aggregate_structure() {
    for (id, width, declaration, marker) in [
        (231, 16, "NVARCHAR(8)", "unicode"),
        (239, 16, "nchar(8)", "unicode"),
        (167, 8, "VARCHAR(8)", "ansi"),
        (175, 8, "CHAR(8)", "ansi"),
        (231, -1, "NVARCHAR(MAX)", "unicode"),
        (167, -1, "VARCHAR(MAX)", "ansi"),
    ] {
        let catalog = catalog(id, width);
        for sql in [
            "SELECT MIN(s),MAX(s) FROM dbo.t",
            "SELECT MIN(DISTINCT s) FROM dbo.t",
            "SELECT MAX(s) OVER(ORDER BY s ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM dbo.t",
            "WITH q(v) AS (SELECT s FROM dbo.t) SELECT MAX(v) FROM q",
            "SELECT MIN(q.s) FROM (SELECT s FROM dbo.t) q",
            "SELECT (SELECT MAX(t.s)) FROM dbo.t t",
            "SELECT a.v FROM dbo.t t CROSS APPLY (SELECT MAX(t.s) AS v) a",
        ] {
            let mut q = query(sql);
            let before = projection::query_fields(&catalog, &q, &Scope::default()).unwrap();
            character_extrema::annotate(&catalog, &mut q, &Scope::default()).unwrap();
            let rendered = q.to_string();
            assert!(
                rendered.contains(&format!("__msduck_extrema_input_{marker}")),
                "{sql}: {rendered}"
            );
            assert!(rendered.contains(declaration), "{rendered}");
            let after = projection::query_fields(&catalog, &q, &Scope::default()).unwrap();
            assert_eq!(
                before.iter().map(|f| &f.collation).collect::<Vec<_>>(),
                after.iter().map(|f| &f.collation).collect::<Vec<_>>(),
                "collation precedence: {sql}"
            );
            assert_eq!(
                before.iter().map(|f| &f.info).collect::<Vec<_>>(),
                after.iter().map(|f| &f.info).collect::<Vec<_>>(),
                "{sql}"
            );
            character_extrema::annotate(&catalog, &mut q, &Scope::default()).unwrap();
            assert_eq!(rendered, q.to_string(), "idempotent: {sql}");
        }
    }
}

#[test]
fn unknown_linguistic_shadowed_and_invalid_inputs_are_not_bound() {
    let c = catalog(231, 16);
    for sql in [
        "SELECT MAX(missing) FROM dbo.t",
        "SELECT MIN(s COLLATE SQL_Latin1_General_CP1_CI_AS) FROM dbo.t",
        "SELECT MIN(s) FROM missing",
        "SELECT (SELECT MAX(t.s) FROM missing t) FROM dbo.t t",
        "SELECT MIN(DISTINCT s) OVER() FROM dbo.t",
        "SELECT MAX(s,s) FROM dbo.t",
        "SELECT MAX(42)",
        "SELECT MAX(NULL)",
    ] {
        let mut q = query(sql);
        let original = q.to_string();
        character_extrema::annotate(&c, &mut q, &Scope::default()).unwrap();
        assert_eq!(original, q.to_string(), "{sql}");
    }
    for (id, width) in [(56, 4), (231, 3), (239, -1), (175, -1), (231, 0)] {
        let mut q = query("SELECT MIN(s) FROM dbo.t");
        let original = q.to_string();
        character_extrema::annotate(&catalog(id, width), &mut q, &Scope::default()).unwrap();
        assert_eq!(original, q.to_string());
    }
}

#[test]
fn late_lowering_preserves_window_and_distinct_and_evaluates_one_operand() {
    struct Lower;
    impl VisitorMut for Lower {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            if character_extrema::bound_result(expr) {
                if let Expr::Function(f) = expr {
                    msduck_sql::aggregate::validate(f).unwrap();
                }
                assert!(character_extrema::lower(expr));
            }
            ControlFlow::Continue(())
        }
    }
    for (sql, suffix) in [
        (
            "SELECT MIN(DISTINCT s) FROM dbo.t",
            "__msduck_min_bin2_unicode(s)",
        ),
        (
            "SELECT MAX(s) OVER(ORDER BY s ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM dbo.t",
            "OVER (ORDER BY s ROWS BETWEEN 1 PRECEDING AND CURRENT ROW)",
        ),
    ] {
        let mut q = query(sql);
        character_extrema::annotate(&catalog(231, 16), &mut q, &Scope::default()).unwrap();
        let _ = VisitMut::visit(&mut q, &mut Lower);
        let rendered = q.to_string();
        assert!(rendered.contains("_bin2_unicode("), "{rendered}");
        assert!(rendered.contains(suffix), "{rendered}");
        assert!(!rendered.contains("extrema_input"));
    }
}
