//! Read-only preparation of joined UPDATE row capture from catalog declarations.
//! This API does not execute assignments, create images, or mutate the target.
use anyhow::{Result, ensure};
use duckdb::Connection;
use msduck_sql::{
    binding_scope::{Field, Scope},
    output_join::Plan,
    output_target,
};
use sqlparser::{ast::*, dialect::GenericDialect, parser::Parser};
use std::{collections::HashMap, ops::ControlFlow};

pub struct Binding {
    pub target: TableFactor,
    /// Original target declarations, without outer-join null extension.
    pub target_fields: Vec<Field>,
    /// Logical source fields, including join nullability and parameter types.
    pub scope: Scope,
    /// Private physical columns correspond to source fields in declaration order.
    pub candidates: Plan,
    catalog: msduck_sql::catalog_snapshot::CatalogSnapshot,
    output_scope: Scope,
}

impl Binding {
    pub fn assignment_money(&self, update: &Update) -> Result<Vec<bool>> {
        let mut query = self.candidates.capture.clone();
        let SetExpr::Select(select) = query.body.as_mut() else {
            unreachable!()
        };
        select.qualify = None;
        select.projection = update
            .assignments
            .iter()
            .map(|a| SelectItem::UnnamedExpr(a.value.clone()))
            .collect();
        let fields = msduck_sql::projection::query_fields(&self.catalog, &query, &self.scope)
            .ok_or_else(|| anyhow::anyhow!("unresolved assignment declarations"))?;
        Ok(fields
            .iter()
            .map(|field| {
                field
                    .info
                    .as_ref()
                    .is_some_and(|info| matches!(info.system_type_id, Some(60 | 122)))
            })
            .collect())
    }

    pub fn candidate_declarations(&self) -> Result<msduck_sql::aggregate_columns::Columns> {
        let fields = self
            .scope
            .rows
            .last()
            .and_then(Option::as_ref)
            .ok_or_else(|| anyhow::anyhow!("missing candidate declarations"))?
            .iter()
            .flat_map(|source| &source.fields)
            .collect::<Vec<_>>();
        ensure!(
            fields.len() == self.candidates.columns.len(),
            "candidate declaration count changed"
        );
        let mut columns = vec![(
            self.candidates.identity.value.clone(),
            Some(DataType::BigInt(None)),
        )];
        columns.extend(
            self.candidates
                .columns
                .iter()
                .zip(fields)
                .map(|(column, field)| {
                    (
                        column.slot.value.clone(),
                        field
                            .info
                            .as_ref()
                            .and_then(|info| info.logical_type())
                            .map(crate::sql_type::ast),
                    )
                }),
        );
        Ok(columns)
    }

    pub fn image_declarations(
        &self,
        plan: &msduck_sql::output_update::Plan,
    ) -> Result<msduck_sql::aggregate_columns::Columns> {
        let mut columns = self.candidate_declarations()?;
        ensure!(
            plan.columns.len() == self.target_fields.len(),
            "image declaration count changed"
        );
        for (image, field) in plan.columns.iter().zip(&self.target_fields) {
            let kind = field
                .info
                .as_ref()
                .and_then(|info| info.logical_type())
                .map(crate::sql_type::ast);
            columns.push((image.deleted.value.clone(), kind.clone()));
            columns.push((image.inserted.value.clone(), kind));
        }
        Ok(columns)
    }

    /// Infer logical OUTPUT fields from the captured catalog snapshot, never
    /// from private image storage or current parameter values. Items must have
    /// already resolved column references; this is metadata, not validation.
    pub fn output_fields(&self, items: &[SelectItem]) -> Result<Vec<Field>> {
        let Statement::Query(mut query) =
            Parser::parse_sql(&GenericDialect {}, "SELECT 1")?.remove(0)
        else {
            unreachable!()
        };
        let SetExpr::Select(select) = query.body.as_mut() else {
            unreachable!()
        };
        select.projection = items.to_vec();
        msduck_sql::projection::query_fields(&self.catalog, &query, &self.output_scope)
            .ok_or_else(|| anyhow::anyhow!("unresolved joined OUTPUT metadata"))
    }

