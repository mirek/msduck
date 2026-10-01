//! `sp_rename` for tables, views, modules, constraints, columns and indexes
//! (reference/gaps-catalog.json).
//!
//! Object IDs, column IDs and index IDs stay; only names change, in every
//! store that keeps them, and the backend tables, views, columns and
//! indexes follow. DuckDB refuses to rename a table, or a column of one,
//! while indexes depend on it, so the table's indexes are dropped and
//! created again around the rename in the same transaction. Like SQL
//! Server, definitions that name the old object (views, modules) are not
//! changed. A column that a CHECK constraint or computed column uses
//! cannot be renamed (15336), nor can a computed column (4928).
use super::call::{self, Diagnostic};
use super::sources::atomically;
use crate::engine::{Parameter, Session, ext::Exec};
use anyhow::{Result, bail};
use sqlparser::ast::{Expr, Ident, ObjectNamePart, Statement, VisitMut, VisitorMut};
use std::collections::HashMap;
use std::ops::ControlFlow;

const PROCEDURE: &str = "sp_rename";

fn error(number: i32, state: u8, severity: u8, line: i32, message: String) -> Diagnostic {
    Diagnostic {
        number,
        state,
        severity,
        message,
        procedure: PROCEDURE,
        line,
    }
}

fn caution() -> Diagnostic {
    error(
        15477,
        1,
        10,
        801,
        "Caution: Changing any part of an object name could break scripts and stored procedures."
            .into(),
    )
}

/// PARSENAME's parts of a multi-part name: brackets and double quotes
/// delimit, and up to four parts are allowed.
fn parts(name: &str) -> Option<Vec<String>> {
    let mut parts = vec![String::new()];
    let mut chars = name.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '[' | '"' => {
                let close = if c == '[' { ']' } else { '"' };
                loop {
                    match chars.next()? {
                        ch if ch == close => {
                            if chars.peek() == Some(&close) {
                                chars.next();
                                parts.last_mut()?.push(close);
                            } else {
                                break;
                            }
                        }
                        ch => parts.last_mut()?.push(ch),
                    }
                }
            }
            '.' => parts.push(String::new()),
            c => parts.last_mut()?.push(c),
        }
    }
    (parts.len() <= 4).then_some(parts)
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// An object of the current database.
#[derive(Clone, Debug)]
struct Object {
    id: i32,
    schema: String,
    name: String,
    /// `sys.objects.type`, trimmed.
    kind: String,
}

fn object_by(
    db: &duckdb::Connection,
    filter: &str,
    values: &[&dyn duckdb::ToSql],
) -> Result<Option<Object>> {
    let sql = format!(
        "SELECT o.object_id,s.name,o.name,rtrim(o.type) FROM sys.objects o
         JOIN main.__msduck_schemas s ON s.schema_id=o.schema_id
         WHERE NOT o.is_ms_shipped AND NOT main.__msduck_temporary(o.name) AND {filter}"
    );
    let mut statement = db.prepare(&sql)?;
    let mut rows = statement.query_map(values, |row| {
        Ok(Object {
            id: row.get(0)?,
            schema: row.get(1)?,
            name: row.get(2)?,
            kind: row.get(3)?,
        })
    })?;
    Ok(rows.next().transpose()?)
}

/// An object named by up to three parts; a database part must name the
/// current database.
fn object(session: &Session, parts: &[String]) -> Result<Option<Object>> {
    let (database, schema, name) = match parts {
        [name] => (None, None, name),
        [schema, name] => (None, Some(schema), name),
        [database, schema, name] => (Some(database), Some(schema), name),
        _ => return Ok(None),
    };
    if database.is_some_and(|database| {
        !database.is_empty() && !database.eq_ignore_ascii_case(&session.database.name)
    }) {
        return Ok(None);
    }
    let schema = schema
        .filter(|schema| !schema.is_empty())
        .map_or("dbo", |schema| schema.as_str());
    object_by(
        &session.db,
        "lower(s.name)=lower(?) AND lower(o.name)=lower(?)",
        &[&schema, name],
    )
}

/// The table (or view) of a column or index name: all parts but the last.
fn table(session: &Session, parts: &[String]) -> Result<Option<Object>> {
    if parts.len() < 2 {
        return Ok(None);
    }
    Ok(object(session, &parts[..parts.len() - 1])?.filter(|object| object.kind == "U"))
}

