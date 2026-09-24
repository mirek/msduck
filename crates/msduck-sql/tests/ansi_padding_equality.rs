use msduck_core::{catalog::TypeMetadata, collation::Label};
use msduck_sql::{
    binding_scope::Field, catalog_snapshot::CatalogSnapshot, dialect::ServerDialect, projection,
};
use sqlparser::{ast::Statement, parser::Parser};

fn catalog() -> CatalogSnapshot {
    let name = "SQL_Latin1_General_CP1_CI_AS";
    let mut catalog = CatalogSnapshot {
        default_collation: Some(name.into()),
        ..Default::default()
    };
    for (name, id) in [("varchar", 167), ("nvarchar", 231), ("char", 175)] {
        catalog.types.insert(
            name.into(),
            TypeMetadata {
                system_type_id: Some(id),
                user_type_id: Some(id as i32),
                max_length: Some(8000),
                ..Default::default()
            },
        );
    }
    catalog.tables.insert(
        "dbo.t".into(),
        [("v", 175), ("n", 231), ("i", 56)]
            .into_iter()
            .map(|(column, id)| Field {
                name: column.into(),
                info: Some(TypeMetadata {
                    system_type_id: Some(id),
                    user_type_id: Some(id as i32),
                    max_length: Some(2),
                    collation_name: Some(name.into()),
                    ..Default::default()
                }),
                collation: Some(Ok(Label::Implicit(name.into()))),
                properties: Default::default(),
                json_fragment: false,
            })
            .collect(),
    );
    catalog
}
fn lower(sql: &str) -> String {
    let Statement::Query(mut query) = Parser::parse_sql(&ServerDialect, sql).unwrap().remove(0)
    else {
        panic!("query")
    };
    projection::ansi_padding::lower(&catalog(), &mut query, &Default::default()).unwrap();
    query.to_string()
}
#[test]
fn ansi_equality_preserves_scope_and_does_not_duplicate_operands() {
    assert_eq!(
        lower("SELECT v FROM dbo.t WHERE v='U'"),
        "SELECT v FROM dbo.t WHERE __msduck_rtrim(v, ' ') = __msduck_rtrim('U', ' ')"
    );
    for sql in [
        "SELECT v FROM dbo.t WHERE v='U'",
        "SELECT v FROM dbo.t WHERE v<>'U '",
        "WITH q AS (SELECT v FROM dbo.t) SELECT v FROM q WHERE v='U'",
        "SELECT a.v FROM dbo.t a JOIN dbo.t b ON a.v=b.v",
        "SELECT a.v FROM dbo.t a WHERE EXISTS(SELECT 1 FROM dbo.t b WHERE b.v=a.v)",
        "SELECT v FROM dbo.t WHERE v=NULL",
    ] {
        let result = lower(sql);
        assert_eq!(result.matches("__msduck_rtrim").count(), 2, "{result}");
    }
    let result = lower("SELECT v FROM dbo.t WHERE UPPER(v)='U'");
    assert_eq!(result.matches("UPPER(v)").count(), 1, "{result}");
    assert_eq!(result.matches("__msduck_rtrim").count(), 2, "{result}");
}
#[test]
fn padding_does_not_change_order_like_unicode_numeric_or_bin2_paths() {
    for sql in [
        "SELECT v FROM dbo.t WHERE v<'U'",
        "SELECT v FROM dbo.t WHERE v LIKE 'U'",
        "SELECT v FROM dbo.t WHERE v=n",
        "SELECT v FROM dbo.t WHERE v=i",
        "SELECT v FROM dbo.t WHERE missing='U'",
        "SELECT v FROM dbo.t WHERE v COLLATE Latin1_General_100_BIN2='U'",
    ] {
        assert!(!lower(sql).contains("__msduck_rtrim"), "{sql}");
    }
    // Forward aliases are not visible in the preceding ON predicate.
    let result = lower("SELECT a.v FROM dbo.t a JOIN dbo.t b ON c.v=a.v JOIN dbo.t c ON c.v=b.v");
    assert_eq!(result.matches("__msduck_rtrim").count(), 2, "{result}");
}
