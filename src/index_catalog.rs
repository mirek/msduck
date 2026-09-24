//! Transactional table-owned logical index identities, separate from backend names.
use anyhow::{Result, bail, ensure};
use duckdb::{Connection, params};
use sqlparser::ast::{CreateIndex, Expr, Ident, ObjectName, ObjectNamePart, OrderBySort};

#[derive(Clone, Copy, Debug)]
pub enum Transaction {
    /// Caller promises there is no open transaction; this operation owns one.
    Owned,
    /// The engine owns an active transaction and must handle any error/rollback.
    CallerOwned,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Index {
    pub object_id: i32,
    pub index_id: i32,
    pub name: String,
    pub unique: bool,
    pub incarnation: i64,
    pub backend_schema: String,
    pub backend_name: String,
}
fn quote(value: &str) -> String {
    Ident::with_quote('"', value).to_string()
}
fn parts(name: &ObjectName) -> Result<Vec<&str>> {
    name.0
        .iter()
        .map(|p| match p {
            ObjectNamePart::Identifier(id) => Ok(id.value.as_str()),
            _ => bail!("unsupported index name expression"),
        })
        .collect()
}
fn transaction<T>(
    db: &Connection,
    owner: Transaction,
    work: impl FnOnce() -> Result<T>,
) -> Result<T> {
    if matches!(owner, Transaction::CallerOwned) {
        return work();
    }
    db.execute_batch("BEGIN TRANSACTION")?;
    match work() {
        Ok(value) => match db.execute_batch("COMMIT") {
            Ok(()) => Ok(value),
            Err(error) => {
                let _ = db.execute_batch("ROLLBACK");
                Err(error.into())
            }
        },
        Err(error) => {
            db.execute_batch("ROLLBACK")?;
            Err(error)
        }
    }
}

/// Called after the object/column catalogs exist. Public sys.indexes views are
/// deliberately not installed until their full supported metadata is acquired.
pub fn register(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE SEQUENCE IF NOT EXISTS main.__msduck_index_incarnations START 1 NO CYCLE;
        CREATE TABLE IF NOT EXISTS main.__msduck_index_catalog(
          object_id INTEGER NOT NULL,index_id INTEGER NOT NULL,name VARCHAR NOT NULL,name_key VARCHAR NOT NULL,
          is_unique BOOLEAN NOT NULL,incarnation BIGINT NOT NULL UNIQUE,
          backend_schema VARCHAR NOT NULL,backend_name VARCHAR NOT NULL,table_oid BIGINT NOT NULL,
          PRIMARY KEY(object_id,index_id),UNIQUE(object_id,name_key),UNIQUE(backend_schema,backend_name));
        CREATE TABLE IF NOT EXISTS main.__msduck_index_keys(
          incarnation BIGINT NOT NULL,ordinal INTEGER NOT NULL,column_id INTEGER NOT NULL,
          column_name VARCHAR NOT NULL,PRIMARY KEY(incarnation,ordinal))")?;
    Ok(())
}

/// Remove stale logical records after transactional object synchronization. A
/// replacement table with the same name cannot inherit an older table's index.
pub fn sync(db: &Connection) -> Result<()> {
    db.execute_batch("DELETE FROM main.__msduck_index_catalog c WHERE NOT EXISTS(
        SELECT 1 FROM sys.objects o JOIN sys.schemas s USING(schema_id)
        JOIN duckdb_tables() t ON t.schema_name=s.name AND t.table_name=o.name
        JOIN duckdb_indexes() i ON i.schema_name=c.backend_schema AND i.index_name=c.backend_name AND i.table_oid=t.table_oid
        WHERE o.object_id=c.object_id AND o.type='U' AND t.table_oid=c.table_oid);
        DELETE FROM main.__msduck_index_keys k WHERE NOT EXISTS(SELECT 1 FROM main.__msduck_index_catalog c WHERE c.incarnation=k.incarnation)")?;
    Ok(())
}

/// Read only identities that still belong to the same live physical table/index.
pub fn acquire(db: &Connection) -> Result<Vec<Index>> {
    Ok(db.prepare("SELECT c.object_id,c.index_id,c.name,c.is_unique,c.incarnation,c.backend_schema,c.backend_name
        FROM main.__msduck_index_catalog c JOIN sys.objects o ON o.object_id=c.object_id AND o.type='U'
        JOIN sys.schemas s ON s.schema_id=o.schema_id
        JOIN duckdb_tables() t ON t.schema_name=s.name AND t.table_name=o.name AND t.table_oid=c.table_oid
        JOIN duckdb_indexes() i ON i.schema_name=c.backend_schema AND i.index_name=c.backend_name AND i.table_oid=t.table_oid
        ORDER BY c.object_id,c.index_id")?.query_map([], |r| Ok(Index {
            object_id:r.get(0)?,index_id:r.get(1)?,name:r.get(2)?,unique:r.get(3)?,incarnation:r.get(4)?,backend_schema:r.get(5)?,backend_name:r.get(6)?,
        }))?.collect::<duckdb::Result<Vec<_>>>()?)
}

