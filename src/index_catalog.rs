//! Transactional table-owned logical index identities, separate from backend names,
//! and the sys.indexes and sys.index_columns views over them and over the keys
//! feature's PRIMARY KEY and UNIQUE constraints (`main.__msduck_keys`).
//!
//! Every query of DuckDB's physical catalog (`duckdb_tables()`,
//! `duckdb_indexes()`, `duckdb_constraints()`) is limited to the current
//! database, which owns `main` and `sys`; same-named tables of other
//! databases are not this database's.
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

/// Called after the object/column catalogs exist. The public views are
/// installed by [`publish_views`].
pub fn register(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE SEQUENCE IF NOT EXISTS main.__msduck_index_incarnations START 1 NO CYCLE;
        CREATE TABLE IF NOT EXISTS main.__msduck_index_catalog(
          object_id INTEGER NOT NULL,index_id INTEGER NOT NULL,name VARCHAR NOT NULL,name_key VARCHAR NOT NULL,
          is_unique BOOLEAN NOT NULL,incarnation BIGINT NOT NULL UNIQUE,
          backend_schema VARCHAR NOT NULL,backend_name VARCHAR NOT NULL,table_oid BIGINT NOT NULL,
          PRIMARY KEY(object_id,index_id),UNIQUE(object_id,name_key),UNIQUE(backend_schema,backend_name));
        CREATE TABLE IF NOT EXISTS main.__msduck_index_keys(
          incarnation BIGINT NOT NULL,ordinal INTEGER NOT NULL,column_id INTEGER NOT NULL,
          column_name VARCHAR NOT NULL,PRIMARY KEY(incarnation,ordinal));
        -- Descending key columns (1-based ordinals) of registered indexes.
        -- Storage ignores key order; sys.index_columns reports it.
        CREATE TABLE IF NOT EXISTS main.__msduck_index_descending(
          incarnation BIGINT NOT NULL,ordinal INTEGER NOT NULL,PRIMARY KEY(incarnation,ordinal))")?;
    // Functions are instance-wide; every database of an instance shares one.
    let registered: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM duckdb_functions() WHERE function_name=?)",
        [FILTER_FUNCTION],
        |r| r.get(0),
    )?;
    if !registered {
        db.register_scalar_function::<FilterDefinition>(FILTER_FUNCTION)?;
    }
    Ok(())
}

