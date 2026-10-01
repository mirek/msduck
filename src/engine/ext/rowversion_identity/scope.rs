//! SCOPE_IDENTITY() and @@IDENTITY.
//!
//! An INSERT that generates identity values holds a process-wide lock on the
//! table's private sequence while it runs, so the sequence's last value
//! afterwards is the last value this INSERT generated. Sequences are shared
//! by every session of the database and never roll back, as SQL Server's
//! identity allocation.
use crate::engine::Session;
use anyhow::Result;
use sqlparser::ast::*;
use std::{
    collections::HashMap,
    sync::{Condvar, LazyLock, Mutex},
    time::{Duration, Instant},
};

fn numeric(value: Option<i128>) -> Expr {
    Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(Expr::Value(
            match value {
                Some(value) => Value::Number(value.to_string(), false),
                None => Value::Null,
            }
            .into(),
        )),
        data_type: DataType::Numeric(ExactNumberInfo::PrecisionAndScale(38, 0)),
        format: None,
    }
}

/// `SCOPE_IDENTITY()` and `@@IDENTITY` as numeric(38,0) values.
pub(super) fn rewrite(session: &Session, expr: &mut Expr) -> Result<()> {
    let state = &session.ext.rowversion_identity;
    match expr {
        Expr::Identifier(id) if id.value.eq_ignore_ascii_case("@@IDENTITY") => {
            *expr = numeric(state.last);
        }
        Expr::Function(function)
            if function
                .name
                .to_string()
                .eq_ignore_ascii_case("SCOPE_IDENTITY")
                && matches!(&function.args, FunctionArguments::List(list) if list.args.is_empty()) =>
        {
            *expr = numeric(state.scope());
        }
        _ => {}
    }
    Ok(())
}

/// The last value a private identity sequence handed out, if any.
pub(super) fn last_value(db: &duckdb::Connection, sequence: &str) -> Result<Option<i128>> {
    let value: Option<i64> = db
        .query_row(
            "SELECT last_value FROM duckdb_sequences() WHERE database_name=current_database() \
             AND schema_name||'.'||sequence_name=?",
            [sequence],
            |row| row.get(0),
        )
        .map_err(anyhow::Error::from)?;
    Ok(value.map(i128::from))
}

#[derive(Default)]
struct Locks {
    /// Sequence name -> (owning session token, depth).
    held: Mutex<HashMap<String, (u64, usize)>>,
    released: Condvar,
}

static LOCKS: LazyLock<Locks> = LazyLock::new(Locks::default);

/// How long an INSERT waits for another session's INSERT into the same
/// table. Waiting longer could only matter for INSERTs whose triggers insert
/// into each other's tables; they then proceed unserialized.
const WAIT: Duration = Duration::from_secs(10);

/// A held allocation lock; released on drop.
pub(super) struct Lock {
    sequence: Option<String>,
}

impl Drop for Lock {
    fn drop(&mut self) {
        let Some(sequence) = self.sequence.take() else {
            return;
        };
        let mut held = LOCKS.held.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = held.get_mut(&sequence) {
            entry.1 -= 1;
            if entry.1 == 0 {
                held.remove(&sequence);
            }
        }
        LOCKS.released.notify_all();
    }
}

/// Serialize generating INSERTs into one table across sessions. Reentrant
/// for the same session (an INSERT nested in another into the same table).
pub(super) fn lock(sequence: &str, token: u64) -> Lock {
    let deadline = Instant::now() + WAIT;
    let mut held = LOCKS.held.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        match held.get_mut(sequence) {
            None => {
                held.insert(sequence.to_owned(), (token, 1));
                break;
            }
            Some((owner, depth)) if *owner == token => {
                *depth += 1;
                break;
            }
            Some(_) => {
                let now = Instant::now();
                if now >= deadline {
                    return Lock { sequence: None };
                }
                held = LOCKS
                    .released
                    .wait_timeout(held, deadline - now)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
        }
    }
    Lock {
        sequence: Some(sequence.to_owned()),
    }
}
