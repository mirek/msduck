//! The session values that stored definitions read, as DuckDB variables of
//! the session's own connection.
//!
//! DuckDB binds `getvariable` when it binds a statement, in the connection
//! that runs it. A column DEFAULT that reads these variables therefore sees
//! the inserting session's values. The engine refreshes them before every
//! batch and statement; only changed variables are written.
use crate::engine::Session;
use msduck_sql::dialect::ext::computed::session::{
    APP, HOST, LOGIN, context_value, context_variables,
};
use std::collections::BTreeMap;

#[derive(Default)]
pub(crate) struct State {
    /// LOGIN7 host and application names, read once from the session
    /// registry. RESETCONNECTION creates a new session and state.
    client: Option<(Option<String>, Option<String>)>,
    /// Variables as last written to the connection.
    written: BTreeMap<String, String>,
}

fn client(session: &Session) -> (Option<String>, Option<String>) {
    session
        .db
        .query_row(
            "SELECT host_name, program_name FROM __msduck_sessions() WHERE session_id = ?",
            [session.process.spid()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap_or((None, None))
}

/// The variables the session's current values call for.
fn wanted(session: &Session, host: Option<&str>, app: Option<&str>) -> BTreeMap<String, String> {
    let mut wanted = BTreeMap::new();
    wanted.insert(LOGIN.to_owned(), session.original_login.clone());
    if let Some(host) = host {
        wanted.insert(HOST.to_owned(), host.to_owned());
    }
    if let Some(app) = app {
        wanted.insert(APP.to_owned(), app.to_owned());
    }
    for (key, value) in session.session_context.entries() {
        if let Some((kind, text)) = context_value(value) {
            let (value, kind_variable) = context_variables(key);
            wanted.insert(value, text);
            wanted.insert(kind_variable, kind.to_owned());
        }
    }
    wanted
}

/// Write changed variables and reset removed ones. A failure (for example
/// in a transaction DuckDB already aborted) leaves the variable to be
/// written again next time; it never fails the user's statement.
pub(super) fn sync(session: &mut Session) {
    let (host, app) = match &session.ext.computed.variables.client {
        Some(client) => client.clone(),
        None => {
            let client = client(session);
            session.ext.computed.variables.client = Some(client.clone());
            client
        }
    };
    let wanted = wanted(session, host.as_deref(), app.as_deref());
    let state = &mut session.ext.computed.variables;
    if wanted == state.written {
        return;
    }
    let db = &session.db;
    // Names are generated from fixed prefixes and hexadecimal digits; the
    // values are bound.
    state.written.retain(|name, _| {
        wanted.contains_key(name) || db.execute_batch(&format!("RESET VARIABLE {name}")).is_err()
    });
    for (name, value) in wanted {
        if state.written.get(&name) == Some(&value) {
            continue;
        }
        let written = db
            .prepare(&format!("SET VARIABLE {name} = CAST(? AS VARCHAR)"))
            .and_then(|mut statement| statement.execute([&value]));
        if written.is_ok() {
            state.written.insert(name, value);
        } else {
            state.written.remove(&name);
        }
    }
}
