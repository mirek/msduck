//! Read-only projection provenance for persisted query-result column metadata.
use crate::declared_columns::Info;
use duckdb::{Connection, types::Value};
use msduck_sql::catalog_snapshot::CatalogSnapshot;
use sqlparser::ast::*;

pub use msduck_sql::binding_scope::{Field, QueryScopes, Scope};
use msduck_sql::projection as infer;

/// Adapt only aligned, resolved logical labels to known wire descriptors.
/// Unknown labels and unsupported mappings retain the existing wire fallback;
/// this adapter is not SQL collation validation or comparison dispatch.
pub fn wire_collation(
    fields: &[Field],
    width: usize,
    index: usize,
) -> Option<crate::tds::collation::Collation> {
    if fields.len() != width {
        return None;
    }
    let label = fields.get(index)?.collation.as_ref()?.as_ref().ok()?;
    crate::tds::collation::Collation::for_name(label.name()?)
}

pub fn scopes(db: &Connection, query: &Query, outer: &Scope) -> duckdb::Result<QueryScopes> {
    Ok(infer::scopes(&snapshot(db, query)?, query, outer))
}
pub fn projection_in_scope(
    db: &Connection,
    query: &Query,
    outer: &Scope,
) -> duckdb::Result<Option<Vec<Field>>> {
    Ok(infer::query_fields(&snapshot(db, query)?, query, outer))
}
pub fn projection(db: &Connection, query: &Query) -> duckdb::Result<Option<Vec<Field>>> {
    projection_in_scope(db, query, &Scope::default())
}

/// Bind declarations independently of their current values and row-source scope.
pub fn projection_with_parameters(
    db: &Connection,
    query: &Query,
    parameters: &std::collections::HashMap<String, crate::parameter::Parameter>,
) -> duckdb::Result<Option<Vec<Field>>> {
    let catalog = snapshot(db, query)?;
    let mut scope = Scope::default();
    for (name, parameter) in parameters {
        if let Some(info) = catalog.cast_info(&parameter.ast_type()) {
            scope.parameters.insert(name.to_lowercase(), info);
        }
    }
    Ok(infer::query_fields(&catalog, query, &scope))
}

pub fn checked_projection_with_parameters(
    db: &Connection,
    query: &Query,
    parameters: &std::collections::HashMap<String, crate::parameter::Parameter>,
) -> anyhow::Result<Option<Vec<Field>>> {
    let catalog = snapshot(db, query)?;
    let mut scope = Scope::default();
    for (name, parameter) in parameters {
        if let Some(info) = catalog.cast_info(&parameter.ast_type()) {
            scope.parameters.insert(name.to_lowercase(), info);
        }
    }
    infer::validate_query_operations(&catalog, query, &scope)?;
    Ok(infer::query_fields(&catalog, query, &scope))
}

/// Retain logical result fields before applying backend comparison calls.
pub fn bind_query_with_parameters(
    db: &Connection,
    query: &mut Query,
    parameters: &std::collections::HashMap<String, crate::parameter::Parameter>,
) -> anyhow::Result<Option<Vec<Field>>> {
    let catalog = snapshot(db, query)?;
    let mut scope = Scope::default();
    for (name, parameter) in parameters {
        if let Some(info) = catalog.cast_info(&parameter.ast_type()) {
            scope.parameters.insert(name.to_lowercase(), info);
        }
    }
    let fields = infer::query_fields(&catalog, query, &scope);
    infer::lower_bin2_comparisons(&catalog, query, &scope)?;
    Ok(fields)
}

