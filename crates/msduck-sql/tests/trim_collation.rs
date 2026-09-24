use msduck_core::{catalog::TypeMetadata, collation::Label};
use msduck_sql::{catalog_snapshot::CatalogSnapshot, dialect::ServerDialect, projection};
use sqlparser::{
    ast::{Query, Statement},
    parser::Parser,
};

fn query(sql: &str) -> Box<Query> {
    match Parser::parse_sql(&ServerDialect, sql).unwrap().remove(0) {
        Statement::Query(query) => query,
        _ => panic!("expected query"),
    }
}
fn catalog() -> CatalogSnapshot {
    let mut catalog = CatalogSnapshot {
        default_collation: Some("SQL_Latin1_General_CP1_CI_AS".into()),
        ..Default::default()
    };
    for (name, id) in [("nvarchar", 231), ("varchar", 167)] {
        catalog.types.insert(
            name.into(),
            TypeMetadata {
                system_type_id: Some(id),
                user_type_id: Some(i32::from(id)),
                max_length: Some(8000),
                ..Default::default()
            },
        );
    }
    catalog
}

#[test]
fn retained_trim_conflicts_survive_query_boundaries() {
    let catalog = catalog();
    for (sql, message) in CONFLICTS {
        let expr = sql
            .strip_prefix("SELECT ")
            .unwrap()
            .strip_suffix(" AS n")
            .unwrap();
        for sql in [
            sql.to_string(),
            format!("SELECT ({expr}) COLLATE Latin1_General_100_BIN2 AS n"),
            format!("SELECT 1 WHERE {expr} IS NULL"),
            format!("SELECT (SELECT {expr}) AS n"),
            format!("SELECT a.n FROM (SELECT 1 AS n) a JOIN (SELECT 2 AS n) b ON {expr}=N''"),
        ] {
            let error =
                projection::validate_query_operations(&catalog, &query(&sql), &Default::default())
                    .expect_err(&sql);
            assert_eq!(
                (error.number, error.state, error.severity),
                (468, 9, 16),
                "{sql}"
            );
            assert_eq!(error.message, *message, "{sql}");
        }
    }
}

#[test]
fn trim_binding_resolves_explicit_precedence_without_fabricating_unknown_labels() {
    let catalog = catalog();
    for expr in [
        "LTRIM(N'x' COLLATE Latin1_General_100_CI_AI,N'a')",
        "RTRIM(N'x',N'a' COLLATE Latin1_General_100_CI_AI)",
        "TRIM(N'a' COLLATE Latin1_General_100_CI_AI FROM N'x')",
        "TRIM(LEADING N'a' FROM N'x' COLLATE Latin1_General_100_CI_AI)",
        "TRIM(TRAILING N'a' COLLATE Latin1_General_100_CI_AI FROM N'x' COLLATE Latin1_General_100_CI_AI)",
    ] {
        let q = query(&format!("SELECT {expr} AS n"));
        projection::validate_query_operations(&catalog, &q, &Default::default()).unwrap();
        let fields = projection::query_fields(&catalog, &q, &Default::default()).unwrap();
        assert_eq!(
            fields[0].collation,
            Some(Ok(Label::Explicit("Latin1_General_100_CI_AI".into()))),
            "{expr}"
        );
    }
    for expr in [
        "LTRIM(unknown_value,N'a')",
        "RTRIM(N'x',unknown_set)",
        "TRIM(unknown_set FROM N'x')",
    ] {
        let q = query(&format!("SELECT {expr} AS n"));
        let fields = projection::query_fields(&catalog, &q, &Default::default()).unwrap();
        assert!(fields[0].collation.is_none(), "{expr}");
    }
}

// Exact SQL/error messages copied from the ten reference/trim-collation.json
// conflict captures. The full fixture also contains isolated surrogate strings,
// which Rust's UTF-8-only JSON strings cannot represent.
const CONFLICTS: &[(&str, &str)] = &[
    (
        "SELECT LTRIM((N'AéxÉa') COLLATE Latin1_General_100_CI_AI,(N'ae') COLLATE Latin1_General_100_CS_AS) AS n",
        "Cannot resolve the collation conflict between \"Latin1_General_100_CS_AS\" and \"Latin1_General_100_CI_AI\" in the ltrim operation.",
    ),
    (
        "SELECT RTRIM((N'AéxÉa') COLLATE Latin1_General_100_CI_AI,(N'ae') COLLATE Latin1_General_100_CS_AS) AS n",
        "Cannot resolve the collation conflict between \"Latin1_General_100_CS_AS\" and \"Latin1_General_100_CI_AI\" in the rtrim operation.",
    ),
    (
        "SELECT TRIM((N'ae') COLLATE Latin1_General_100_CS_AS FROM (N'AéxÉa') COLLATE Latin1_General_100_CI_AI) AS n",
        "Cannot resolve the collation conflict between \"Latin1_General_100_CI_AI\" and \"Latin1_General_100_CS_AS\" in the Trim operation.",
    ),
    (
        "SELECT TRIM(LEADING (N'ae') COLLATE Latin1_General_100_CS_AS FROM (N'AéxÉa') COLLATE Latin1_General_100_CI_AI) AS n",
        "Cannot resolve the collation conflict between \"Latin1_General_100_CI_AI\" and \"Latin1_General_100_CS_AS\" in the Trim operation.",
    ),
    (
        "SELECT TRIM(TRAILING (N'ae') COLLATE Latin1_General_100_CS_AS FROM (N'AéxÉa') COLLATE Latin1_General_100_CI_AI) AS n",
        "Cannot resolve the collation conflict between \"Latin1_General_100_CI_AI\" and \"Latin1_General_100_CS_AS\" in the Trim operation.",
    ),
    (
        "SELECT LTRIM((N'AéxÉa') COLLATE Latin1_General_100_CS_AS,(N'ae') COLLATE Latin1_General_100_CI_AI) AS n",
        "Cannot resolve the collation conflict between \"Latin1_General_100_CI_AI\" and \"Latin1_General_100_CS_AS\" in the ltrim operation.",
    ),
    (
        "SELECT RTRIM((N'AéxÉa') COLLATE Latin1_General_100_CS_AS,(N'ae') COLLATE Latin1_General_100_CI_AI) AS n",
        "Cannot resolve the collation conflict between \"Latin1_General_100_CI_AI\" and \"Latin1_General_100_CS_AS\" in the rtrim operation.",
    ),
    (
        "SELECT TRIM((N'ae') COLLATE Latin1_General_100_CI_AI FROM (N'AéxÉa') COLLATE Latin1_General_100_CS_AS) AS n",
        "Cannot resolve the collation conflict between \"Latin1_General_100_CS_AS\" and \"Latin1_General_100_CI_AI\" in the Trim operation.",
    ),
    (
        "SELECT TRIM(LEADING (N'ae') COLLATE Latin1_General_100_CI_AI FROM (N'AéxÉa') COLLATE Latin1_General_100_CS_AS) AS n",
        "Cannot resolve the collation conflict between \"Latin1_General_100_CS_AS\" and \"Latin1_General_100_CI_AI\" in the Trim operation.",
    ),
    (
        "SELECT TRIM(TRAILING (N'ae') COLLATE Latin1_General_100_CI_AI FROM (N'AéxÉa') COLLATE Latin1_General_100_CS_AS) AS n",
        "Cannot resolve the collation conflict between \"Latin1_General_100_CS_AS\" and \"Latin1_General_100_CI_AI\" in the Trim operation.",
    ),
];