fn column(db: &duckdb::Connection, table: i32, name: &str) -> Result<Option<(i32, String)>> {
    let mut statement = db.prepare(
        "SELECT column_id,name FROM main.__msduck_column_info WHERE object_id=? AND lower(name)=lower(?)",
    )?;
    let mut rows = statement.query_map(duckdb::params![table, name], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })?;
    Ok(rows.next().transpose()?)
}

/// An index of a table: a registered index or a keys-managed one, or the
/// index of a PRIMARY KEY or UNIQUE constraint.
#[derive(Debug)]
enum Index {
    Catalog { name: String },
    Key { tag: i64, name: String },
}

fn index(db: &duckdb::Connection, table: i32, name: &str) -> Result<Option<Index>> {
    let key: Option<(i64, String)> = db
        .prepare("SELECT tag,name FROM main.__msduck_keys WHERE object_id=? AND lower(name)=lower(?) ORDER BY tag LIMIT 1")?
        .query_map(duckdb::params![table, name], |row| Ok((row.get(0)?, row.get(1)?)))?
        .next()
        .transpose()?;
    if let Some((tag, name)) = key {
        return Ok(Some(Index::Key { tag, name }));
    }
    let catalog: Option<String> = db
        .prepare(
            "SELECT name FROM main.__msduck_index_catalog WHERE object_id=? AND name_key=lower(?)",
        )?
        .query_map(duckdb::params![table, name], |row| row.get(0))?
        .next()
        .transpose()?;
    Ok(catalog.map(|name| Index::Catalog { name }))
}

/// What `@objname` resolved to.
enum Target {
    Object(Object),
    Column {
        table: Object,
        id: i32,
        name: String,
    },
    Index {
        table: Object,
        index: Index,
    },
}

