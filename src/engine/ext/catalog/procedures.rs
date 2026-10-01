//! `sp_pkeys` and `sp_fkeys`, with SQL Server's result sets
//! (reference/gaps-catalog.json).
//!
//! Names match exactly, without wildcards and ignoring case. A qualifier
//! other than the current database's name fails with 15250. The completion
//! tokens follow the procedures' bodies: `sp_pkeys` runs five statements
//! before its result, `sp_fkeys` none.
use super::call::{self, Cell, Diagnostic, Kind, ResultColumn};
use crate::engine::{Parameter, Session, ext::Exec};
use anyhow::Result;
use sqlparser::ast::Statement;
use std::collections::HashMap;

const fn column(name: &'static str, kind: Kind, nullable: bool, expression: bool) -> ResultColumn {
    ResultColumn {
        name,
        kind,
        nullable,
        expression,
    }
}

const PKEYS: [ResultColumn; 6] = [
    column("TABLE_QUALIFIER", Kind::Name, true, true),
    column("TABLE_OWNER", Kind::Name, true, true),
    column("TABLE_NAME", Kind::Name, true, true),
    column("COLUMN_NAME", Kind::Name, true, true),
    column("KEY_SEQ", Kind::SmallInt, true, true),
    column("PK_NAME", Kind::Name, true, true),
];

const FKEYS: [ResultColumn; 14] = [
    column("PKTABLE_QUALIFIER", Kind::Name, true, true),
    column("PKTABLE_OWNER", Kind::Name, true, true),
    column("PKTABLE_NAME", Kind::Name, true, true),
    column("PKCOLUMN_NAME", Kind::Name, true, false),
    column("FKTABLE_QUALIFIER", Kind::Name, true, true),
    column("FKTABLE_OWNER", Kind::Name, true, true),
    column("FKTABLE_NAME", Kind::Name, true, true),
    column("FKCOLUMN_NAME", Kind::Name, true, false),
    column("KEY_SEQ", Kind::SmallInt, false, true),
    column("UPDATE_RULE", Kind::SmallInt, true, true),
    column("DELETE_RULE", Kind::SmallInt, true, true),
    column("FK_NAME", Kind::Name, true, true),
    column("PK_NAME", Kind::Name, true, false),
    column("DEFERRABILITY", Kind::SmallInt, true, true),
];

fn qualifier_error(procedure: &'static str, line: i32) -> Diagnostic {
    Diagnostic {
        number: 15250,
        state: 1,
        severity: 16,
        message: "The database name component of the object qualifier must be the name of the current database.".into(),
        procedure,
        line,
    }
}

/// A call to another database's procedure runs in that database; only the
/// current database's catalog is available here.
fn same_database(session: &Session, statement: &Statement) -> bool {
    match call::database_part(statement) {
        Some(database) => {
            database.is_empty() || database.eq_ignore_ascii_case(&session.database.name)
        }
        None => true,
    }
}

fn mismatched(session: &Session, qualifier: Option<&Option<String>>) -> bool {
    matches!(qualifier, Some(Some(name)) if !name.eq_ignore_ascii_case(&session.database.name))
}

fn text(row: &duckdb::Row<'_>, index: usize) -> duckdb::Result<Cell> {
    Ok(match row.get::<_, Option<String>>(index)? {
        Some(text) => Cell::Text(text),
        None => Cell::Null,
    })
}

fn int(row: &duckdb::Row<'_>, index: usize) -> duckdb::Result<Cell> {
    Ok(match row.get::<_, Option<i64>>(index)? {
        Some(value) => Cell::Int(value),
        None => Cell::Null,
    })
}

