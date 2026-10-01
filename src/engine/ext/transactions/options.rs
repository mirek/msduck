//! The session's transaction isolation level and DBCC USEROPTIONS.
//!
//! SQL Server accepts five isolation levels. DuckDB has exactly one: every
//! transaction reads a snapshot taken when it starts, and a write that
//! conflicts with a concurrent committed write fails. msduck records and
//! reports the level the client chose, but runs every level as DuckDB
//! snapshot isolation; it takes no locks. docs/gaps-transactions.md maps
//! each level.
use super::super::super::{Execution, Session};
use anyhow::{Result, bail};
use sqlparser::ast::{TransactionIsolationLevel, TransactionMode};

pub(super) const READ_COMMITTED: u8 = 2;

/// Set the session's isolation level (TDS numbering, 1-5).
pub(super) fn set(session: &mut Session, isolation: u8) {
    session.ext.transactions.isolation = isolation;
    publish(session);
}

/// Show the session's level in `sys.dm_exec_sessions`.
pub(super) fn publish(session: &Session) {
    session
        .process
        .set_isolation(i16::from(session.ext.transactions.isolation));
}

/// A transaction began: a transaction-manager request selects a level
/// (1-5) that stays the session's level; 0 keeps the current one.
pub(super) fn begin_request(session: &mut Session, isolation: u8) {
    if (1..=5).contains(&isolation) {
        set(session, isolation);
    }
}

/// A batch starts. SQL Server restores the caller's level when an RPC
/// (`sp_executesql`, prepared execution) or a procedure body returns, so
/// those batches remember it; an ordinary SQL batch keeps its changes.
pub(super) fn batch_begin(session: &mut Session, rpc: bool) {
    let saved = rpc.then_some(session.ext.transactions.isolation);
    session.ext.transactions.batch_isolation.push(saved);
}

pub(super) fn batch_end(session: &mut Session) {
    if let Some(Some(isolation)) = session.ext.transactions.batch_isolation.pop()
        && isolation != session.ext.transactions.isolation
    {
        set(session, isolation);
    }
}

/// `SET TRANSACTION ISOLATION LEVEL level`.
pub(super) fn set_statement(session: &mut Session, modes: &[TransactionMode]) -> Result<Execution> {
    let [TransactionMode::IsolationLevel(level)] = modes else {
        bail!(
            "unsupported SET TRANSACTION mode: only SET TRANSACTION ISOLATION LEVEL is supported"
        );
    };
    set(
        session,
        match level {
            TransactionIsolationLevel::ReadUncommitted => 1,
            TransactionIsolationLevel::ReadCommitted => 2,
            TransactionIsolationLevel::RepeatableRead => 3,
            TransactionIsolationLevel::Serializable => 4,
            TransactionIsolationLevel::Snapshot => 5,
        },
    );
    Ok(Execution::statement(Vec::new(), None, 0))
}

/// DBCC USEROPTIONS, following the captured SQL Server rows: options that
/// are OFF are omitted, and READ COMMITTED reads "read committed snapshot"
/// in a database with READ_COMMITTED_SNAPSHOT ON.
pub(super) fn user_options(session: &mut Session, no_infomsgs: bool) -> Result<Execution> {
    let read_committed_snapshot: bool = session
        .db
        .query_row(
            "SELECT coalesce(max(is_read_committed_snapshot_on::INTEGER), 0) = 1 FROM sys.databases WHERE database_id = ?",
            [session.database.database_id],
            |row| row.get(0),
        )
        .unwrap_or(false);
    let isolation = match session.ext.transactions.isolation {
        1 => "read uncommitted",
        2 if read_committed_snapshot => "read committed snapshot",
        2 => "read committed",
        3 => "repeatable read",
        4 => "serializable",
        _ => "snapshot",
    };
    let datefirst = session.datefirst.to_string();
    let mut rows: Vec<(&str, &str)> = vec![
        ("textsize", "2147483647"),
        ("language", "us_english"),
        ("dateformat", "mdy"),
        ("datefirst", &datefirst),
        ("lock_timeout", "-1"),
        ("quoted_identifier", "SET"),
        ("arithabort", "SET"),
    ];
    if session.nocount {
        rows.push(("nocount", "SET"));
    }
    rows.push(("ansi_null_dflt_on", "SET"));
    if session.xact_abort {
        rows.push(("xact_abort", "SET"));
    }
    if session.ansi_warnings {
        rows.push(("ansi_warnings", "SET"));
    }
    rows.extend([
        ("ansi_padding", "SET"),
        ("ansi_nulls", "SET"),
        ("concat_null_yields_null", "SET"),
        ("isolation level", isolation),
    ]);
    let values = rows
        .iter()
        .enumerate()
        .map(|(index, (option, value))| format!("({index}, N'{option}', N'{value}')"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT CAST(o AS NVARCHAR(128)) AS [Set Option], CAST(v AS NVARCHAR(46)) AS [Value] FROM (VALUES {values}) AS u(n, o, v) ORDER BY n"
    );
    let [query] = <[_; 1]>::try_from(msduck_sql::batch::parse(&sql)?)
        .map_err(|_| anyhow::anyhow!("DBCC USEROPTIONS query must be one statement"))?;
    let mut result = super::super::reenter(session, "transactions", |session| {
        session.execute(query, &mut Default::default())
    })?;
    if !no_infomsgs {
        // SQL Server sends this after the rows' DONE; msduck sends it before
        // the statement's single DONE (docs/gaps-transactions.md).
        crate::tds::diagnostic_utf16(
            &mut result.tokens,
            crate::tds::DiagnosticKind::Information,
            0,
            1,
            2528,
            &"DBCC execution completed. If DBCC printed error messages, contact your system administrator."
                .encode_utf16()
                .collect::<Vec<_>>(),
        );
    }
    Ok(result)
}