pub(super) fn run(
    session: &mut Session,
    statement: &Statement,
    variables: &mut HashMap<String, Parameter>,
) -> Result<Exec> {
    let bound = call::bind(
        session,
        PROCEDURE,
        &[("@objname", false), ("@newname", false), ("@objtype", true)],
        statement,
        variables,
    )?;
    let argument = |name: &str| bound.arguments.get(name).cloned().flatten();
    let objname = argument("@objname");
    let newname = argument("@newname");
    let objtype = argument("@objtype");
    let fail = |session: &Session,
                variables: &mut HashMap<String, Parameter>,
                diagnostics: Vec<Diagnostic>| {
        call::finish(session, &bound, Vec::new(), diagnostics, 1, variables)
    };
    let kind = objtype.as_deref().map(str::to_ascii_uppercase);
    if let Some(kind) = &kind
        && !matches!(
            kind.as_str(),
            "COLUMN" | "DATABASE" | "INDEX" | "OBJECT" | "STATISTICS" | "USERDATATYPE"
        )
    {
        return fail(
            session,
            variables,
            vec![error(
                15249,
                1,
                11,
                90,
                format!(
                    "Error: Explicit @objtype '{}' is unrecognized.",
                    objtype.as_deref().unwrap_or_default()
                ),
            )],
        );
    }
    let Some(newname) = newname else {
        return fail(
            session,
            variables,
            vec![error(
                15223,
                11,
                11,
                96,
                "Error: The input parameter 'NewName' is not allowed to be null.".into(),
            )],
        );
    };
    let Some(objname) = objname else {
        return fail(
            session,
            variables,
            vec![error(
                15223,
                1,
                11,
                101,
                "Error: The input parameter 'OldName' is not allowed to be null.".into(),
            )],
        );
    };
    if newname.is_empty() || newname.chars().count() > 128 {
        return fail(
            session,
            variables,
            vec![
                Diagnostic {
                    number: 15004,
                    state: 1,
                    severity: 16,
                    message: "Name cannot be NULL.".into(),
                    procedure: "sp_validname",
                    line: 17,
                },
                error(
                    15224,
                    15,
                    11,
                    109,
                    format!(
                        "Error: The value for the @newname parameter contains invalid characters or violates a basic restriction ({newname})."
                    ),
                ),
            ],
        );
    }
    if matches!(
        kind.as_deref(),
        Some("DATABASE" | "STATISTICS" | "USERDATATYPE")
    ) {
        bail!(
            "unsupported sp_rename @objtype '{}'",
            objtype.unwrap_or_default()
        );
    }
    let parts = parts(&objname).unwrap_or_default();
    let ambiguous = |line: i32| {
        error(
            15248,
            1,
            11,
            line,
            format!(
                "Either the parameter @objname is ambiguous or the claimed @objtype ({}) is wrong.",
                objtype.as_deref().unwrap_or_default()
            ),
        )
    };
    let target = match kind.as_deref() {
        Some("OBJECT") => match object(session, &parts)? {
            Some(object) => Target::Object(object),
            None => return fail(session, variables, vec![ambiguous(620)]),
        },
        Some("COLUMN") => {
            let found = match table(session, &parts)? {
                Some(table) => column(&session.db, table.id, parts.last().unwrap())?
                    .map(|(id, name)| Target::Column { table, id, name }),
                None => None,
            };
            match found {
                Some(found) => found,
                None => return fail(session, variables, vec![ambiguous(269)]),
            }
        }
        Some("INDEX") => {
            let found = match table(session, &parts)? {
                Some(table) => index(&session.db, table.id, parts.last().unwrap())?
                    .map(|index| Target::Index { table, index }),
                None => None,
            };
            match found {
                Some(found) => found,
                None => return fail(session, variables, vec![ambiguous(450)]),
            }
        }
        _ => {
            let mut found = object(session, &parts)?.map(Target::Object);
            if found.is_none()
                && let Some(table) = table(session, &parts)?
            {
                let last = parts.last().unwrap();
                found = match column(&session.db, table.id, last)? {
                    Some((id, name)) => Some(Target::Column {
                        table: table.clone(),
                        id,
                        name,
                    }),
                    None => index(&session.db, table.id, last)?
                        .map(|index| Target::Index { table, index }),
                };
            }
            match found {
                Some(found) => found,
                None => {
                    return fail(
                        session,
                        variables,
                        vec![error(
                            15225,
                            1,
                            11,
                            637,
                            format!(
                                "No item by the name of '{objname}' could be found in the current database '{}', given that @itemtype was input as '(null)'.",
                                session.database.name
                            ),
                        )],
                    );
                }
            }
        }
    };
    let duplicate = |what: &str| {
        error(
            15335,
            1,
            11,
            738,
            format!(
                "Error: The new name '{newname}' is already in use as a {what} name and would cause a duplicate that is not permitted."
            ),
        )
    };
    match &target {
        Target::Object(object) => {
            let taken = object_by(
                &session.db,
                "lower(s.name)=lower(?) AND lower(o.name)=lower(?) AND o.object_id<>?",
                &[&object.schema, &newname, &object.id],
            )?
            .is_some();
            if taken {
                return fail(session, variables, vec![duplicate("object")]);
            }
        }
        Target::Column { table, id, .. } => {
            if let Some((other, _)) = column(&session.db, table.id, &newname)?
                && other != *id
            {
                return fail(session, variables, vec![duplicate("COLUMN")]);
            }
            if enforced(&session.db, table.id, *id)? {
                return fail(
                    session,
                    variables,
                    vec![error(
                        15336,
                        1,
                        16,
                        774,
                        format!(
                            "Object '{objname}' cannot be renamed because the object participates in enforced dependencies."
                        ),
                    )],
                );
            }
        }
        Target::Index {
            table,
            index: found,
        } => {
            let current = match found {
                Index::Catalog { name } | Index::Key { name, .. } => name,
            };
            if !current.eq_ignore_ascii_case(&newname)
                && index(&session.db, table.id, &newname)?.is_some()
            {
                return fail(session, variables, vec![duplicate("INDEX")]);
            }
        }
    }
    if let Target::Column { table, id, name } = &target {
        let computed: bool = session.db.query_row(
            "SELECT count(*)>0 FROM main.__msduck_computed_columns k JOIN main.__msduck_column_info c
               ON c.object_id=k.object_id AND k.name_key=lower(c.name) WHERE c.object_id=? AND c.column_id=?",
            duckdb::params![table.id, id],
            |row| row.get(0),
        )?;
        if computed {
            return call::finish(
                session,
                &bound,
                Vec::new(),
                vec![
                    caution(),
                    error(
                        4928,
                        1,
                        16,
                        905,
                        format!("Cannot alter column '{name}' because it is 'COMPUTED'."),
                    ),
                ],
                1,
                variables,
            );
        }
    }
    atomically(session, |session| apply(session, &target, &newname))?;
    call::finish(session, &bound, Vec::new(), vec![caution()], 0, variables)
}