pub(super) fn pkeys(
    session: &mut Session,
    statement: &Statement,
    variables: &mut HashMap<String, Parameter>,
) -> Result<Exec> {
    anyhow::ensure!(
        same_database(session, statement),
        "unsupported sp_pkeys call in another database"
    );
    let bound = call::bind(
        session,
        "sp_pkeys",
        &[
            ("@table_name", false),
            ("@table_owner", true),
            ("@table_qualifier", true),
        ],
        statement,
        variables,
    )?;
    let arguments = &bound.arguments;
    if mismatched(session, arguments.get("@table_qualifier")) {
        return call::finish(
            session,
            &bound,
            Vec::new(),
            vec![qualifier_error("sp_pkeys", 17)],
            -6,
            variables,
        );
    }
    let name = arguments.get("@table_name").cloned().flatten();
    let owner = arguments.get("@table_owner").cloned().flatten();
    let database = session.database.name.clone();
    let rows: Vec<Vec<Cell>> = session
        .db
        .prepare(
            "SELECT ? AS qualifier,s.name,o.name,coalesce(c.name,k.column_name),CAST(k.ordinal AS BIGINT),mk.name
             FROM main.__msduck_keys mk
             JOIN main.__msduck_catalog_tables o ON o.object_id=mk.object_id
             JOIN main.__msduck_schemas s ON s.schema_id=o.schema_id
             CROSS JOIN LATERAL (SELECT unnest(from_json(mk.key_columns,'[\"VARCHAR\"]')) AS column_name,
               unnest(generate_series(1,len(from_json(mk.key_columns,'[\"VARCHAR\"]')))) AS ordinal) k
             LEFT JOIN main.__msduck_column_info c ON c.object_id=o.object_id AND lower(c.name)=lower(k.column_name)
             WHERE mk.kind='PK' AND lower(o.name)=lower(?) AND (? IS NULL OR lower(s.name)=lower(?))
             ORDER BY s.name,o.name,k.ordinal",
        )?
        .query_map(
            duckdb::params![database, name, owner, owner],
            |row| {
                Ok(vec![
                    text(row, 0)?,
                    text(row, 1)?,
                    text(row, 2)?,
                    text(row, 3)?,
                    int(row, 4)?,
                    text(row, 5)?,
                ])
            },
        )?
        .collect::<duckdb::Result<_>>()?;
    let mut tokens = Vec::new();
    // The body's statements before its SELECT.
    if !session.nocount {
        for _ in 0..3 {
            call::done_in_proc(&mut tokens, 0x01, 192, 0);
        }
        for _ in 0..2 {
            call::done_in_proc(&mut tokens, 0x11, 193, 1);
        }
    }
    call::result_set(&mut tokens, &PKEYS, &rows)?;
    if !session.nocount {
        call::done_in_proc(&mut tokens, 0x11, 193, rows.len() as u64);
    }
    session.rowcount = rows.len() as u64;
    call::finish(session, &bound, tokens, vec![], 0, variables)
}