fn contains_unicode_operations<T: Visit>(node: &T) -> bool {
    struct Find;
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &Expr) -> std::ops::ControlFlow<()> {
            if matches!(expr, Expr::Function(f) if matches!(f.name.to_string().to_ascii_uppercase().as_str(), "LOWER" | "UPPER"))
                || matches!(
                    expr,
                    Expr::Cast {
                        data_type: DataType::Binary(_) | DataType::Varbinary(_),
                        ..
                    } | Expr::Convert {
                        data_type: Some(DataType::Binary(_) | DataType::Varbinary(_)),
                        ..
                    }
                )
            {
                return std::ops::ControlFlow::Break(());
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    Visit::visit(node, &mut Find).is_break()
}

/// Bind a standalone initializer or predicate with no surrounding row source.
pub fn bind_unicode_expression(
    db: &Connection,
    expression: &mut Expr,
    parameters: &std::collections::HashMap<String, crate::parameter::Parameter>,
) -> anyhow::Result<()> {
    if !contains_unicode_operations(expression) {
        return Ok(());
    }
    // Bind standalone initializers/predicates in an empty row scope, preserving
    // nested query scopes. Move the AST into a SELECT; never reparse user text.
    let Statement::Query(mut query) = msduck_sql::batch::parse("SELECT 0")?.remove(0) else {
        unreachable!("constant SELECT template")
    };
    let SetExpr::Select(select) = query.body.as_mut() else {
        unreachable!("constant SELECT body")
    };
    select.projection = vec![SelectItem::UnnamedExpr(expression.clone())];
    bind_unicode_operations(db, query.as_mut(), parameters)?;
    let SetExpr::Select(select) = query.body.as_mut() else {
        unreachable!("retained SELECT body")
    };
    let SelectItem::UnnamedExpr(bound) = select.projection.remove(0) else {
        unreachable!("retained scalar projection")
    };
    *expression = bound;
    Ok(())
}

/// Annotate complete query scopes after logical metadata acquisition and before
/// physical translation. Nested queries are covered by the outer query's plan.
pub fn bind_unicode_operations<T: Visit + VisitMut>(
    db: &Connection,
    node: &mut T,
    parameters: &std::collections::HashMap<String, crate::parameter::Parameter>,
) -> anyhow::Result<()> {
    if !contains_unicode_operations(node) {
        return Ok(());
    }
    let catalog = snapshot(db, node)?;
    let mut scope = Scope::default();
    for (name, parameter) in parameters {
        if let Some(info) = catalog.cast_info(&parameter.ast_type()) {
            scope.parameters.insert(name.to_lowercase(), info);
        }
    }
    struct Annotate<'a> {
        catalog: &'a CatalogSnapshot,
        scope: &'a Scope,
        depth: usize,
    }
    impl VisitorMut for Annotate<'_> {
        type Break = msduck_core::diagnostic::SqlError;
        fn pre_visit_query(&mut self, query: &mut Query) -> std::ops::ControlFlow<Self::Break> {
            if self.depth == 0
                && let Err(error) =
                    infer::annotate_unicode_case_inputs(self.catalog, query, self.scope)
            {
                return std::ops::ControlFlow::Break(error);
            }
            if self.depth == 0
                && let Err(error) =
                    infer::lower_unicode_binary_conversions(self.catalog, query, self.scope)
            {
                return std::ops::ControlFlow::Break(error);
            }
            self.depth += 1;
            std::ops::ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &mut Query) -> std::ops::ControlFlow<Self::Break> {
            self.depth -= 1;
            std::ops::ControlFlow::Continue(())
        }
    }
    match VisitMut::visit(
        node,
        &mut Annotate {
            catalog: &catalog,
            scope: &scope,
            depth: 0,
        },
    ) {
        std::ops::ControlFlow::Continue(()) => Ok(()),
        std::ops::ControlFlow::Break(error) => Err(error.into()),
    }
}

pub fn expand_qualified_stars(
    db: &Connection,
    query: &mut Query,
    outer: &Scope,
) -> anyhow::Result<()> {
    if !msduck_sql::for_json::projection(&query.body)?
        .projection
        .iter()
        .any(|item| matches!(item, SelectItem::QualifiedWildcard(..)))
    {
        return Ok(());
    }
    let catalog = snapshot(db, query)?;
    infer::expand_qualified_stars(&catalog, query, outer)
        .ok_or_else(|| anyhow::anyhow!("nested FOR JSON star requires known source columns"))
}