/// Whether a CHECK constraint or computed column uses the column.
fn enforced(db: &duckdb::Connection, table: i32, column: i32) -> Result<bool> {
    let name: String = db.query_row(
        "SELECT name FROM main.__msduck_column_info WHERE object_id=? AND column_id=?",
        duckdb::params![table, column],
        |row| row.get(0),
    )?;
    let checks: bool = db.query_row(
        "SELECT count(*)>0 FROM main.__msduck_constraints WHERE parent_object_id=? AND type_code='C'
           AND list_contains(list_transform(columns,lambda x: lower(x)),lower(?))",
        duckdb::params![table, name],
        |row| row.get(0),
    )?;
    if checks {
        return Ok(true);
    }
    let sources: Vec<Option<String>> = db
        .prepare("SELECT source FROM main.__msduck_computed_sources WHERE object_id=?")?
        .query_map([table], |row| row.get(0))?
        .collect::<duckdb::Result<_>>()?;
    for source in sources.into_iter().flatten() {
        if let Some(expr) = msduck_sql::dialect::ext::catalog::definition::parse_expression(&source)
            && references(&expr, &name)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn references(expr: &Expr, column: &str) -> bool {
    struct Find<'a>(&'a str, bool);
    impl sqlparser::ast::Visitor for Find<'_> {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            let name = match expr {
                Expr::Identifier(ident) => Some(&ident.value),
                Expr::CompoundIdentifier(parts) => parts.last().map(|ident| &ident.value),
                _ => None,
            };
            if name.is_some_and(|name| name.eq_ignore_ascii_case(self.0)) {
                self.1 = true;
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        }
    }
    let mut find = Find(column, false);
    let _ = sqlparser::ast::Visit::visit(expr, &mut find);
    find.1
}

/// The backend indexes of a table: name and definition.
fn indexes(db: &duckdb::Connection, table: &Object) -> Result<Vec<(String, String)>> {
    Ok(db
        .prepare(
            "SELECT index_name,sql FROM duckdb_indexes()
             WHERE database_name=current_database() AND schema_name=? AND table_name=? ORDER BY index_oid",
        )?
        .query_map([&table.schema, &table.name], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<duckdb::Result<_>>()?)
}

/// An index definition under a new backend name, for the renamed table or
/// column.
fn redefine(sql: &str, name: &str, table: &str, column: Option<(&str, &str)>) -> Result<String> {
    let mut statements =
        sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::GenericDialect {}, sql)?;
    anyhow::ensure!(statements.len() == 1, "unexpected index definition");
    let Statement::CreateIndex(mut index) = statements.remove(0) else {
        bail!("unexpected index definition");
    };
    if let Some(ObjectNamePart::Identifier(last)) = index.table_name.0.last_mut() {
        *last = Ident::with_quote('"', table);
    }
    index.name = Some(sqlparser::ast::ObjectName::from(vec![Ident::with_quote(
        '"', name,
    )]));
    if let Some((old, new)) = column {
        struct Rename<'a>(&'a str, &'a str);
        impl VisitorMut for Rename<'_> {
            type Break = ();
            fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
                match expr {
                    Expr::Identifier(ident) if ident.value.eq_ignore_ascii_case(self.0) => {
                        *ident = Ident::with_quote('"', self.1);
                    }
                    Expr::CompoundIdentifier(parts) => {
                        if let Some(last) = parts.last_mut()
                            && last.value.eq_ignore_ascii_case(self.0)
                        {
                            *last = Ident::with_quote('"', self.1);
                        }
                    }
                    _ => {}
                }
                ControlFlow::Continue(())
            }
        }
        let _ = index.columns.visit(&mut Rename(old, new));
        if let Some(predicate) = &mut index.predicate {
            let _ = predicate.visit(&mut Rename(old, new));
        }
    }
    Ok(Statement::CreateIndex(index).to_string())
}