/// Remove stale logical records after transactional object synchronization. A
/// replacement table with the same name cannot inherit an older table's index.
/// The caller must synchronize logical objects after every DDL mutation. Native
/// DuckDB OIDs are used only for current ownership joins; they change on reopen.
/// Physical tables and indexes are those of the current database only: another
/// database can hold a table, and a backend index, of the same name.
pub fn sync(db: &Connection) -> Result<()> {
    db.execute_batch("DELETE FROM main.__msduck_index_catalog c WHERE NOT EXISTS(
        SELECT 1 FROM sys.objects o JOIN sys.schemas s USING(schema_id)
        JOIN duckdb_tables() t ON t.database_name=current_database() AND t.schema_name=s.name AND t.table_name=o.name
        JOIN duckdb_indexes() i ON i.database_name=current_database() AND i.schema_name=c.backend_schema AND i.index_name=c.backend_name AND i.table_oid=t.table_oid
        WHERE o.object_id=c.object_id AND rtrim(o.type)='U');
        DELETE FROM main.__msduck_index_keys k WHERE NOT EXISTS(SELECT 1 FROM main.__msduck_index_catalog c WHERE c.incarnation=k.incarnation)")?;
    publish_key_indexes(db)?;
    // The keys feature rebuilds an altered table's indexes under their old
    // incarnations, so its record keeps their key order.
    db.execute_batch("DELETE FROM main.__msduck_index_descending d
        WHERE NOT EXISTS(SELECT 1 FROM main.__msduck_index_catalog c WHERE c.incarnation=d.incarnation)
          AND NOT EXISTS(SELECT 1 FROM main.__msduck_key_indexes k WHERE k.incarnation=d.incarnation)")?;
    Ok(())
}

/// Read only identities that still belong to the same live physical table/index.
pub fn acquire(db: &Connection) -> Result<Vec<Index>> {
    Ok(db.prepare("SELECT c.object_id,c.index_id,c.name,c.is_unique,c.incarnation,c.backend_schema,c.backend_name
        FROM main.__msduck_index_catalog c JOIN sys.objects o ON o.object_id=c.object_id AND rtrim(o.type)='U'
        JOIN sys.schemas s ON s.schema_id=o.schema_id
        JOIN duckdb_tables() t ON t.database_name=current_database() AND t.schema_name=s.name AND t.table_name=o.name
        JOIN duckdb_indexes() i ON i.database_name=current_database() AND i.schema_name=c.backend_schema AND i.index_name=c.backend_name AND i.table_oid=t.table_oid
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
             JOIN duckdb_tables() t ON t.database_name=current_database() AND t.schema_name=s.name AND t.table_name=o.name WHERE o.object_id=? AND rtrim(o.type)='U'",
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
        if duplicate != 0 {
            return Err(msduck_core::diagnostic::SqlError::new(
                1913,
                1,
                format!(
                    "The operation failed because an index or statistics with name '{}' already exists on table '{}'.",
                    index_name[0], target.join(".")
                ),
            ).into());
        }
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
            "DELETE FROM main.__msduck_index_descending WHERE incarnation=?",
            [expected.incarnation],
        )?;
        db.execute(
            "DELETE FROM main.__msduck_index_catalog WHERE incarnation=?",
            [expected.incarnation],
        )?;
        Ok(())
    })
}

/// Migrate ordinary unmanaged indexes into explicit logical/backend identities.
/// The complete migration is atomic. Rebuild unique integer indexes with SQL
/// Server NULL comparison; incompatible existing data aborts the migration.
pub fn reconcile(db: &Connection, owner: Transaction) -> Result<()> {
    transaction(db, owner, || {
        sync(db)?;
        let records=db.prepare("SELECT o.object_id,i.schema_name,i.index_name,i.sql FROM duckdb_indexes() i
            JOIN sys.schemas s ON s.name=i.schema_name
            JOIN sys.objects o ON o.schema_id=s.schema_id AND o.name=i.table_name AND rtrim(o.type)='U'
            WHERE i.database_name=current_database() AND NOT EXISTS(SELECT 1 FROM main.__msduck_index_catalog c WHERE c.backend_schema=i.schema_name AND c.backend_name=i.index_name)
            ORDER BY o.object_id,i.index_oid")?.query_map([],|r|Ok((r.get::<_,i32>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?.collect::<duckdb::Result<Vec<_>>>()?;
        for (object_id, schema, name, sql) in records {
            ensure!(
                !name.starts_with("__msduck_index_"),
                "unregistered private backend index requires explicit recovery"
            );
            let mut parsed =
                sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::GenericDialect {}, &sql)?;
            ensure!(
                parsed.len() == 1,
                "unmanaged index has an unexpected definition"
            );
            let sqlparser::ast::Statement::CreateIndex(ast) = parsed.remove(0) else {
                bail!("unmanaged index definition is not CREATE INDEX")
            };
            let declared = parts(
                ast.name
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("unnamed unmanaged index"))?,
            )?;
            ensure!(
                declared == vec![name.as_str()],
                "unmanaged index name differs from its definition"
            );
            create(db, object_id, &ast, Transaction::CallerOwned)?;
            db.execute_batch(&format!("DROP INDEX {}.{}", quote(&schema), quote(&name)))?;
        }
        Ok(())
    })
}

