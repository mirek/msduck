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
/// The caller must synchronize logical objects after every DDL mutation. Native
/// DuckDB OIDs are used only for current ownership joins; they change on reopen.
pub fn sync(db: &Connection) -> Result<()> {
    db.execute_batch("DELETE FROM main.__msduck_index_catalog c WHERE NOT EXISTS(
        SELECT 1 FROM sys.objects o JOIN sys.schemas s USING(schema_id)
        JOIN duckdb_tables() t ON t.schema_name=s.name AND t.table_name=o.name
        JOIN duckdb_indexes() i ON i.schema_name=c.backend_schema AND i.index_name=c.backend_name AND i.table_oid=t.table_oid
        WHERE o.object_id=c.object_id AND o.type='U');
        DELETE FROM main.__msduck_index_keys k WHERE NOT EXISTS(SELECT 1 FROM main.__msduck_index_catalog c WHERE c.incarnation=k.incarnation)")?;
    Ok(())
}

/// Read only identities that still belong to the same live physical table/index.
pub fn acquire(db: &Connection) -> Result<Vec<Index>> {
    Ok(db.prepare("SELECT c.object_id,c.index_id,c.name,c.is_unique,c.incarnation,c.backend_schema,c.backend_name
        FROM main.__msduck_index_catalog c JOIN sys.objects o ON o.object_id=c.object_id AND o.type='U'
        JOIN sys.schemas s ON s.schema_id=o.schema_id
        JOIN duckdb_tables() t ON t.schema_name=s.name AND t.table_name=o.name
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

/// Migrate ordinary unmanaged indexes into explicit logical/backend identities.
/// The complete migration is atomic. Rebuild unique integer indexes with SQL
/// Server NULL comparison; incompatible existing data aborts the migration.
pub fn reconcile(db: &Connection, owner: Transaction) -> Result<()> {
    transaction(db, owner, || {
        sync(db)?;
        let records=db.prepare("SELECT o.object_id,i.schema_name,i.index_name,i.sql FROM duckdb_indexes() i
            JOIN sys.schemas s ON s.name=i.schema_name
            JOIN sys.objects o ON o.schema_id=s.schema_id AND o.name=i.table_name AND o.type='U'
            WHERE NOT EXISTS(SELECT 1 FROM main.__msduck_index_catalog c WHERE c.backend_schema=i.schema_name AND c.backend_name=i.index_name)
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
        JOIN sys.objects o ON o.schema_id=s.schema_id AND o.name=i.table_name AND o.type='U'
        WHERE NOT EXISTS(SELECT 1 FROM main.__msduck_index_catalog c WHERE c.backend_schema=i.schema_name AND c.backend_name=i.index_name AND c.object_id=o.object_id)",[],|r|r.get(0))?;
    ensure!(
        unknown == 0,
        "unmanaged index catalog is incomplete; reconcile before binding"
    );
    let constraints: i64 = db.query_row(
        "SELECT count(*) FROM duckdb_constraints() c
        JOIN sys.schemas s ON s.name=c.schema_name
        JOIN sys.objects o ON o.schema_id=s.schema_id AND o.name=c.table_name AND o.type='U'
        WHERE c.constraint_type IN ('PRIMARY KEY','UNIQUE')",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        constraints == 0,
        "constraint-backed index catalog is not yet supported"
    );
    acquire(db)
}

/// Publish the supported heap/ordinary-index catalog rows. Unsupported physical
/// states raise explicitly rather than silently omitting indexes or inventing
/// heap rows for constraint-backed tables. Wire declaration metadata still needs
/// the root catalog adapter's logical type/property integration.
pub fn publish_views(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE OR REPLACE MACRO main.__msduck_index_catalog_ready() AS
        CASE WHEN EXISTS(SELECT 1 FROM duckdb_indexes() i
          JOIN sys.schemas s ON s.name=i.schema_name
          JOIN sys.objects o ON o.schema_id=s.schema_id AND o.name=i.table_name AND o.type='U'
          WHERE NOT EXISTS(SELECT 1 FROM main.__msduck_index_catalog c WHERE c.backend_schema=i.schema_name AND c.backend_name=i.index_name AND c.object_id=o.object_id))
        OR EXISTS(SELECT 1 FROM duckdb_constraints() c
          JOIN sys.schemas s ON s.name=c.schema_name
          JOIN sys.objects o ON o.schema_id=s.schema_id AND o.name=c.table_name AND o.type='U'
          WHERE c.constraint_type IN ('PRIMARY KEY','UNIQUE'))
        THEN error('Index catalog contains unsupported unmanaged or constraint-backed indexes') ELSE true END;
        CREATE OR REPLACE VIEW main.__msduck_live_indexes AS
        SELECT c.* FROM main.__msduck_index_catalog c
          JOIN sys.objects o ON o.object_id=c.object_id AND o.type='U'
          JOIN sys.schemas s ON s.schema_id=o.schema_id
          JOIN duckdb_tables() t ON t.schema_name=s.name AND t.table_name=o.name
          JOIN duckdb_indexes() i ON i.schema_name=c.backend_schema AND i.index_name=c.backend_name AND i.table_oid=t.table_oid;
        CREATE OR REPLACE VIEW sys.indexes AS
        SELECT r.object_id,r.name,r.index_id,CAST(CASE WHEN r.index_id=0 THEN 0 ELSE 2 END AS UTINYINT) AS type,
          CAST(CASE WHEN r.index_id=0 THEN 'HEAP' ELSE 'NONCLUSTERED' END AS VARCHAR) AS type_desc,
          r.is_unique,CAST(1 AS INTEGER) AS data_space_id,false AS ignore_dup_key,
          false AS is_primary_key,false AS is_unique_constraint,CAST(0 AS UTINYINT) AS fill_factor,
          false AS is_padded,false AS is_disabled,false AS is_hypothetical,false AS is_ignored_in_optimization,
          true AS allow_row_locks,true AS allow_page_locks,false AS has_filter,
          CAST(NULL AS VARCHAR) AS filter_definition,CAST(NULL AS INTEGER) AS compression_delay,
          false AS suppress_dup_key_messages,false AS auto_created,false AS optimize_for_sequential_key
        FROM (SELECT object_id,CAST(NULL AS VARCHAR) AS name,CAST(0 AS INTEGER) AS index_id,false AS is_unique
              FROM sys.objects WHERE type='U'
              UNION ALL SELECT object_id,name,index_id,is_unique FROM main.__msduck_live_indexes) r
        WHERE main.__msduck_index_catalog_ready();
        CREATE OR REPLACE VIEW sys.index_columns AS
        SELECT i.object_id,i.index_id,k.ordinal AS index_column_id,k.column_id,
          CAST(k.ordinal AS UTINYINT) AS key_ordinal,CAST(0 AS UTINYINT) AS partition_ordinal,
          false AS is_descending_key,false AS is_included_column,
          CAST(0 AS UTINYINT) AS column_store_order_ordinal,CAST(0 AS UTINYINT) AS data_clustering_ordinal
        FROM main.__msduck_live_indexes i JOIN main.__msduck_index_keys k USING(incarnation)
        WHERE main.__msduck_index_catalog_ready()")?;
    Ok(())
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