    /// Plan assignment evaluation after the adapter has captured candidates.
    /// The adapter must convert assigned inserted slots to target storage before
    /// materializing this capture. Logical target declarations stay here;
    /// private image fields are not metadata.
    pub fn images(
        &self,
        update: &Update,
        candidate_relation: ObjectName,
        image_relation: ObjectName,
        image_alias: Ident,
    ) -> Result<msduck_sql::output_update::Plan> {
        let mut physical = update.clone();
        physical.table.relation = self.target.clone();
        let columns = self
            .target_fields
            .iter()
            .map(|field| Ident::with_quote('"', &field.name))
            .collect::<Vec<_>>();
        msduck_sql::output_update::from_scoped_candidates(
            &physical,
            &columns,
            msduck_sql::output_bind::Context {
                candidates: &self.candidates,
                catalog: &self.catalog,
                scope: &self.scope,
            },
            candidate_relation,
            image_relation,
            image_alias,
        )
    }
}

pub fn bind(
    db: &Connection,
    update: &Update,
    with: Option<With>,
    parameters: &HashMap<String, crate::parameter::Parameter>,
) -> Result<Binding> {
    ensure!(
        update.limit.is_none() && update.order_by.is_empty() && update.or.is_none(),
        "joined OUTPUT capture requires separate ordered/limited update planning"
    );
    struct Names(std::collections::BTreeSet<String>);
    impl Visitor for Names {
        type Break = ();
        fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<()> {
            if let TableFactor::Table {
                name, args: None, ..
            } = factor
            {
                self.0.insert(name.to_string());
            }
            ControlFlow::Continue(())
        }
    }
    let mut names = Names(Default::default());
    let _ = update.visit(&mut names);
    let _ = with.visit(&mut names);
    let mut ids = HashMap::new();
    let mut lookup = db.prepare("SELECT CAST(__msduck_object_id(?,NULL) AS BIGINT)")?;
    for name in names.0 {
        let id: Option<i64> = lookup.query_row([&name], |row| row.get(0))?;
        if let Some(id) = id {
            ids.insert(name, id);
        }
    }
    let resolved = output_target::resolve(update, &ids)?;
    let TableFactor::Table {
        name,
        args: None,
        alias,
        ..
    } = &resolved.relation
    else {
        anyhow::bail!("joined OUTPUT target requires writable base-table resolution")
    };
    ensure!(
        alias.as_ref().is_none_or(|alias| alias.columns.is_empty()),
        "joined OUTPUT target column aliases require writable projection resolution"
    );
    let Statement::Query(mut logical) =
        Parser::parse_sql(&GenericDialect {}, "SELECT *")?.remove(0)
    else {
        unreachable!()
    };
    logical.with = with.clone();
    let SetExpr::Select(select) = logical.body.as_mut() else {
        unreachable!()
    };
    select.from = resolved.sources.clone();
    let catalog = crate::query_catalog::snapshot(
        db,
        &vec![
            Statement::Query(logical.clone()),
            Statement::Update(update.clone()),
        ],
    )?;
    let mut outer = Scope::default();
    for (name, parameter) in parameters {
        if let Some(info) = catalog.cast_info(&parameter.ast_type()) {
            outer.parameters.insert(name.to_lowercase(), info);
        }
    }
    let scope = msduck_sql::projection::scopes(&catalog, &logical, &outer).body;
    if let [part] = name.0.as_slice()
        && let Some(id) = part.as_ident()
    {
        ensure!(
            !scope.ctes.contains_key(&id.value.to_lowercase()),
            "joined OUTPUT CTE target requires writable projection resolution"
        );
    }
    let mut physical = update.clone();
    physical.table.relation = resolved.relation.clone();
    let stored = crate::output_update::columns(db, &physical)?;
    ensure!(
        !stored
            .iter()
            .any(|id| id.value.eq_ignore_ascii_case("rowid")),
        "joined OUTPUT target shadows physical row identity"
    );
    let target_fields = catalog
        .tables
        .get(&name.to_string())
        .ok_or_else(|| anyhow::anyhow!("missing joined OUTPUT target declarations"))?
        .clone();
    ensure!(
        stored.len() == target_fields.len()
            && stored
                .iter()
                .zip(&target_fields)
                .all(|(id, field)| id.value.eq_ignore_ascii_case(&field.name)),
        "joined OUTPUT target declarations disagree with storage"
    );
    let sources = scope
        .rows
        .last()
        .and_then(Option::as_ref)
        .ok_or_else(|| anyhow::anyhow!("joined OUTPUT requires resolved source columns"))?;
    let factors = resolved
        .sources
        .iter()
        .flat_map(|tree| {
            std::iter::once(&tree.relation).chain(tree.joins.iter().map(|join| &join.relation))
        })
        .collect::<Vec<_>>();
    ensure!(
        factors.len() == sources.len(),
        "joined OUTPUT source scope mismatch"
    );
    let target_index = factors
        .iter()
        .position(|factor| **factor == resolved.relation)
        .ok_or_else(|| anyhow::anyhow!("joined OUTPUT target missing from source scope"))?;
    let mut output_scope = scope.clone();
    let output_sources = output_scope
        .rows
        .last_mut()
        .and_then(Option::as_mut)
        .unwrap();
    output_sources.remove(target_index);
    for source in output_sources.iter_mut() {
        source
            .qualifiers
            .retain(|qualifier| !matches!(qualifier.as_str(), "inserted" | "deleted"));
    }
    for image in ["inserted", "deleted"] {
        output_sources.push(msduck_sql::binding_scope::Source {
            qualifiers: vec![image.into()],
            fields: target_fields.clone(),
        });
    }
    let mut columns = vec![];
    for (factor, source) in factors.into_iter().zip(sources) {
        let prefix = output_target::qualifier(factor)?;
        for field in &source.fields {
            let mut reference = prefix.clone();
            reference.push(Ident::with_quote('"', &field.name));
            columns.push(reference);
        }
    }
    let mut identity = output_target::qualifier(&resolved.relation)?;
    identity.push(Ident::new("rowid"));
    let candidates = msduck_sql::output_join::plan(
        with,
        resolved.sources,
        update.selection.clone(),
        identity,
        columns,
    )?;
    Ok(Binding {
        target: resolved.relation,
        target_fields,
        scope,
        candidates,
        catalog,
        output_scope,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use msduck_core::{
        character::{CharacterType, Family, Length},
        types::Type,
        value::Value,
    };

    fn update(sql: &str) -> Update {
        let Statement::Update(update) = Parser::parse_sql(&crate::dialect::ServerDialect, sql)
            .unwrap()
            .remove(0)
        else {
            unreachable!()
        };
        update
    }
    fn session() -> crate::engine::Session {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        assert!(session.batch_response(
            "CREATE TABLE target(id INT NOT NULL PRIMARY KEY,n INT NOT NULL); INSERT INTO target VALUES(10,1),(20,2); CREATE TABLE source(id INT NOT NULL,extra INT NOT NULL); INSERT INTO source VALUES(10,7),(10,9),(30,5)",
            &HashMap::new(), false, None).1);
        session
    }

    #[test]
    fn binding_is_read_only_and_separates_target_declarations_from_join_fields() {
        let session = session();
        let db = &session.db;
        let tables = || {
            db.query_row("SELECT count(*) FROM duckdb_tables()", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
        };
        let before = tables();
        for target in ["t", "target", "dbo.target"] {
            let update = update(&format!(
                "UPDATE {target} SET n=1/0 FROM target t FULL JOIN source s ON t.id=s.id WHERE 1/0=0"
            ));
            let original = update.clone();
            let bound = bind(db, &update, None, &HashMap::new()).unwrap();
            assert_eq!(update, original);
            assert_eq!(bound.target.to_string(), "target t");
            assert!(
                bound
                    .target_fields
                    .iter()
                    .all(|f| f.properties.nullable == Some(false))
            );
            let sources = bound.scope.rows.last().unwrap().as_ref().unwrap();
            assert!(
                sources
                    .iter()
                    .flat_map(|s| &s.fields)
                    .all(|f| f.properties.nullable == Some(true))
            );
            assert_eq!(
                bound
                    .candidates
                    .columns
                    .iter()
                    .map(|c| c
                        .reference
                        .iter()
                        .map(|i| i.value.as_str())
                        .collect::<Vec<_>>())
                    .collect::<Vec<_>>(),
                vec![
                    vec!["t", "id"],
                    vec!["t", "n"],
                    vec!["s", "id"],
                    vec!["s", "extra"]
                ]
            );
        }
        assert_eq!(tables(), before);
        assert_eq!(
            db.query_row("SELECT CAST(sum(n) AS BIGINT) FROM target", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            3
        );
        // The read-only binding result also produces executable native capture:
        // duplicates collapse, null-target rows disappear, unmatched targets stay.
        let bound = bind(
            db,
            &update("UPDATE target SET n=s.extra FROM target t FULL JOIN source s ON t.id=s.id"),
            None,
            &HashMap::new(),
        )
        .unwrap();
        let rows = db
            .prepare(&bound.candidates.capture.to_string())
            .unwrap()
            .query_map([], |r| {
                Ok((r.get::<_, i32>(1)?, r.get::<_, Option<i32>>(4)?))
            })
            .unwrap()
            .collect::<duckdb::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.contains(&(20, None)));
        assert!(rows.iter().any(|r| r.0 == 10 && matches!(r.1, Some(7 | 9))));
    }

    #[test]
    fn declarations_and_identifier_boundaries_survive_ctes_and_quoted_aliases() {
        let session = session();
        session
            .db
            .execute_batch("CREATE SEQUENCE binding_evaluations")
            .unwrap();
        let parameters = HashMap::from([(
            "@P".into(),
            crate::parameter::Parameter {
                data_type: Type::Character(
                    CharacterType::new(Family::Nvarchar, Length::Bounded(37)).unwrap(),
                ),
                value: Value::Null,
            },
        )]);
        let Statement::Query(query) = Parser::parse_sql(
            &crate::dialect::ServerDialect,
            "WITH c AS (SELECT @p AS [value.with.dot],nextval('binding_evaluations') AS evaluation) SELECT * FROM c",
        )
        .unwrap()
        .remove(0) else {
            unreachable!()
        };
        let bound = bind(
            &session.db,
            &update("UPDATE [t.dot] SET n=1 FROM target [t.dot] CROSS JOIN c [s.dot]"),
            query.with,
            &parameters,
        )
        .unwrap();
        let column = &bound.candidates.columns[2];
        assert_eq!(column.reference.len(), 2);
        assert_eq!(column.reference[0].value, "s.dot");
        assert_eq!(column.reference[1].value, "value.with.dot");
        let sources = bound.scope.rows.last().unwrap().as_ref().unwrap();
        let info = sources[1].fields[0].info.as_ref().unwrap();
        assert_eq!(info.max_length, Some(74));
        assert_eq!(info.system_type_id, Some(231));
        assert!(bound.candidates.capture.with.is_some());
        assert_eq!(
            session
                .db
                .query_row("SELECT nextval('binding_evaluations')", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        session
            .db
            .execute_batch("DROP TABLE target; DROP TABLE source")
            .unwrap();
        assert_eq!(bound.target_fields[0].name, "id");
        let Statement::Query(query) = Parser::parse_sql(
            &crate::dialect::ServerDialect,
            "SELECT inserted.id,[s.dot].[value.with.dot],@p AS parameter",
        )
        .unwrap()
        .remove(0) else {
            unreachable!()
        };
        let SetExpr::Select(select) = query.body.as_ref() else {
            unreachable!()
        };
        let fields = bound.output_fields(&select.projection).unwrap();
        assert_eq!(fields[0].properties.nullable, Some(false));
        assert_eq!(fields[1].name, "value.with.dot");
        assert_eq!(fields[1].info.as_ref().unwrap().max_length, Some(74));
        assert_eq!(fields[2].info.as_ref().unwrap().max_length, Some(74));
    }

    #[test]
    fn derived_and_shadowed_targets_do_not_bind_to_unrelated_storage() {
        let session = session();
        assert!(
            bind(
                &session.db,
                &update("UPDATE t SET n=1 FROM (SELECT id,n FROM target) t"),
                None,
                &HashMap::new()
            )
            .is_err()
        );
        let Statement::Query(query) = Parser::parse_sql(
            &crate::dialect::ServerDialect,
            "WITH target AS (SELECT id,extra AS n FROM source) SELECT * FROM target",
        )
        .unwrap()
        .remove(0) else {
            unreachable!()
        };
        let error = bind(
            &session.db,
            &update("UPDATE t SET n=1 FROM target t"),
            query.with,
            &HashMap::new(),
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("CTE target"));
        assert_eq!(
            session
                .db
                .query_row("SELECT CAST(sum(n) AS BIGINT) FROM target", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            3
        );
    }

    #[test]
    fn catalog_binding_drives_paired_writes_and_old_new_projection() {
        let session = session();
        let db = &session.db;
        let update = update(
            "UPDATE target SET id=t.id+100,n=CAST(t.n+COALESCE(s.extra,0) AS INT) OUTPUT deleted.*,inserted.*,s.*,deleted.n+COALESCE(s.extra,0) AS computed FROM target t FULL JOIN source s ON t.id=s.id",
        );
        let bound = bind(db, &update, None, &HashMap::new()).unwrap();
        let images = bound
            .images(
                &update,
                ObjectName::from(vec![Ident::new("candidates")]),
                ObjectName::from(vec![Ident::new("images")]),
                Ident::new("captured"),
            )
            .unwrap();
        let Some(OutputClause::Output { select_items, .. }) = &update.output else {
            unreachable!()
        };
        let output = images.projection(select_items).unwrap();
        let fields = bound.output_fields(select_items).unwrap();
        assert_eq!(
            fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            ["id", "n", "id", "n", "id", "extra", "computed"]
        );
        assert!(
            fields[..4]
                .iter()
                .all(|field| field.properties.nullable == Some(false))
        );
        assert!(
            fields[4..6]
                .iter()
                .all(|field| field.properties.nullable == Some(true))
        );
        db.execute_batch("BEGIN TRANSACTION").unwrap();
        db.execute_batch(&format!(
            "CREATE TEMP TABLE candidates AS {}",
            bound.candidates.capture
        ))
        .unwrap();
        db.execute_batch(&format!("CREATE TEMP TABLE images AS {}", images.capture))
            .unwrap();
        db.execute_batch(&images.write.to_string()).unwrap();
        db.execute_batch("UPDATE source SET extra=999").unwrap();
        let mut rows = db
            .prepare(&output.to_string())
            .unwrap()
            .query_map([], |r| {
                Ok((
                    r.get::<_, i32>(0)?,
                    r.get::<_, i32>(1)?,
                    r.get::<_, i32>(2)?,
                    r.get::<_, i32>(3)?,
                    r.get::<_, Option<i32>>(4)?,
                    r.get::<_, Option<i32>>(5)?,
                    r.get::<_, i32>(6)?,
                ))
            })
            .unwrap()
            .collect::<duckdb::Result<Vec<_>>>()
            .unwrap();
        rows.sort();
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].0, rows[0].1, rows[0].2), (10, 1, 110));
        assert!(matches!(rows[0].3, 8 | 10));
        assert_eq!(rows[0].4, Some(10));
        assert_eq!(rows[0].5.map(|extra| extra + 1), Some(rows[0].3));
        assert_eq!(rows[0].6, rows[0].3);
        assert_eq!(rows[1], (20, 2, 120, 2, None, None, 2));
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM target WHERE id IN (110,120)",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
        db.execute_batch("ROLLBACK").unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM target WHERE id IN (10,20)", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert!(db.prepare("SELECT * FROM candidates").is_err());
        assert!(db.prepare("SELECT * FROM images").is_err());
    }

    #[test]
    fn scoped_assignments_keep_inner_rows_and_captured_outer_inputs() {
        let mut session = session();
        assert!(session.batch_response("CREATE TABLE lookup(id INT NOT NULL,n INT NOT NULL); INSERT INTO lookup VALUES(10,50),(20,60)", &HashMap::new(),false,None).1);
        let db = &session.db;
        let update = update(
            "UPDATE target SET id=t.id+100,n=CAST(COALESCE((SELECT q.n+extra FROM lookup q WHERE q.id=t.id),n) AS INT) OUTPUT deleted.n,inserted.n,s.extra FROM target t LEFT JOIN source s ON t.id=s.id",
        );
        let bound = bind(db, &update, None, &HashMap::new()).unwrap();
        let images = bound
            .images(
                &update,
                ObjectName::from(vec![Ident::new("candidates")]),
                ObjectName::from(vec![Ident::new("images")]),
                Ident::new("captured"),
            )
            .unwrap();
        let Some(OutputClause::Output { select_items, .. }) = &update.output else {
            unreachable!()
        };
        let output = images.projection(select_items).unwrap();
        db.execute_batch("BEGIN TRANSACTION").unwrap();
        db.execute_batch(&format!(
            "CREATE TEMP TABLE candidates AS {}",
            bound.candidates.capture
        ))
        .unwrap();
        db.execute_batch("UPDATE source SET extra=999").unwrap();
        db.execute_batch(&format!("CREATE TEMP TABLE images AS {}", images.capture))
            .unwrap();
        db.execute_batch(&images.write.to_string()).unwrap();
        let mut rows = db
            .prepare(&output.to_string())
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, i32>(0)?,
                    row.get::<_, i32>(1)?,
                    row.get::<_, Option<i32>>(2)?,
                ))
            })
            .unwrap()
            .collect::<duckdb::Result<Vec<_>>>()
            .unwrap();
        rows.sort();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, 1);
        assert!(matches!(rows[0].2, Some(7 | 9)));
        assert_eq!(rows[0].1, 50 + rows[0].2.unwrap());
        assert_eq!(rows[1], (2, 2, None));
        db.execute_batch("ROLLBACK").unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM target WHERE id IN (10,20)", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
}
