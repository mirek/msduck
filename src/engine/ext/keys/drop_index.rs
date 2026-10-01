//! DROP INDEX over registered indexes and key constraints.
//!
//! The binder resolves each target in order against registered indexes
//! (including keys-managed ones) and the recorded PRIMARY KEY and UNIQUE
//! constraints. A constraint's index cannot be dropped (3723). As in the
//! built-in path, each drop commits on its own outside a user transaction,
//! so earlier targets stay dropped when a later one fails.
use super::{catalog, error};
use crate::engine::{Execution, Session};
use anyhow::Result;
use msduck_sql::drop_index::{self as bind, OptionClause, Request};
use sqlparser::ast::{Ident, ObjectName};

/// Binder IDs for constraints, outside the catalog's index ID range.
const CONSTRAINT_IDS: u64 = 1 << 40;

pub(super) fn run(session: &mut Session, request: Request) -> Result<Execution> {
    session.require_committable()?;
    let db = &session.db;
    let unknown: i64 = db.query_row(
        "SELECT count(*) FROM duckdb_indexes() i
         JOIN sys.schemas s ON s.name=i.schema_name
         JOIN sys.objects o ON o.schema_id=s.schema_id AND o.name=i.table_name AND rtrim(o.type)='U'
         WHERE i.database_name=current_database()
           AND NOT EXISTS(SELECT 1 FROM main.__msduck_index_catalog c
             WHERE c.backend_schema=i.schema_name AND c.backend_name=i.index_name AND c.object_id=o.object_id)
           AND NOT EXISTS(SELECT 1 FROM main.__msduck_keys k
             WHERE k.backend_name=i.index_name AND k.object_id=o.object_id)",
        [],
        |r| r.get(0),
    )?;
    anyhow::ensure!(
        unknown == 0,
        "unmanaged index catalog is incomplete; reconcile before binding"
    );
    // `acquire` can repeat a row when another database has a table of the
    // same name; the repeated rows are identical.
    let mut registered = crate::index_catalog::acquire(db)?;
    registered.dedup_by(|a, b| a.object_id == b.object_id && a.index_id == b.index_id);
    let keys = catalog::all(db)?;
    let tables = db
        .prepare("SELECT o.object_id,s.name,o.name FROM sys.objects o JOIN sys.schemas s USING(schema_id) WHERE rtrim(o.type)='U'")?
        .query_map([], |r| {
            Ok(bind::Table {
                id: r.get::<_, i32>(0)? as u64,
                schema: r.get(1)?,
                name: r.get(2)?,
            })
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;
    let mut indexes: Vec<bind::Index> = registered
        .iter()
        .map(|i| bind::Index {
            table_id: i.object_id as u64,
            id: i.index_id as u64,
            name: i.name.clone(),
            clustered: false,
            constraint_backed: false,
            backend_name: ObjectName::from(vec![
                Ident::with_quote('"', &i.backend_schema),
                Ident::with_quote('"', &i.backend_name),
            ]),
        })
        .collect();
    let constraints: Vec<&catalog::Key> = keys.iter().filter(|k| k.constraint()).collect();
    indexes.extend(constraints.iter().map(|k| bind::Index {
        table_id: k.object_id as u64,
        id: CONSTRAINT_IDS + k.tag as u64,
        name: k.name.clone(),
        clustered: false,
        constraint_backed: false,
        backend_name: ObjectName::from(vec![Ident::new(format!("__msduck_constraint_{}", k.tag))]),
    }));
    let sql_error = |d: bind::Diagnostic| error((d.number, d.state, d.class, d.message));
    for target in &request.targets {
        let single = |option: OptionClause| Request {
            if_exists: request.if_exists,
            targets: vec![bind::Target {
                option,
                ..target.clone()
            }],
        };
        let plan = bind::bind(
            &single(OptionClause::None),
            &tables,
            &indexes,
            &["dbo"],
            str::eq_ignore_ascii_case,
        )
        .map_err(|e| match e {
            bind::Error::Unsupported(message) => anyhow::anyhow!(message),
            bind::Error::Sql(d) => sql_error(d),
        })?;
        if let Some(terminal) = plan.terminal {
            return Err(sql_error(terminal));
        }
        let Some(drop) = plan.drops.first() else {
            continue;
        };
        let path = target
            .table
            .0
            .iter()
            .map(|part| match part {
                sqlparser::ast::ObjectNamePart::Identifier(ident) => ident.value.clone(),
                other => other.to_string(),
            })
            .chain(std::iter::once(target.index.value.clone()))
            .collect::<Vec<_>>()
            .join(".");
        if drop.index_id >= CONSTRAINT_IDS {
            let key = constraints
                .iter()
                .find(|k| CONSTRAINT_IDS + k.tag as u64 == drop.index_id)
                .expect("bound constraint");
            return Err(error((
                3723,
                4,
                16,
                format!(
                    "An explicit DROP INDEX is not allowed on index '{path}'. It is being used for {} constraint enforcement.",
                    if key.kind == "PK" {
                        "PRIMARY KEY"
                    } else {
                        "UNIQUE KEY"
                    }
                ),
            )));
        }
        let index = registered
            .iter()
            .find(|i| i.object_id as u64 == drop.table_id && i.index_id as u64 == drop.index_id)
            .ok_or_else(|| {
                anyhow::anyhow!("Bound index identity is absent from its catalog snapshot")
            })?;
        let key = keys
            .iter()
            .find(|k| k.incarnation == Some(index.incarnation));
        // MAXDOP applies only to clustered indexes; the binder reports 3748
        // for a nonclustered one.
        if target.option == OptionClause::MaxdopOne && !key.is_some_and(|k| k.clustered) {
            let plan = bind::bind(
                &single(OptionClause::MaxdopOne),
                &tables,
                &indexes,
                &["dbo"],
                str::eq_ignore_ascii_case,
            )
            .map_err(|e| match e {
                bind::Error::Unsupported(message) => anyhow::anyhow!(message),
                bind::Error::Sql(d) => sql_error(d),
            })?;
            if let Some(terminal) = plan.terminal {
                return Err(sql_error(terminal));
            }
        }
        let tag = key.map(|k| k.tag);
        let remove = |db: &duckdb::Connection| -> Result<()> {
            crate::index_catalog::drop_index(
                db,
                index,
                crate::index_catalog::Transaction::CallerOwned,
            )?;
            if let Some(tag) = tag {
                catalog::remove(db, tag)?;
            }
            Ok(())
        };
        if session.transactions == 0 {
            // The physical index, its registration and its key record go
            // together.
            db.execute_batch("BEGIN TRANSACTION")?;
            match remove(db) {
                Ok(()) => db.execute_batch("COMMIT")?,
                Err(error) => {
                    let _ = db.execute_batch("ROLLBACK");
                    return Err(error);
                }
            }
        } else {
            remove(db)?;
        }
        indexes.retain(|i| !(i.table_id == drop.table_id && i.id == drop.index_id));
    }
    Ok(Execution::statement(vec![], None, 201))
}