/// Initially supports ordinary/unique ascending column indexes only. Other AST
/// options fail before mutation rather than being silently discarded.
pub fn create(
    db: &Connection,
    object_id: i32,
    ast: &CreateIndex,
    owner: Transaction,
) -> Result<Index> {
    ensure!(
        ast.using.is_none()
            && !ast.concurrently
            && !ast.r#async
            && !ast.if_not_exists
            && ast.include.is_empty()
            && ast.nulls_distinct.is_none()
            && ast.with.is_empty()
            && ast.predicate.is_none()
            && ast.index_options.is_empty()
            && ast.alter_options.is_empty(),
        "unsupported index kind or options"
    );
    let index_name = parts(
        ast.name
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("index name required"))?,
    )?;
    ensure!(
        index_name.len() == 1 && !index_name[0].is_empty(),
        "index name must be one identifier"
    );
    ensure!(!ast.columns.is_empty(), "index key columns required");
    let mut keys = vec![];
    for key in &ast.columns {
        ensure!(
            key.operator_class.is_none()
                && matches!(key.column.options.sort, None | Some(OrderBySort::Asc))
                && key.column.options.nulls_first.is_none()
                && key.column.with_fill.is_none(),
            "unsupported index key options"
        );
        let Expr::Identifier(id) = &key.column.expr else {
            bail!("index expressions are unsupported")
        };
        keys.push(id.value.clone());
    }
    transaction(db, owner, || {
        sync(db)?;
        let (schema,table,table_oid):(String,String,i64)=db.query_row(
            "SELECT s.name,o.name,CAST(t.table_oid AS BIGINT) FROM sys.objects o JOIN sys.schemas s USING(schema_id)
             JOIN duckdb_tables() t ON t.schema_name=s.name AND t.table_name=o.name WHERE o.object_id=? AND o.type='U'",
            [object_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        let target = parts(&ast.table_name)?;
        ensure!(
            match target.as_slice() {
                [t] => t.eq_ignore_ascii_case(&table),
                [s, t] => s.eq_ignore_ascii_case(&schema) && t.eq_ignore_ascii_case(&table),
                _ => false,
            },
            "resolved table identity does not match CREATE INDEX target"
        );
        let duplicate:i64=db.query_row("SELECT count(*) FROM main.__msduck_index_catalog WHERE object_id=? AND name_key=lower(?)",params![object_id,index_name[0]],|r|r.get(0))?;
        ensure!(
            duplicate == 0,
            "index name already exists on the target table"
        );
        let mut columns = vec![];
        for name in &keys {
            let (id, declared, kind): (i32, String, i32) = db.query_row(
                "SELECT column_id,name,CAST(system_type_id AS INTEGER) FROM sys.columns WHERE object_id=? AND lower(name)=lower(?)",
                params![object_id, name],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            ensure!(
                !ast.unique || [48, 52, 56, 127].contains(&kind),
                "unique index comparison is currently implemented only for integer keys"
            );
            ensure!(
                !columns.iter().any(|(previous, _)| *previous == id),
                "repeated index key column"
            );
            columns.push((id, declared));
        }
        let used = acquire(db)?
            .into_iter()
            .filter(|i| i.object_id == object_id)
            .map(|i| i.index_id)
            .collect::<std::collections::HashSet<_>>();
        let index_id = (2..i32::MAX)
            .find(|n| !used.contains(n))
            .ok_or_else(|| anyhow::anyhow!("index ID space exhausted"))?;
        let incarnation: i64 = db.query_row(
            "SELECT nextval('main.__msduck_index_incarnations')",
            [],
            |r| r.get(0),
        )?;
        let backend_name = format!("__msduck_index_{incarnation}");
        let sql = format!(
            "CREATE {}INDEX {} ON {}.{} ({})",
            if ast.unique { "UNIQUE " } else { "" },
            quote(&backend_name),
            quote(&schema),
            quote(&table),
            columns
                .iter()
                .map(|(_, n)| {
                    let name = quote(n);
                    // SQL Server treats NULL as one comparable index key. The
                    // discriminator keeps NULL distinct from the zero sentinel.
                    if ast.unique {
                        format!("({name} IS NULL),(coalesce({name},0))")
                    } else {
                        name
                    }
                })
                .collect::<Vec<_>>()
                .join(",")
        );
        db.execute_batch(&sql)?;
        db.execute(
            "INSERT INTO main.__msduck_index_catalog VALUES(?,?,?,lower(?),?,?,?,?,?)",
            params![
                object_id,
                index_id,
                index_name[0],
                index_name[0],
                ast.unique,
                incarnation,
                schema,
                backend_name,
                table_oid
            ],
        )?;
        for (ordinal, (id, name)) in columns.iter().enumerate() {
            db.execute(
                "INSERT INTO main.__msduck_index_keys VALUES(?,?,?,?)",
                params![incarnation, ordinal as i32 + 1, id, name],
            )?;
        }
        Ok(Index {
            object_id,
            index_id,
            name: index_name[0].into(),
            unique: ast.unique,
            incarnation,
            backend_schema: schema,
            backend_name,
        })
    })
}

/// A bound identity is an incarnation, not just a reusable logical index ID.
pub fn drop_index(db: &Connection, expected: &Index, owner: Transaction) -> Result<()> {
    transaction(db, owner, || {
        let current = acquire(db)?
            .into_iter()
            .find(|i| i.object_id == expected.object_id && i.index_id == expected.index_id);
        ensure!(
            current.as_ref() == Some(expected),
            "stale or missing bound index incarnation"
        );
        db.execute_batch(&format!(
            "DROP INDEX {}.{}",
            quote(&expected.backend_schema),
            quote(&expected.backend_name)
        ))?;
        db.execute(
            "DELETE FROM main.__msduck_index_keys WHERE incarnation=?",
            [expected.incarnation],
        )?;
        db.execute(
            "DELETE FROM main.__msduck_index_catalog WHERE incarnation=?",
            [expected.incarnation],
        )?;
        Ok(())
    })
}
