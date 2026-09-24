//! Read-only projection provenance for persisted query-result column metadata.
use crate::declared_columns::Info;
use duckdb::{Connection, types::Value};
use msduck_sql::catalog_snapshot::CatalogSnapshot;
use sqlparser::ast::*;

pub use msduck_sql::binding_scope::{Field, QueryScopes, Scope};
use msduck_sql::projection as infer;

/// Retain logical definitions, not execution tickets or a stale property cache.
pub fn register(db: &Connection) -> duckdb::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS main.__msduck_view_definitions(object_id INTEGER PRIMARY KEY,query_sql VARCHAR NOT NULL)")?;
    sync(db)
}

pub fn sync(db: &Connection) -> duckdb::Result<()> {
    db.execute_batch("DELETE FROM main.__msduck_view_definitions d WHERE NOT EXISTS(SELECT 1 FROM sys.objects o WHERE o.object_id=d.object_id AND rtrim(o.type)='V')")
}

pub fn record_view_definition(db: &Connection, statement: &Statement) -> duckdb::Result<()> {
    let (name, query) = match statement {
        Statement::CreateView(view) => (&view.name, &view.query),
        Statement::AlterView { name, query, .. } => (name, query),
        _ => return Ok(()),
    };
    // Batch parsing marks explicit integer inputs once. Persist SQL without
    // those parser annotations so reparsing does not nest markers and lose
    // the original operand's logical type/provenance.
    struct Unmark;
    impl VisitorMut for Unmark {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &mut Expr) -> std::ops::ControlFlow<()> {
            match expr {
                Expr::Cast { expr, .. } | Expr::Convert { expr, .. } => {
                    msduck_sql::variant_cast::take(expr);
                }
                _ => {}
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    let mut query = query.clone();
    let _ = VisitMut::visit(&mut query, &mut Unmark);
    db.execute(
        "INSERT OR REPLACE INTO main.__msduck_view_definitions SELECT object_id,? FROM sys.objects WHERE object_id=__msduck_object_id(?,NULL) AND rtrim(type)='V'",
        duckdb::params![query.to_string(), name.to_string()],
    )?;
    Ok(())
}

#[derive(Default)]
struct ViewBinding {
    active: std::collections::HashSet<i32>,
    properties: std::collections::HashMap<i32, Option<Vec<msduck_core::result::Properties>>>,
}

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
    infer::ansi_padding::lower(&catalog, query, &scope)?;
    Ok(fields)
}

fn contains_unicode_operations<T: Visit>(node: &T, binary_only: bool) -> bool {
    struct Find {
        binary_only: bool,
    }
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &Expr) -> std::ops::ControlFlow<()> {
            if !self.binary_only
                && matches!(expr, Expr::Function(f) if matches!(f.name.to_string().to_ascii_uppercase().as_str(), "LOWER" | "UPPER" | "MIN" | "MAX"))
                || matches!(expr,
                    Expr::Cast { data_type, .. } | Expr::Convert { data_type: Some(data_type), .. }
                    if matches!(data_type, DataType::Nvarchar(_))
                    || matches!(data_type, DataType::Custom(name, _) if name.to_string().eq_ignore_ascii_case("nchar")))
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
    Visit::visit(node, &mut Find { binary_only }).is_break()
}

/// Bind a standalone initializer or predicate with no surrounding row source.
pub fn bind_unicode_expression(
    db: &Connection,
    expression: &mut Expr,
    parameters: &std::collections::HashMap<String, crate::parameter::Parameter>,
) -> anyhow::Result<()> {
    if !contains_unicode_operations(expression, false) {
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
    bind_operations(db, node, parameters, false)
}

/// Resolve character-to-binary declarations before character CAST lowering
/// replaces public type syntax with backend functions.
pub fn bind_binary_operations<T: Visit + VisitMut>(
    db: &Connection,
    node: &mut T,
    parameters: &std::collections::HashMap<String, crate::parameter::Parameter>,
) -> anyhow::Result<()> {
    bind_operations(db, node, parameters, true)
}

fn bind_operations<T: Visit + VisitMut>(
    db: &Connection,
    node: &mut T,
    parameters: &std::collections::HashMap<String, crate::parameter::Parameter>,
    binary_only: bool,
) -> anyhow::Result<()> {
    if !contains_unicode_operations(node, binary_only) {
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
        binary_only: bool,
    }
    impl VisitorMut for Annotate<'_> {
        type Break = msduck_core::diagnostic::SqlError;
        fn pre_visit_query(&mut self, query: &mut Query) -> std::ops::ControlFlow<Self::Break> {
            if self.depth == 0
                && !self.binary_only
                && let Err(error) =
                    infer::character_extrema::annotate(self.catalog, query, self.scope)
            {
                return std::ops::ControlFlow::Break(error);
            }
            if self.depth == 0
                && !self.binary_only
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
            binary_only,
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
    snapshot_with_views(db, query, &mut ViewBinding::default())
}

pub(crate) fn system_catalog_fields(view: &str, catalog_collation: &str) -> Option<Vec<Field>> {
    crate::index_catalog::fields(view, catalog_collation)
        .or_else(|| object_catalog_fields(view, catalog_collation))
}

// SQL Server system-view declarations and projection origins, captured with
// sys.all_columns and empty SELECT * results. Resource strings have a different
// collation from database-owned sysname values.
fn object_catalog_fields(view: &str, catalog_collation: &str) -> Option<Vec<Field>> {
    use msduck_core::{
        catalog::TypeMetadata,
        collation::Label,
        result::{Origin, Properties},
    };
    // name, system/user type, bytes, precision, scale, nullable, computed
    let view = view.to_ascii_lowercase();
    let mut fields = if matches!(view.as_str(), "tables" | "views") {
        object_catalog_fields("objects", catalog_collation)?
    } else {
        Vec::new()
    };
    let definitions = match view.as_str() {
        "schemas" => vec![
            ("name", 231, 256, 256, 0, 0, false, false),
            ("schema_id", 56, 56, 4, 10, 0, false, false),
            ("principal_id", 56, 56, 4, 10, 0, true, false),
        ],
        "objects" => vec![
            ("name", 231, 256, 256, 0, 0, false, false),
            ("object_id", 56, 56, 4, 10, 0, false, false),
            ("principal_id", 56, 56, 4, 10, 0, true, false),
            ("schema_id", 56, 56, 4, 10, 0, false, false),
            ("parent_object_id", 56, 56, 4, 10, 0, false, false),
            ("type", 175, 175, 2, 0, 0, true, true),
            ("type_desc", 231, 231, 120, 0, 0, true, false),
            ("create_date", 61, 61, 8, 23, 3, false, false),
            ("modify_date", 61, 61, 8, 23, 3, false, false),
            ("is_ms_shipped", 104, 104, 1, 1, 0, false, true),
            ("is_published", 104, 104, 1, 1, 0, false, true),
            ("is_schema_published", 104, 104, 1, 1, 0, false, true),
        ],
        "tables" => vec![
            ("lob_data_space_id", 56, 56, 4, 10, 0, false, true),
            ("filestream_data_space_id", 56, 56, 4, 10, 0, true, false),
            ("max_column_id_used", 56, 56, 4, 10, 0, false, false),
            ("lock_on_bulk_load", 104, 104, 1, 1, 0, false, true),
            ("uses_ansi_nulls", 104, 104, 1, 1, 0, true, true),
            ("is_replicated", 104, 104, 1, 1, 0, true, true),
            ("has_replication_filter", 104, 104, 1, 1, 0, true, true),
            ("is_merge_published", 104, 104, 1, 1, 0, true, true),
            ("is_sync_tran_subscribed", 104, 104, 1, 1, 0, true, true),
            (
                "has_unchecked_assembly_data",
                104,
                104,
                1,
                1,
                0,
                false,
                true,
            ),
            ("text_in_row_limit", 56, 56, 4, 10, 0, true, false),
            (
                "large_value_types_out_of_row",
                104,
                104,
                1,
                1,
                0,
                true,
                true,
            ),
            ("is_tracked_by_cdc", 104, 104, 1, 1, 0, true, true),
            ("lock_escalation", 48, 48, 1, 3, 0, true, true),
            ("lock_escalation_desc", 231, 231, 120, 0, 0, true, false),
            ("is_filetable", 104, 104, 1, 1, 0, true, true),
            ("is_memory_optimized", 104, 104, 1, 1, 0, true, true),
            ("durability", 48, 48, 1, 3, 0, true, true),
            ("durability_desc", 231, 231, 120, 0, 0, true, false),
            ("temporal_type", 48, 48, 1, 3, 0, true, true),
            ("temporal_type_desc", 231, 231, 120, 0, 0, true, true),
            ("history_table_id", 56, 56, 4, 10, 0, true, true),
            (
                "is_remote_data_archive_enabled",
                104,
                104,
                1,
                1,
                0,
                true,
                true,
            ),
            ("is_external", 104, 104, 1, 1, 0, false, true),
            ("history_retention_period", 56, 56, 4, 10, 0, true, true),
            (
                "history_retention_period_unit",
                56,
                56,
                4,
                10,
                0,
                true,
                true,
            ),
            (
                "history_retention_period_unit_desc",
                231,
                231,
                20,
                0,
                0,
                true,
                true,
            ),
            ("is_node", 104, 104, 1, 1, 0, true, true),
            ("is_edge", 104, 104, 1, 1, 0, true, true),
            ("data_retention_period", 56, 56, 4, 10, 0, true, true),
            ("data_retention_period_unit", 56, 56, 4, 10, 0, true, true),
            (
                "data_retention_period_unit_desc",
                231,
                231,
                20,
                0,
                0,
                true,
                true,
            ),
            ("ledger_type", 48, 48, 1, 3, 0, true, true),
            ("ledger_type_desc", 231, 231, 120, 0, 0, true, true),
            ("ledger_view_id", 56, 56, 4, 10, 0, true, false),
            ("is_dropped_ledger_table", 104, 104, 1, 1, 0, true, true),
        ],
        "views" => vec![
            ("is_replicated", 104, 104, 1, 1, 0, true, true),
            ("has_replication_filter", 104, 104, 1, 1, 0, true, true),
            ("has_opaque_metadata", 104, 104, 1, 1, 0, false, true),
            (
                "has_unchecked_assembly_data",
                104,
                104,
                1,
                1,
                0,
                false,
                true,
            ),
            ("with_check_option", 104, 104, 1, 1, 0, false, true),
            ("is_date_correlation_view", 104, 104, 1, 1, 0, false, true),
            ("is_tracked_by_cdc", 104, 104, 1, 1, 0, true, true),
            ("has_snapshot", 104, 104, 1, 1, 0, true, true),
            ("ledger_view_type", 48, 48, 1, 3, 0, true, true),
            ("ledger_view_type_desc", 231, 231, 120, 0, 0, true, true),
            ("is_dropped_ledger_view", 104, 104, 1, 1, 0, true, true),
        ],
        _ => return None,
    };
    fields.extend(definitions.into_iter().map(
        |(name, system, user, length, precision, scale, nullable, computed)| {
            let collation_name = matches!(system, 175 | 231).then(|| {
                if name != "name" {
                    "Latin1_General_CI_AS_KS_WS"
                } else {
                    catalog_collation
                }
                .to_owned()
            });
            Field {
                name: name.into(),
                info: Some(TypeMetadata {
                    system_type_id: Some(system),
                    user_type_id: Some(user),
                    max_length: Some(length),
                    precision: Some(precision),
                    scale: Some(scale),
                    collation_name: collation_name.clone(),
                }),
                collation: collation_name.map(|name| Ok(Label::Implicit(name))),
                properties: Properties {
                    nullable: Some(nullable),
                    origin: if computed {
                        Origin::Expression
                    } else {
                        Origin::Stored
                    },
                },
                json_fragment: false,
            }
        },
    ));
    Some(fields)
}

fn snapshot_with_views<T: Visit>(
    db: &Connection,
    query: &T,
    views: &mut ViewBinding,
) -> duckdb::Result<CatalogSnapshot> {
    struct Tables(std::collections::BTreeMap<String, ObjectName>);
    impl Visitor for Tables {
        type Break = ();
        fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> std::ops::ControlFlow<()> {
            if let TableFactor::Table {
                name, args: None, ..
            } = factor
            {
                self.0.insert(name.to_string(), name.clone());
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
    for (name, object_name) in names.0 {
        if let [
            ObjectNamePart::Identifier(schema),
            ObjectNamePart::Identifier(view),
        ] = object_name.0.as_slice()
            && schema.value.eq_ignore_ascii_case("sys")
            && let Some(collation) = catalog.default_collation.as_deref()
            && let Some(fields) = system_catalog_fields(&view.value, collation)
        {
            catalog.tables.insert(name, fields);
            continue;
        }
        let mut fields = columns
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
        let definition = db.prepare("SELECT d.object_id,d.query_sql FROM main.__msduck_view_definitions d JOIN sys.objects o ON o.object_id=d.object_id WHERE o.object_id=__msduck_object_id(?,NULL) AND rtrim(o.type)='V'")?
            .query_map([&name], |row| Ok((row.get::<_,i32>(0)?,row.get::<_,String>(1)?)))?
            .next().transpose()?;
        if let Some((id, sql)) = definition {
            if !views.properties.contains_key(&id)
                && views.active.len() < 32
                && views.active.insert(id)
            {
                let statements =
                    msduck_sql::batch::parse(&sql).map_err(|_| duckdb::Error::InvalidQuery)?;
                let properties = if let [Statement::Query(query)] = statements.as_slice() {
                    let inputs = snapshot_with_views(db, query.as_ref(), views)?;
                    infer::view_fields(&inputs, query).map(|fields| {
                        fields
                            .into_iter()
                            .map(|field| field.properties)
                            .collect::<Vec<_>>()
                    })
                } else {
                    None
                };
                views.active.remove(&id);
                views.properties.insert(id, properties);
            }
            if let Some(Some(properties)) = views.properties.get(&id)
                && properties.len() == fields.len()
            {
                for (field, properties) in fields.iter_mut().zip(properties) {
                    field.properties = *properties;
                }
            }
        }
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
    fn persisted_view_casts_rebind_without_duplicate_parser_annotations() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        for sql in [
            "CREATE TABLE dbo.cast_base(id INT IDENTITY NOT NULL,v INT)",
            "CREATE VIEW dbo.cast_view AS SELECT CAST(id AS BIGINT) AS converted,CAST(NULL AS INT) AS absent FROM dbo.cast_base",
            "CREATE VIEW dbo.cast_aggregate AS SELECT CAST(MIN(v) AS BIGINT) AS converted FROM dbo.cast_base",
        ] {
            let (tokens, ok) = session.batch_response(sql, &Default::default(), false, None);
            assert!(ok, "{sql}: {tokens:?}");
        }
        let definitions: Vec<String> = session
            .db
            .prepare("SELECT query_sql FROM main.__msduck_view_definitions")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<duckdb::Result<_>>()
            .unwrap();
        assert_eq!(definitions.len(), 2);
        assert!(
            definitions
                .iter()
                .all(|sql| !sql.contains("__msduck_explicit_integer_source"))
        );
        for _ in 0..3 {
            for sql in [
                "SELECT * FROM dbo.cast_view",
                "SELECT * FROM dbo.cast_aggregate",
            ] {
                let statements = msduck_sql::batch::parse(sql).unwrap();
                let sqlparser::ast::Statement::Query(query) = &statements[0] else {
                    unreachable!()
                };
                let fields = super::projection(&session.db, query).unwrap().unwrap();
                assert!(!fields.is_empty());
                assert!(
                    fields.iter().all(|field| field.properties
                        == msduck_core::result::Properties::expression(true)),
                    "{sql}"
                );
            }
        }
    }

    #[test]
    fn view_properties_survive_restart_and_follow_transactional_definitions() {
        use msduck_core::result::{Origin, Properties};
        let path = std::env::temp_dir().join(format!(
            "msduck-view-properties-{}-{}.duckdb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let run = |session: &mut crate::engine::Session, sql: &str| {
            let (tokens, ok) = session.batch_response(sql, &Default::default(), false, None);
            assert!(ok, "{sql}: {tokens:?}");
        };
        let properties = |session: &crate::engine::Session, sql: &str| {
            let mut statements = msduck_sql::batch::parse(sql).unwrap();
            let sqlparser::ast::Statement::Query(query) = statements.remove(0) else {
                unreachable!()
            };
            super::projection(&session.db, &query).unwrap().unwrap()[0].properties
        };
        let server = crate::server::Server::open(path.to_str().unwrap()).unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        run(
            &mut session,
            "CREATE TABLE dbo.provenance_base(id INT NOT NULL,v INT)",
        );
        let logical = properties(&session, "SELECT MIN(v) AS result FROM dbo.provenance_base");
        let computed = Properties {
            nullable: logical.nullable,
            origin: Origin::Stored,
        };
        let stored = properties(&session, "SELECT id FROM dbo.provenance_base");
        assert_eq!(computed.nullable, Some(true));
        assert_ne!(computed.origin, Origin::Unknown);
        assert_eq!(
            stored,
            Properties {
                nullable: Some(false),
                origin: Origin::Stored
            }
        );
        run(
            &mut session,
            "CREATE VIEW dbo.provenance_view(projected_value) AS SELECT MIN(v) AS result FROM dbo.provenance_base",
        );
        run(
            &mut session,
            "CREATE VIEW dbo.provenance_nested AS SELECT projected_value FROM dbo.provenance_view",
        );
        for name in ["provenance_view", "provenance_nested"] {
            assert_eq!(
                properties(&session, &format!("SELECT projected_value FROM dbo.{name}")),
                computed
            );
        }
        run(&mut session, "BEGIN TRAN");
        run(
            &mut session,
            "ALTER VIEW dbo.provenance_view AS SELECT id AS projected_value FROM dbo.provenance_base",
        );
        assert_eq!(
            properties(&session, "SELECT projected_value FROM dbo.provenance_view"),
            stored
        );
        assert_eq!(
            properties(
                &session,
                "SELECT projected_value FROM dbo.provenance_nested"
            ),
            stored
        );
        run(&mut session, "ROLLBACK");
        assert_eq!(
            properties(&session, "SELECT projected_value FROM dbo.provenance_view"),
            computed
        );
        assert!(
            !session
                .batch_response(
                    "ALTER VIEW dbo.provenance_view AS SELECT missing FROM dbo.provenance_base",
                    &Default::default(),
                    false,
                    None
                )
                .1
        );
        assert_eq!(
            properties(&session, "SELECT projected_value FROM dbo.provenance_view"),
            computed
        );
        run(&mut session, "CREATE TABLE dbo.provenance_unrelated(v INT)");
        drop(session);
        drop(server);
        let server = crate::server::Server::open(path.to_str().unwrap()).unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        assert_eq!(
            properties(&session, "SELECT projected_value FROM dbo.provenance_view"),
            computed
        );
        assert_eq!(
            properties(
                &session,
                "SELECT projected_value FROM dbo.provenance_nested"
            ),
            computed
        );
        run(
            &mut session,
            "DROP VIEW dbo.provenance_nested; DROP VIEW dbo.provenance_view",
        );
        run(
            &mut session,
            "CREATE VIEW dbo.provenance_view AS SELECT id AS projected_value FROM dbo.provenance_base",
        );
        assert_eq!(
            properties(&session, "SELECT projected_value FROM dbo.provenance_view"),
            stored
        );
        // A pre-migration view with no saved provenance must remain unknown.
        session
            .db
            .execute("DELETE FROM main.__msduck_view_definitions", [])
            .unwrap();
        assert_eq!(
            properties(&session, "SELECT projected_value FROM dbo.provenance_view"),
            Properties::default()
        );
        run(
            &mut session,
            "ALTER VIEW dbo.provenance_view AS SELECT MIN(v) AS projected_value FROM dbo.provenance_base",
        );
        assert_eq!(
            properties(&session, "SELECT projected_value FROM dbo.provenance_view"),
            computed
        );
        run(&mut session, "DROP VIEW dbo.provenance_view");
        assert_eq!(
            session
                .db
                .query_row(
                    "SELECT count(*) FROM main.__msduck_view_definitions",
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