pub(super) fn fkeys(
    session: &mut Session,
    statement: &Statement,
    variables: &mut HashMap<String, Parameter>,
) -> Result<Exec> {
    anyhow::ensure!(
        same_database(session, statement),
        "unsupported sp_fkeys call in another database"
    );
    let bound = call::bind(
        session,
        "sp_fkeys",
        &[
            ("@pktable_name", true),
            ("@pktable_owner", true),
            ("@pktable_qualifier", true),
            ("@fktable_name", true),
            ("@fktable_owner", true),
            ("@fktable_qualifier", true),
        ],
        statement,
        variables,
    )?;
    let argument = |name: &str| bound.arguments.get(name).cloned().flatten();
    let primary = argument("@pktable_name");
    let foreign = argument("@fktable_name");
    let failure = if primary.is_none() && foreign.is_none() {
        Some(Diagnostic {
            number: 15252,
            state: 1,
            severity: 16,
            message: "The primary or foreign key table name must be given.".into(),
            procedure: "sp_fkeys",
            line: 20,
        })
    } else if mismatched(session, bound.arguments.get("@fktable_qualifier")) {
        Some(qualifier_error("sp_fkeys", 28))
    } else if mismatched(session, bound.arguments.get("@pktable_qualifier")) {
        Some(qualifier_error("sp_fkeys", 37))
    } else {
        None
    };
    if let Some(failure) = failure {
        return call::finish(session, &bound, Vec::new(), vec![failure], -6, variables);
    }
    let database = session.database.name.clone();
    let order = if primary.is_some() {
        "fs.name,f.name,k.ordinal,c.name"
    } else {
        "ps.name,p.name,k.ordinal,c.name"
    };
    // SQL Server reports both rules as 1 (NO ACTION) when only the foreign
    // key table is given.
    let rules = primary.is_some();
    let rows: Vec<Vec<Cell>> = session
        .db
        .prepare(&format!(
            "SELECT ?,ps.name,p.name,coalesce(pc.name,k.referenced),?,fs.name,f.name,coalesce(fc.name,k.referencing),
               CAST(k.ordinal AS BIGINT),
               CAST(CASE WHEN NOT ? THEN 1 WHEN c.update_action=1 THEN 0 WHEN c.update_action=0 THEN 1 ELSE c.update_action END AS BIGINT),
               CAST(CASE WHEN NOT ? THEN 1 WHEN c.delete_action=1 THEN 0 WHEN c.delete_action=0 THEN 1 ELSE c.delete_action END AS BIGINT),
               c.name,
               (SELECT mk.name FROM main.__msduck_keys mk WHERE mk.object_id=c.referenced_object_id AND mk.kind IN ('PK','UQ')
                  AND list_sort(list_transform(from_json(mk.key_columns,'[\"VARCHAR\"]'),lambda x: lower(x)))
                    =list_sort(list_transform(c.referenced_columns,lambda x: lower(x)))
                ORDER BY mk.kind='PK' DESC,mk.tag LIMIT 1),
               CAST(7 AS BIGINT)
             FROM main.__msduck_constraints c
             JOIN main.__msduck_catalog_tables f ON f.object_id=c.parent_object_id
             JOIN main.__msduck_schemas fs ON fs.schema_id=f.schema_id
             JOIN main.__msduck_catalog_tables p ON p.object_id=c.referenced_object_id
             JOIN main.__msduck_schemas ps ON ps.schema_id=p.schema_id
             CROSS JOIN LATERAL (SELECT unnest(c.columns) AS referencing,unnest(c.referenced_columns) AS referenced,
               unnest(generate_series(1,len(c.columns))) AS ordinal) k
             LEFT JOIN main.__msduck_column_info fc ON fc.object_id=f.object_id AND lower(fc.name)=lower(k.referencing)
             LEFT JOIN main.__msduck_column_info pc ON pc.object_id=p.object_id AND lower(pc.name)=lower(k.referenced)
             WHERE c.type_code='F'
               AND (? IS NULL OR lower(p.name)=lower(?)) AND (? IS NULL OR lower(ps.name)=lower(?))
               AND (? IS NULL OR lower(f.name)=lower(?)) AND (? IS NULL OR lower(fs.name)=lower(?))
             ORDER BY {order}"
        ))?
        .query_map(
            duckdb::params![
                database,
                database,
                rules,
                rules,
                primary,
                primary,
                argument("@pktable_owner"),
                argument("@pktable_owner"),
                foreign,
                foreign,
                argument("@fktable_owner"),
                argument("@fktable_owner"),
            ],
            |row| {
                (0..14)
                    .map(|index| match index {
                        8..=10 | 13 => int(row, index),
                        _ => text(row, index),
                    })
                    .collect()
            },
        )?
        .collect::<duckdb::Result<_>>()?;
    let mut tokens = Vec::new();
    call::result_set(&mut tokens, &FKEYS, &rows)?;
    if !session.nocount {
        call::done_in_proc(&mut tokens, 0x11, 193, rows.len() as u64);
    }
    session.rowcount = rows.len() as u64;
    call::finish(session, &bound, tokens, vec![], 0, variables)
}