pub fn validate_cte_columns<T: Visit>(db: &Connection, node: &T) -> anyhow::Result<()> {
    if !msduck_sql::cte_columns::required(node) {
        return Ok(());
    }
    msduck_sql::cte_columns::validate(node, &snapshot(db, node)?)
        .map_err(|error| anyhow::Error::msg(error.message))
}

pub fn lower_recursion<T: Visit + VisitMut + Clone>(
    db: &Connection,
    node: &mut T,
) -> anyhow::Result<()> {
    if !msduck_sql::recursive_lower::required(node) {
        return Ok(());
    }
    msduck_sql::recursive_lower::lower(node, &snapshot(db, node)?).map_err(anyhow::Error::msg)
}

/// Acquire inputs once. No query expressions are evaluated while collecting metadata.
pub(crate) fn snapshot<T: Visit>(db: &Connection, query: &T) -> duckdb::Result<CatalogSnapshot> {
    struct Tables(std::collections::BTreeSet<String>);
    impl Visitor for Tables {
        type Break = ();
        fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> std::ops::ControlFlow<()> {
            if let TableFactor::Table {
                name, args: None, ..
            } = factor
            {
                self.0.insert(name.to_string());
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    let mut names = Tables(Default::default());
    let _ = query.visit(&mut names);
    let mut catalog = CatalogSnapshot::default();
    let mut types = db.prepare("SELECT name,system_type_id,user_type_id,max_length,precision,scale,collation_name FROM sys.types")?;
    catalog.types = types
        .query_map([], |row| {
            Ok((row.get(0)?, crate::declared_columns::read_info(row, 1)?))
        })?
        .collect::<duckdb::Result<_>>()?;
    // This server currently exposes one database and one catalog default.
    // Multi-database acquisition must supply the selected database's default.
    catalog.default_collation = catalog
        .types
        .get("nvarchar")
        .and_then(|t| t.collation_name.clone());
    let mut columns = db.prepare("SELECT c.name,c.system_type_id,c.user_type_id,c.max_length,c.precision,c.scale,c.collation_name,c.is_nullable,c.is_identity,o.type FROM sys.columns c JOIN sys.objects o ON c.object_id=o.object_id WHERE c.object_id=__msduck_object_id(?,NULL) ORDER BY c.column_id")?;
    for name in names.0 {
        let fields = columns
            .query_map([&name], |row| {
                Ok(Field {
                    collation: row
                        .get::<_, Option<String>>(6)?
                        .map(|name| Ok(msduck_core::collation::Label::Implicit(name))),
                    name: row.get(0)?,
                    properties: if row.get::<_, String>(9)?.trim() == "U" {
                        msduck_core::result::Properties {
                            nullable: Some(row.get(7)?),
                            origin: if row.get::<_, bool>(8)? {
                                msduck_core::result::Origin::Identity
                            } else {
                                msduck_core::result::Origin::Stored
                            },
                        }
                    } else {
                        // A view's backend columns do not establish whether its
                        // original projection was stored, computed or nullable.
                        Default::default()
                    },
                    json_fragment: false,
                    info: Some(crate::declared_columns::read_info(row, 1)?),
                })
            })?
            .collect::<duckdb::Result<Vec<_>>>()?;
        catalog.tables.insert(name, fields);
    }
    Ok(catalog)
}

// Unknown branches must not be treated as NULL or assigned a guessed scale.
pub fn time_scale(info: Option<&Info>) -> Option<u8> {
    info?.time_scale()
}

pub fn view(
    db: &Connection,
    statement: &Statement,
) -> duckdb::Result<Option<(String, Vec<Field>)>> {
    let (name, query) = match statement {
        Statement::CreateView(v) => (&v.name, &v.query),
        Statement::AlterView { name, query, .. } => (name, query),
        _ => return Ok(None),
    };
    Ok(Some((
        name.to_string(),
        projection(db, query)?.unwrap_or_default(),
    )))
}
pub fn record(db: &Connection, name: &str, fields: &[Field]) -> duckdb::Result<()> {
    let id: Option<i32> =
        db.query_row("SELECT __msduck_object_id(?,NULL)", [name], |r| r.get(0))?;
    let Some(id) = id else { return Ok(()) };
    db.execute(
        "DELETE FROM main.__msduck_declared_columns WHERE object_id=?",
        [id],
    )?;
    let count: i64 = db.query_row(
        "SELECT count(*) FROM main.__msduck_column_info WHERE object_id=?",
        [id],
        |r| r.get(0),
    )?;
    if count as usize != fields.len() {
        return Ok(());
    }
    for (index, field) in fields.iter().enumerate() {
        if let Some(info) = &field.info {
            let values = std::iter::once(Value::Int(id))
                .chain(std::iter::once(Value::Int(index as i32 + 1)))
                .chain(crate::declared_columns::info_values(info));
            db.execute(
                "INSERT INTO main.__msduck_declared_columns VALUES(?,?,?,?,?,?,?,?)",
                duckdb::params_from_iter(values),
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn view_and_into_declarations_survive_restart_and_unrelated_ddl() {
        let path = std::env::temp_dir().join(format!(
            "msduck-provenance-{}-{}.duckdb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let run = |s: &mut crate::engine::Session, sql| {
            assert!(
                s.batch_response(sql, &std::collections::HashMap::new(), false, None)
                    .1,
                "{sql}"
            )
        };
        let server = crate::server::Server::open(path.to_str().unwrap()).unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        run(&mut session, "CREATE TABLE dbo.base(n NVARCHAR(37))");
        run(
            &mut session,
            "CREATE VIEW dbo.projected AS SELECT n FROM dbo.base",
        );
        run(
            &mut session,
            "SELECT n INTO dbo.copied FROM dbo.projected; CREATE TABLE dbo.unrelated(v INT)",
        );
        drop(session);
        drop(server);
        let server = crate::server::Server::open(path.to_str().unwrap()).unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        for table in ["projected", "copied"] {
            assert_eq!(session.db.query_row("SELECT user_type_id,max_length FROM sys.columns WHERE object_id=__msduck_object_id(?,NULL)",[table],|r|Ok((r.get::<_,i32>(0)?,r.get::<_,i16>(1)?))).unwrap(),(231,74));
        }
        run(
            &mut session,
            "DROP VIEW dbo.projected; DROP TABLE dbo.copied; DROP TABLE dbo.base; DROP TABLE dbo.unrelated",
        );
        assert_eq!(
            session
                .db
                .query_row(
                    "SELECT count(*) FROM main.__msduck_declared_columns",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        drop(session);
        drop(server);
        std::fs::remove_file(path).unwrap();
    }
}

#[cfg(test)]
mod scope_tests {
    use super::*;
    #[test]
    fn cte_snapshots_shadow_tables_without_exposing_later_declarations() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let db = server.connection().unwrap();
        db.execute_batch("SET schema='dbo'; CREATE TABLE future(wrong INT)")
            .unwrap();
        let statements = sqlparser::parser::Parser::parse_sql(&crate::dialect::ServerDialect,
            "WITH first_cte AS (SELECT * FROM future), future AS (SELECT 1 AS right_name) SELECT * FROM future").unwrap();
        let Statement::Query(query) = &statements[0] else {
            unreachable!()
        };
        let snapshots = scopes(&db, query, &Scope::default()).unwrap();
        assert_eq!(snapshots.definitions.len(), 2);
        assert!(snapshots.definitions[0].get("future").unwrap().is_empty());
        assert!(
            snapshots.definitions[1]
                .get("first_cte")
                .unwrap()
                .is_empty()
        );
        assert_eq!(snapshots.body.get("future").unwrap()[0].name, "right_name");
        assert!(snapshots.inherited.is_empty());
        let statements = sqlparser::parser::Parser::parse_sql(
            &crate::dialect::ServerDialect,
            "SELECT * FROM future CROSS JOIN (SELECT 1 AS known) k",
        )
        .unwrap();
        let Statement::Query(star) = &statements[0] else {
            unreachable!()
        };
        assert!(
            projection_in_scope(&db, star, &snapshots.definitions[0])
                .unwrap()
                .is_none()
        );
        assert_eq!(
            projection_in_scope(&db, star, &snapshots.body)
                .unwrap()
                .unwrap()
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>(),
            ["right_name", "known"]
        );
    }
}

#[cfg(test)]
mod row_scope_tests {
    use super::*;
    fn query(sql: &str) -> Box<Query> {
        let statement = sqlparser::parser::Parser::parse_sql(&crate::dialect::ServerDialect, sql)
            .unwrap()
            .remove(0);
        let Statement::Query(query) = statement else {
            unreachable!()
        };
        query
    }
    #[test]
    fn metadata_lookup_respects_nearest_names_qualifiers_and_unknown_scopes() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let db = server.connection().unwrap();
        let parent = query(
            "SELECT * FROM (SELECT CAST(1 AS MONEY) AS price,CAST('01:02:03.12' AS TIME(2)) AS clock) p",
        );
        let scope = scopes(&db, &parent, &Scope::default()).unwrap().body;
        let child = query("SELECT p.price,clock");
        let fields = projection_in_scope(&db, &child, &scope).unwrap().unwrap();
        assert!(fields[0].info.is_some());
        assert_eq!(time_scale(fields[1].info.as_ref()), Some(2));
        for sql in [
            "SELECT price FROM (SELECT 1 AS price) q",
            "SELECT p.price FROM (SELECT 1 AS other) p",
            "SELECT price FROM (SELECT 1 AS price) q CROSS JOIN (SELECT 2 AS price) r",
        ] {
            let fields = projection_in_scope(&db, &query(sql), &scope)
                .unwrap()
                .unwrap();
            assert!(fields[0].info.is_none(), "{sql}");
        }
        let mut unknown = scope.clone();
        unknown.rows.push(None);
        assert!(
            projection_in_scope(&db, &child, &unknown).unwrap().unwrap()[0]
                .info
                .is_none()
        );
        let ctes = query("WITH c AS (SELECT p.price AS v) SELECT * FROM c");
        let definitions = scopes(&db, &ctes, &scope).unwrap();
        assert!(definitions.definitions[0].rows.is_empty());
        assert!(definitions.body.get("c").unwrap()[0].info.is_none());
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    #[test]
    fn captured_metadata_survives_ddl_and_connection_close() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        let run = |session: &mut crate::engine::Session, sql| {
            assert!(
                session
                    .batch_response(sql, &Default::default(), false, None)
                    .1,
                "{sql}"
            );
        };
        run(
            &mut session,
            "CREATE TABLE dbo.snapshot_source(t TIME(2), n NVARCHAR(37))",
        );
        let statements = sqlparser::parser::Parser::parse_sql(&crate::dialect::ServerDialect,
            "WITH c AS (SELECT * FROM dbo.snapshot_source) SELECT t,n,CAST(NULL AS NVARCHAR) AS label FROM c").unwrap();
        let Statement::Query(query) = &statements[0] else {
            unreachable!()
        };
        let before = snapshot(&session.db, query).unwrap();
        run(
            &mut session,
            "ALTER TABLE dbo.snapshot_source ALTER COLUMN t TIME(5)",
        );
        let after = snapshot(&session.db, query).unwrap();
        run(&mut session, "DROP TABLE dbo.snapshot_source");
        let removed = snapshot(&session.db, query).unwrap();
        drop(session);
        drop(server);
        for _ in 0..3 {
            let fields = infer::query_fields(&before, query, &Scope::default()).unwrap();
            assert_eq!(time_scale(fields[0].info.as_ref()), Some(2));
            assert_eq!(fields[1].info.as_ref().unwrap().max_length, Some(74));
            assert_eq!(fields[2].info.as_ref().unwrap().max_length, Some(60));
            let fields = infer::query_fields(&after, query, &Scope::default()).unwrap();
            assert_eq!(time_scale(fields[0].info.as_ref()), Some(5));
            assert!(infer::query_fields(&removed, query, &Scope::default()).is_none());
        }
    }
}