/// Only this entry point promises a complete ordinary-index snapshot. Until
/// constraint indexes are modeled, their presence is an explicit error.
pub fn acquire_complete(db: &Connection) -> Result<Vec<Index>> {
    let unknown:i64=db.query_row("SELECT count(*) FROM duckdb_indexes() i
        JOIN sys.schemas s ON s.name=i.schema_name
        JOIN sys.objects o ON o.schema_id=s.schema_id AND o.name=i.table_name AND rtrim(o.type)='U'
        WHERE i.database_name=current_database() AND NOT EXISTS(SELECT 1 FROM main.__msduck_index_catalog c WHERE c.backend_schema=i.schema_name AND c.backend_name=i.index_name AND c.object_id=o.object_id)",[],|r|r.get(0))?;
    ensure!(
        unknown == 0,
        "unmanaged index catalog is incomplete; reconcile before binding"
    );
    let constraints: i64 = db.query_row(
        "SELECT count(*) FROM duckdb_constraints() c
        JOIN sys.schemas s ON s.name=c.schema_name
        JOIN sys.objects o ON o.schema_id=s.schema_id AND o.name=c.table_name AND rtrim(o.type)='U'
        WHERE c.database_name=current_database() AND c.constraint_type IN ('PRIMARY KEY','UNIQUE')",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        constraints == 0,
        "constraint-backed index catalog is not yet supported"
    );
    acquire(db)
}

/// The keys feature's record (`main.__msduck_keys`) as this catalog reads it:
/// one row per PRIMARY KEY or UNIQUE constraint (`PK`, `UQ`) and per index the
/// keys feature created (`IX`), with column lists decoded from JSON.
const KEY_INDEXES: &str = r#"CREATE OR REPLACE MACRO main.__msduck_index_names(list) AS
      list_transform(regexp_extract_all(list,'"((?:[^"\\]|\\.)*)"',1),
        lambda x: replace(replace(x,'\"','"'),'\\','\'));
    CREATE OR REPLACE VIEW main.__msduck_key_indexes AS
    SELECT tag,object_id,name,kind,is_unique,is_clustered,is_native,backend_name,incarnation,
      main.__msduck_index_names(key_columns) AS key_columns,
      main.__msduck_index_names(included_columns) AS included_columns,filter_definition
    FROM main.__msduck_keys"#;

/// The same columns while the keys feature has not bootstrapped this
/// database yet: [`publish_views`] runs before feature bootstraps, and a view
/// cannot name a missing table. [`sync`] replaces it once the table exists.
const NO_KEY_INDEXES: &str = "CREATE OR REPLACE VIEW main.__msduck_key_indexes AS
    SELECT CAST(NULL AS BIGINT) AS tag,CAST(NULL AS INTEGER) AS object_id,CAST(NULL AS VARCHAR) AS name,
      CAST(NULL AS VARCHAR) AS kind,CAST(NULL AS BOOLEAN) AS is_unique,CAST(NULL AS BOOLEAN) AS is_clustered,
      CAST(NULL AS BOOLEAN) AS is_native,CAST(NULL AS VARCHAR) AS backend_name,CAST(NULL AS BIGINT) AS incarnation,
      CAST(NULL AS VARCHAR[]) AS key_columns,CAST(NULL AS VARCHAR[]) AS included_columns,
      CAST(NULL AS VARCHAR) AS filter_definition
    WHERE false";

/// Whether the keys record exists, and whether `main.__msduck_key_indexes`
/// reads it (`None` while the view is missing).
fn key_indexes_state(db: &Connection) -> Result<(bool, Option<bool>)> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM duckdb_tables() WHERE database_name=current_database()
           AND schema_name='main' AND table_name='__msduck_keys'),
         (SELECT contains(sql,'__msduck_keys') FROM duckdb_views() WHERE database_name=current_database()
           AND schema_name='main' AND view_name='__msduck_key_indexes')",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

/// Point `main.__msduck_key_indexes` at the keys record once it exists. Only
/// reads the catalog unless the view must change.
///
/// A missing view is created in the caller's transaction (bootstrap). An
/// outdated one is replaced on a separate connection in a transaction of its
/// own, so concurrent first DDL statements of a new database cannot fail on
/// a write-write conflict over the view; a conflicting replacement is left
/// to the session that made it, and a later call retries a failed one.
fn publish_key_indexes(db: &Connection) -> Result<()> {
    let (exists, view) = key_indexes_state(db)?;
    let definition = |exists: bool| if exists { KEY_INDEXES } else { NO_KEY_INDEXES };
    match view {
        None => db.execute_batch(definition(exists))?,
        Some(current) if current == exists => {}
        Some(_) => {
            let database: String = db.query_row("SELECT current_database()", [], |r| r.get(0))?;
            let replace = || -> Result<()> {
                let other = db.try_clone()?;
                other.execute_batch(&format!("USE {}", quote(&database)))?;
                let (exists, view) = key_indexes_state(&other)?;
                if view != Some(exists) {
                    other.execute_batch(definition(exists))?;
                }
                Ok(())
            };
            let _ = replace();
        }
    }
    Ok(())
}

/// Publish sys.indexes and sys.index_columns.
///
/// Rows come from the table-owned catalog (ordinary and keys-managed indexes)
/// and from the keys feature's PRIMARY KEY and UNIQUE constraints. Index IDs
/// follow SQL Server: 1 for the clustered index, 0 for the heap of a table
/// without one, and 2 upwards for nonclustered indexes, constraints first.
/// SQL Server creates the constraints of one statement in reverse
/// declaration order; constraints recorded with consecutive tags count as one
/// statement. The catalog's stored index IDs (2 upwards per table, gaps
/// reused) order the registered indexes. A PRIMARY KEY is clustered unless the table has a
/// clustered index; UNIQUE constraints are nonclustered. These are SQL
/// Server's defaults: an explicit CLUSTERED or NONCLUSTERED on a constraint
/// is not recorded. A physical index or native key constraint that neither
/// catalog records fails explicitly rather than being omitted.
pub fn publish_views(db: &Connection) -> Result<()> {
    publish_key_indexes(db)?;
    db.execute_batch("CREATE OR REPLACE MACRO main.__msduck_index_catalog_ready() AS
        CASE WHEN EXISTS(SELECT 1 FROM duckdb_indexes() i
          JOIN sys.schemas s ON s.name=i.schema_name
          JOIN sys.objects o ON o.schema_id=s.schema_id AND o.name=i.table_name AND rtrim(o.type)='U'
          WHERE i.database_name=current_database()
            AND NOT EXISTS(SELECT 1 FROM main.__msduck_index_catalog c WHERE c.backend_schema=i.schema_name AND c.backend_name=i.index_name AND c.object_id=o.object_id)
            AND NOT EXISTS(SELECT 1 FROM main.__msduck_key_indexes k WHERE k.backend_name=i.index_name AND k.object_id=o.object_id))
        OR EXISTS(SELECT 1 FROM duckdb_constraints() c
          JOIN sys.schemas s ON s.name=c.schema_name
          JOIN sys.objects o ON o.schema_id=s.schema_id AND o.name=c.table_name AND rtrim(o.type)='U'
          WHERE c.database_name=current_database() AND c.constraint_type IN ('PRIMARY KEY','UNIQUE')
            AND NOT EXISTS(SELECT 1 FROM main.__msduck_key_indexes k WHERE k.object_id=o.object_id
              AND k.kind=CASE c.constraint_type WHEN 'PRIMARY KEY' THEN 'PK' ELSE 'UQ' END
              AND list_sort(list_transform(k.key_columns,lambda x: lower(x)))
                =list_sort(list_transform(c.constraint_column_names,lambda x: lower(x)))))
        THEN error('Index catalog contains unsupported unmanaged or constraint-backed indexes') ELSE true END;
        CREATE OR REPLACE VIEW main.__msduck_live_indexes AS
        SELECT c.* FROM main.__msduck_index_catalog c
          JOIN sys.objects o ON o.object_id=c.object_id AND rtrim(o.type)='U'
          JOIN sys.schemas s ON s.schema_id=o.schema_id
          JOIN duckdb_tables() t ON t.database_name=current_database() AND t.schema_name=s.name AND t.table_name=o.name
          JOIN duckdb_indexes() i ON i.database_name=current_database() AND i.schema_name=c.backend_schema AND i.index_name=c.backend_name AND i.table_oid=t.table_oid;
        CREATE OR REPLACE VIEW main.__msduck_index_entries AS
        WITH ix AS (
          SELECT l.object_id,l.index_id AS stored_id,l.name,l.is_unique,l.incarnation,
            coalesce(k.is_clustered,false) AS clustered,
            coalesce(k.included_columns,CAST([] AS VARCHAR[])) AS included,k.filter_definition AS filter
          FROM main.__msduck_live_indexes l
          LEFT JOIN main.__msduck_key_indexes k ON k.kind='IX' AND k.incarnation=l.incarnation AND k.object_id=l.object_id),
        cx AS (SELECT object_id,min(stored_id) AS stored_id FROM ix WHERE clustered GROUP BY object_id),
        kc AS (
          SELECT k.tag,k.object_id,k.name,k.kind,k.key_columns,
            k.kind='PK' AND NOT EXISTS(SELECT 1 FROM cx WHERE cx.object_id=k.object_id) AS clustered,
            k.tag-row_number() OVER (PARTITION BY k.object_id ORDER BY k.tag) AS run
          FROM main.__msduck_key_indexes k JOIN sys.objects o ON o.object_id=k.object_id AND rtrim(o.type)='U'
          WHERE k.kind IN ('PK','UQ')),
        nc AS (SELECT object_id,count(*) AS n FROM kc WHERE NOT clustered GROUP BY object_id)
        SELECT kc.object_id,
          CAST(CASE WHEN kc.clustered THEN 1
            ELSE 1+row_number() OVER (PARTITION BY kc.object_id,kc.clustered ORDER BY kc.run,kc.tag DESC) END AS INTEGER) AS index_id,
          kc.name,kc.clustered,true AS is_unique,kc.kind='PK' AS is_primary_key,kc.kind='UQ' AS is_unique_constraint,
          CAST(NULL AS VARCHAR) AS filter,kc.key_columns,CAST(NULL AS BIGINT) AS incarnation,
          CAST([] AS VARCHAR[]) AS included
        FROM kc
        UNION ALL
        SELECT ix.object_id,
          CAST(CASE WHEN ix.clustered THEN 1
            ELSE ix.stored_id+coalesce(nc.n,0)-CASE WHEN cx.stored_id<ix.stored_id THEN 1 ELSE 0 END END AS INTEGER),
          ix.name,ix.clustered,ix.is_unique,false,false,ix.filter,CAST(NULL AS VARCHAR[]),ix.incarnation,ix.included
        FROM ix LEFT JOIN nc ON nc.object_id=ix.object_id LEFT JOIN cx ON cx.object_id=ix.object_id;
        CREATE OR REPLACE VIEW sys.indexes AS
        SELECT r.object_id,r.name,r.index_id,CAST(r.type AS UTINYINT) AS type,
          CAST(CASE r.type WHEN 0 THEN 'HEAP' WHEN 1 THEN 'CLUSTERED' ELSE 'NONCLUSTERED' END AS VARCHAR) AS type_desc,
          r.is_unique,CAST(1 AS INTEGER) AS data_space_id,false AS ignore_dup_key,
          r.is_primary_key,r.is_unique_constraint,CAST(0 AS UTINYINT) AS fill_factor,
          false AS is_padded,false AS is_disabled,false AS is_hypothetical,false AS is_ignored_in_optimization,
          true AS allow_row_locks,true AS allow_page_locks,r.filter IS NOT NULL AS has_filter,
          CAST(__msduck_index_filter_definition(r.filter) AS VARCHAR) AS filter_definition,
          CAST(NULL AS INTEGER) AS compression_delay,
          false AS suppress_dup_key_messages,false AS auto_created,false AS optimize_for_sequential_key
        FROM (SELECT o.object_id,CAST(NULL AS VARCHAR) AS name,CAST(0 AS INTEGER) AS index_id,0 AS type,
                false AS is_unique,false AS is_primary_key,false AS is_unique_constraint,CAST(NULL AS VARCHAR) AS filter
              FROM sys.objects o WHERE rtrim(o.type)='U'
                AND NOT EXISTS(SELECT 1 FROM main.__msduck_index_entries e WHERE e.object_id=o.object_id AND e.clustered)
              UNION ALL
              SELECT object_id,name,index_id,CASE WHEN clustered THEN 1 ELSE 2 END,
                is_unique,is_primary_key,is_unique_constraint,filter
              FROM main.__msduck_index_entries) r
        WHERE main.__msduck_index_catalog_ready();
        CREATE OR REPLACE VIEW sys.index_columns AS
        WITH members AS (
          SELECT e.object_id,e.index_id,k.column_id,k.ordinal AS key_ordinal,k.ordinal AS position,
            EXISTS(SELECT 1 FROM main.__msduck_index_descending d
              WHERE d.incarnation=k.incarnation AND d.ordinal=k.ordinal) AS descending,false AS included
          FROM main.__msduck_index_entries e JOIN main.__msduck_index_keys k ON k.incarnation=e.incarnation
          UNION ALL
          SELECT e.object_id,e.index_id,c.column_id,n.ordinal,n.ordinal,false,false
          FROM main.__msduck_index_entries e
          CROSS JOIN LATERAL (SELECT unnest(e.key_columns) AS name,unnest(generate_series(1,len(e.key_columns))) AS ordinal) n
          JOIN sys.columns c ON c.object_id=e.object_id AND lower(c.name)=lower(n.name)
          UNION ALL
          SELECT e.object_id,e.index_id,c.column_id,0,1000000+n.ordinal,false,true
          FROM main.__msduck_index_entries e
          CROSS JOIN LATERAL (SELECT unnest(e.included) AS name,unnest(generate_series(1,len(e.included))) AS ordinal) n
          JOIN sys.columns c ON c.object_id=e.object_id AND lower(c.name)=lower(n.name))
        SELECT object_id,index_id,
          CAST(row_number() OVER (PARTITION BY object_id,index_id
            ORDER BY CASE WHEN index_id=1 THEN column_id ELSE position END) AS INTEGER) AS index_column_id,
          column_id,CAST(key_ordinal AS UTINYINT) AS key_ordinal,CAST(0 AS UTINYINT) AS partition_ordinal,
          descending AS is_descending_key,included AS is_included_column,
          CAST(0 AS UTINYINT) AS column_store_order_ordinal,CAST(0 AS UTINYINT) AS data_clustering_ordinal
        FROM members
        WHERE main.__msduck_index_catalog_ready()")?;
    Ok(())
}

const FILTER_FUNCTION: &str = "__msduck_index_filter_definition";

/// `filter_definition` of sys.indexes from a filter's recorded text.
struct FilterDefinition;
impl duckdb::vscalar::VScalar for FilterDefinition {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut duckdb::core::DataChunkHandle,
        output: &mut dyn duckdb::vtab::arrow::WritableVector,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        use duckdb::core::Inserter;
        let len = input.len();
        let source = input.flat_vector(0);
        let values = unsafe { source.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(len) };
        let mut result = output.flat_vector();
        for (row, value) in values.iter().enumerate() {
            if source.row_is_null(row as u64) {
                result.set_null(row);
                continue;
            }
            let mut value = *value;
            let text = duckdb::types::DuckString::new(&mut value).as_str();
            result.insert(row, filter_definition(&text).as_str());
        }
        Ok(())
    }
    fn signatures() -> Vec<duckdb::vscalar::ScalarFunctionSignature> {
        use duckdb::core::LogicalTypeId;
        vec![duckdb::vscalar::ScalarFunctionSignature::exact(
            vec![LogicalTypeId::Varchar.into()],
            LogicalTypeId::Varchar.into(),
        )]
    }
}

/// SQL Server's normalized text of a filtered index predicate, such as
/// `([w] IS NOT NULL AND [w]>(5))`: bracketed column names, numbers in
/// parentheses, no spaces around comparison operators, `!=` as `<>`, and an
/// IN list in its own parentheses when it is one of several conjuncts. Text
/// outside SQL Server's filter grammar is kept, parenthesized.
pub fn filter_definition(text: &str) -> String {
    use sqlparser::{dialect::MsSqlDialect, parser::Parser};
    fn conjuncts<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
        match expr {
            Expr::BinaryOp {
                left,
                op: sqlparser::ast::BinaryOperator::And,
                right,
            } => {
                conjuncts(left, out);
                conjuncts(right, out);
            }
            Expr::Nested(inner) => conjuncts(inner, out),
            _ => out.push(expr),
        }
    }
    fn column(expr: &Expr) -> Option<String> {
        match expr {
            Expr::Identifier(ident) => Some(format!("[{}]", ident.value.replace(']', "]]"))),
            Expr::CompoundIdentifier(parts) if parts.len() == 1 => {
                column(&Expr::Identifier(parts[0].clone()))
            }
            Expr::Nested(inner) => column(inner),
            _ => None,
        }
    }
    fn constant(expr: &Expr) -> Option<String> {
        use sqlparser::ast::{UnaryOperator, Value};
        match expr {
            Expr::Value(value) => Some(match &value.value {
                Value::Number(n, _) => format!("({n})"),
                Value::SingleQuotedString(s) => format!("'{}'", s.replace('\'', "''")),
                Value::NationalStringLiteral(s) => format!("N'{}'", s.replace('\'', "''")),
                Value::HexStringLiteral(s) => format!("0x{s}"),
                Value::Null => "NULL".into(),
                _ => return None,
            }),
            Expr::UnaryOp {
                op: UnaryOperator::Minus,
                expr,
            } => match expr.as_ref() {
                Expr::Value(value) => match &value.value {
                    Value::Number(n, _) => Some(format!("(-{n})")),
                    _ => None,
                },
                _ => None,
            },
            Expr::UnaryOp {
                op: UnaryOperator::Plus,
                expr,
            } => constant(expr),
            Expr::Nested(inner) => constant(inner),
            _ => None,
        }
    }
    fn term(expr: &Expr, several: bool) -> Option<String> {
        use sqlparser::ast::BinaryOperator as Op;
        match expr {
            Expr::BinaryOp { left, op, right } => {
                let op = match op {
                    Op::Eq => "=",
                    Op::NotEq => "<>",
                    Op::Lt => "<",
                    Op::Gt => ">",
                    Op::LtEq => "<=",
                    Op::GtEq => ">=",
                    _ => return None,
                };
                Some(format!("{}{op}{}", column(left)?, constant(right)?))
            }
            Expr::IsNull(inner) => Some(format!("{} IS NULL", column(inner)?)),
            Expr::IsNotNull(inner) => Some(format!("{} IS NOT NULL", column(inner)?)),
            Expr::InList {
                expr,
                list,
                negated: false,
            } => {
                let values = list.iter().map(constant).collect::<Option<Vec<_>>>()?;
                let text = format!("{} IN ({})", column(expr)?, values.join(", "));
                Some(if several { format!("({text})") } else { text })
            }
            _ => None,
        }
    }
    let render = || {
        let expr = Parser::new(&MsSqlDialect {})
            .try_with_sql(text)
            .ok()?
            .parse_expr()
            .ok()?;
        let mut terms = vec![];
        conjuncts(&expr, &mut terms);
        let several = terms.len() > 1;
        let terms = terms
            .into_iter()
            .map(|t| term(t, several))
            .collect::<Option<Vec<_>>>()?;
        Some(format!("({})", terms.join(" AND ")))
    };
    render().unwrap_or_else(|| format!("({text})"))
}

/// Logical declarations for the columns currently published by this adapter.
/// Catalog-default collation is explicit; type_desc uses SQL Server's captured
/// resource collation. The root query-catalog hook must consume these fields;
/// physical DuckDB types alone do not establish these descriptor properties.
pub fn fields(
    view: &str,
    catalog_collation: &str,
) -> Option<Vec<msduck_sql::binding_scope::Field>> {
    use msduck_core::{
        catalog::TypeMetadata,
        collation::Label,
        result::{Origin, Properties},
    };
    // (name, system type, user type, byte length, precision, nullable, computed)
    let definitions: Vec<(&str, u8, i32, i16, u8, bool, bool)> =
        match view.to_ascii_lowercase().as_str() {
            "indexes" => vec![
                ("object_id", 56, 56, 4, 10, false, false),
                ("name", 231, 256, 256, 0, true, false),
                ("index_id", 56, 56, 4, 10, false, false),
                ("type", 48, 48, 1, 3, false, false),
                ("type_desc", 231, 231, 120, 0, true, false),
                ("is_unique", 104, 104, 1, 1, true, true),
                ("data_space_id", 56, 56, 4, 10, true, true),
                ("ignore_dup_key", 104, 104, 1, 1, true, true),
                ("is_primary_key", 104, 104, 1, 1, true, true),
                ("is_unique_constraint", 104, 104, 1, 1, true, true),
                ("fill_factor", 48, 48, 1, 3, false, false),
                ("is_padded", 104, 104, 1, 1, true, true),
                ("is_disabled", 104, 104, 1, 1, true, true),
                ("is_hypothetical", 104, 104, 1, 1, true, true),
                ("is_ignored_in_optimization", 104, 104, 1, 1, true, true),
                ("allow_row_locks", 104, 104, 1, 1, true, true),
                ("allow_page_locks", 104, 104, 1, 1, true, true),
                ("has_filter", 104, 104, 1, 1, true, true),
                ("filter_definition", 231, 231, -1, 0, true, true),
                ("compression_delay", 56, 56, 4, 10, true, true),
                ("suppress_dup_key_messages", 104, 104, 1, 1, true, true),
                ("auto_created", 104, 104, 1, 1, true, true),
                ("optimize_for_sequential_key", 104, 104, 1, 1, true, true),
            ],
            "index_columns" => vec![
                ("object_id", 56, 56, 4, 10, false, false),
                ("index_id", 56, 56, 4, 10, false, false),
                ("index_column_id", 56, 56, 4, 10, false, false),
                ("column_id", 56, 56, 4, 10, false, false),
                ("key_ordinal", 48, 48, 1, 3, false, false),
                ("partition_ordinal", 48, 48, 1, 3, false, false),
                ("is_descending_key", 104, 104, 1, 1, true, true),
                ("is_included_column", 104, 104, 1, 1, true, true),
                ("column_store_order_ordinal", 48, 48, 1, 3, true, true),
                ("data_clustering_ordinal", 48, 48, 1, 3, true, true),
            ],
            _ => return None,
        };
    Some(
        definitions
            .into_iter()
            .map(
                |(name, system, user, length, precision, nullable, computed)| {
                    let collation_name = (system == 231).then(|| {
                        if name == "type_desc" {
                            "Latin1_General_CI_AS_KS_WS".to_owned()
                        } else {
                            catalog_collation.to_owned()
                        }
                    });
                    msduck_sql::binding_scope::Field {
                        name: name.into(),
                        info: Some(TypeMetadata {
                            system_type_id: Some(system),
                            user_type_id: Some(user),
                            max_length: Some(length),
                            precision: Some(precision),
                            scale: Some(0),
                            collation_name: collation_name.clone(),
                        }),
                        collation: collation_name.map(|name| Ok(Label::Implicit(name))),
                        json_fragment: false,
                        properties: Properties {
                            nullable: Some(nullable),
                            origin: if computed {
                                Origin::Expression
                            } else {
                                Origin::Stored
                            },
                        },
                    }
                },
            )
            .collect(),
    )
}