/// Rename a table or one of its columns in the backend, around its indexes.
fn rename_backend(
    session: &Session,
    table: &Object,
    statement: &str,
    new_table: &str,
    column: Option<(&str, &str)>,
) -> Result<()> {
    let db = &session.db;
    let saved = indexes(db, table)?;
    for (name, _) in &saved {
        db.execute_batch(&format!(
            "DROP INDEX {}.{}",
            quote(&table.schema),
            quote(name)
        ))?;
    }
    db.execute_batch(statement)?;
    // DuckDB cannot reuse a name dropped in the same transaction, so the
    // indexes come back under new backend names, which the catalogs follow.
    for (name, sql) in &saved {
        let base = match name.rsplit_once("_r") {
            Some((base, suffix)) if suffix.bytes().all(|b| b.is_ascii_digit()) => base,
            _ => name.as_str(),
        };
        let tag: i64 = db.query_row("SELECT nextval('main.__msduck_key_tags')", [], |row| {
            row.get(0)
        })?;
        let renamed = format!("{base}_r{tag}");
        db.execute_batch(&redefine(sql, &renamed, new_table, column)?)?;
        db.execute(
            "UPDATE main.__msduck_keys SET backend_name=? WHERE backend_name=? AND object_id=?",
            duckdb::params![renamed, name, table.id],
        )?;
        db.execute(
            "UPDATE main.__msduck_index_catalog SET backend_name=? WHERE backend_schema=? AND backend_name=?",
            duckdb::params![renamed, table.schema, name],
        )?;
    }
    // The table-owned index catalog keeps the backend table's OID.
    db.execute(
        "UPDATE main.__msduck_index_catalog SET table_oid=(
           SELECT CAST(table_oid AS BIGINT) FROM duckdb_tables()
           WHERE database_name=current_database() AND schema_name=? AND table_name=?)
         WHERE object_id=?",
        duckdb::params![table.schema, new_table, table.id],
    )?;
    Ok(())
}

/// A name list with one name replaced, ignoring case.
fn replaced(list: &str, old: &str, new: &str) -> String {
    format!(
        "list_transform({list},lambda x: CASE WHEN lower(x)=lower({}) THEN {} ELSE x END)",
        literal(old),
        literal(new)
    )
}

fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

fn apply(session: &mut Session, target: &Target, newname: &str) -> Result<()> {
    let db = &session.db;
    match target {
        Target::Object(object) => match object.kind.as_str() {
            "U" => {
                rename_backend(
                    session,
                    object,
                    &format!(
                        "ALTER TABLE {}.{} RENAME TO {}",
                        quote(&object.schema),
                        quote(&object.name),
                        quote(newname)
                    ),
                    newname,
                    None,
                )?;
                rename_object(db, object.id, newname)?;
            }
            "V" => {
                db.execute_batch(&format!(
                    "ALTER VIEW {}.{} RENAME TO {}",
                    quote(&object.schema),
                    quote(&object.name),
                    quote(newname)
                ))?;
                rename_object(db, object.id, newname)?;
            }
            "P" | "FN" | "IF" | "TF" | "TR" => {
                super::super::modules::rename(db, object.id, newname)?;
            }
            "PK" | "UQ" => {
                db.execute(
                    "UPDATE main.__msduck_keys SET name=? WHERE tag=?",
                    duckdb::params![newname, i64::from(object.id) - 2_000_000_000],
                )?;
            }
            "C" | "F" => {
                db.execute(
                    "UPDATE main.__msduck_constraints SET name=?,modify_date=CAST(current_timestamp AS TIMESTAMP) WHERE object_id=?",
                    duckdb::params![newname, object.id],
                )?;
            }
            "D" => {
                db.execute(
                    "UPDATE main.__msduck_default_constraints SET name=?,modify_date=CAST(current_timestamp AS TIMESTAMP) WHERE object_id=?",
                    duckdb::params![newname, object.id],
                )?;
                db.execute(
                    "UPDATE main.__msduck_default_sources SET is_system_named=false WHERE object_id=?",
                    [object.id],
                )?;
            }
            other => bail!("unsupported sp_rename of an object of type {other}"),
        },
        Target::Column { table, id, name } => {
            rename_backend(
                session,
                table,
                &format!(
                    "ALTER TABLE {}.{} RENAME COLUMN {} TO {}",
                    quote(&table.schema),
                    quote(&table.name),
                    quote(name),
                    quote(newname)
                ),
                &table.name,
                Some((name, newname)),
            )?;
            db.execute(
                "UPDATE main.__msduck_columns SET name=? WHERE object_id=? AND column_id=?",
                duckdb::params![newname, table.id, id],
            )?;
            db.execute(
                "UPDATE main.__msduck_index_keys SET column_name=? WHERE column_id=? AND incarnation IN
                   (SELECT incarnation FROM main.__msduck_index_catalog WHERE object_id=?)",
                duckdb::params![newname, id, table.id],
            )?;
            // Keys and constraints name their columns.
            let keys: Vec<(i64, String, String, String)> = db
                .prepare("SELECT tag,key_columns,included_columns,filter_columns FROM main.__msduck_keys WHERE object_id=?")?
                .query_map([table.id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?
                .collect::<duckdb::Result<_>>()?;
            for (tag, key, included, filter) in keys {
                let rename = |text: &str| -> Result<String> {
                    let names: Vec<String> = serde_json::from_str(text).unwrap_or_default();
                    Ok(serde_json::to_string(
                        &names
                            .into_iter()
                            .map(|n| {
                                if n.eq_ignore_ascii_case(name) {
                                    newname.to_owned()
                                } else {
                                    n
                                }
                            })
                            .collect::<Vec<_>>(),
                    )?)
                };
                db.execute(
                    "UPDATE main.__msduck_keys SET key_columns=?,included_columns=?,filter_columns=? WHERE tag=?",
                    duckdb::params![rename(&key)?, rename(&included)?, rename(&filter)?, tag],
                )?;
            }
            db.execute(
                &format!(
                    "UPDATE main.__msduck_constraints SET columns={} WHERE parent_object_id=?",
                    replaced("columns", name, newname)
                ),
                [table.id],
            )?;
            db.execute(
                &format!(
                    "UPDATE main.__msduck_constraints SET referenced_columns={} WHERE referenced_object_id=?",
                    replaced("referenced_columns", name, newname)
                ),
                [table.id],
            )?;
            db.execute(
                "UPDATE main.__msduck_constraints SET parent_column=? WHERE parent_object_id=? AND lower(parent_column)=lower(?)",
                duckdb::params![newname, table.id, name],
            )?;
        }
        Target::Index { index, .. } => match index {
            Index::Key { tag, .. } => {
                db.execute(
                    "UPDATE main.__msduck_keys SET name=? WHERE tag=?",
                    duckdb::params![newname, tag],
                )?;
                db.execute(
                    "UPDATE main.__msduck_index_catalog SET name=?,name_key=lower(?) WHERE incarnation=(SELECT incarnation FROM main.__msduck_keys WHERE tag=?)",
                    duckdb::params![newname, newname, tag],
                )?;
            }
            Index::Catalog { name } => {
                let Target::Index { table, .. } = target else {
                    unreachable!()
                };
                db.execute(
                    "UPDATE main.__msduck_index_catalog SET name=?,name_key=lower(?) WHERE object_id=? AND name_key=lower(?)",
                    duckdb::params![newname, newname, table.id, name],
                )?;
            }
        },
    }
    crate::object_catalog::sync(db)?;
    crate::index_catalog::sync(db)?;
    Ok(())
}

/// Rename a table or view in the object catalog, keeping its ID.
fn rename_object(db: &duckdb::Connection, id: i32, name: &str) -> Result<()> {
    db.execute(
        "UPDATE main.__msduck_objects SET name=?,modify_date=CAST(current_timestamp AS TIMESTAMP) WHERE object_id=?",
        duckdb::params![name, id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_split_like_parsename() {
        assert_eq!(parts("dbo.t").unwrap(), ["dbo", "t"]);
        assert_eq!(parts("[dbo].[[rp5]]]").unwrap(), ["dbo", "[rp5]"]);
        assert_eq!(parts("db..t").unwrap(), ["db", "", "t"]);
        assert_eq!(parts("\"a.b\".c").unwrap(), ["a.b", "c"]);
        assert!(parts("a.b.c.d.e").is_none());
        assert!(parts("[unterminated").is_none());
    }
}
