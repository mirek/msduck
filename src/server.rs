use crate::{engine::Session, tds};
use anyhow::{Result, ensure};
use duckdb::Connection as BackendConnection;
use std::{
    collections::HashMap,
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};
use zeroize::Zeroize;

/// The owner connection keeps an in-memory database alive; each client gets an
/// independent DuckDB connection and transaction context via try_clone().
pub struct Server {
    owner: Arc<Mutex<BackendConnection>>,
    diagnostics: crate::statement_diagnostics::Registry,
    tls: Option<Arc<rustls::ServerConfig>>,
    administrator: Option<Arc<crate::authentication::Administrator>>,
    max_connections: usize,
    databases: Arc<crate::database_catalog::Catalog>,
}

pub const DEFAULT_MAX_CONNECTIONS: usize = 128;
pub const MAX_CONNECTIONS_LIMIT: usize = 1024;

struct Admission {
    active: AtomicUsize,
    limit: usize,
}

impl Admission {
    fn acquire(self: &Arc<Self>) -> Option<Permit> {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < self.limit).then_some(active + 1)
            })
            .ok()
            .map(|_| Permit(Arc::clone(self)))
    }
}

struct Permit(Arc<Admission>);

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A backend connection paired with its database-owned execution services.
/// Cloning through this wrapper retains the matching diagnostic registry.
pub struct Connection {
    db: BackendConnection,
    diagnostics: crate::statement_diagnostics::Registry,
    databases: Arc<crate::database_catalog::Catalog>,
}
impl std::ops::Deref for Connection {
    type Target = BackendConnection;
    fn deref(&self) -> &Self::Target {
        &self.db
    }
}
impl Connection {
    pub fn try_clone(&self) -> duckdb::Result<Self> {
        Ok(Self {
            db: self.db.try_clone()?,
            diagnostics: self.diagnostics.clone(),
            databases: self.databases.clone(),
        })
    }
    /// The server's databases. A fresh connection starts in `master`.
    pub fn databases(&self) -> &Arc<crate::database_catalog::Catalog> {
        &self.databases
    }
    pub(crate) fn into_parts(
        self,
    ) -> (
        BackendConnection,
        crate::statement_diagnostics::Registry,
        Arc<crate::database_catalog::Catalog>,
    ) {
        (self.db, self.diagnostics, self.databases)
    }
}
impl Server {
    pub fn open(path: &str) -> Result<Self> {
        let owner = if path == ":memory:" {
            BackendConnection::open_in_memory()?
        } else {
            BackendConnection::open(path)?
        };
        crate::scalar::register(&owner)?;
        let diagnostics = crate::statement_diagnostics::Registry::default();
        diagnostics.register(&owner)?;
        crate::database_catalog::bootstrap_objects(&owner)?;
        let databases = Arc::new(crate::database_catalog::Catalog::open(&owner, path)?);
        Ok(Self {
            owner: Arc::new(Mutex::new(owner)),
            diagnostics,
            tls: None,
            administrator: None,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            databases,
        })
    }
    pub fn with_tls(mut self, config: Arc<rustls::ServerConfig>) -> Self {
        self.tls = Some(config);
        self
    }
    pub fn with_administrator(
        mut self,
        administrator: crate::authentication::Administrator,
    ) -> Result<Self> {
        ensure!(self.tls.is_some(), "password authentication requires TLS");
        self.administrator = Some(Arc::new(administrator));
        Ok(self)
    }
    pub fn with_max_connections(mut self, limit: usize) -> Result<Self> {
        ensure!(
            (1..=MAX_CONNECTIONS_LIMIT).contains(&limit),
            "max connections must be between 1 and {MAX_CONNECTIONS_LIMIT}"
        );
        self.max_connections = limit;
        Ok(self)
    }
    pub fn serve(self, listener: TcpListener) -> Result<()> {
        let admission = Arc::new(Admission {
            active: AtomicUsize::new(0),
            limit: self.max_connections,
        });
        for stream in listener.incoming() {
            let stream = stream?;
            let Some(permit) = admission.acquire() else {
                // No TDS request has been read, so there is no proven SQL Server
                // diagnostic to send. Drop the socket before allocating a DB
                // connection or worker.
                drop(stream);
                continue;
            };
            let db = self.connection()?;
            let tls = self.tls.clone();
            let administrator = self.administrator.clone();
            thread::Builder::new().spawn(move || {
                let _permit = permit;
                if let Err(error) = serve_connection_configured(stream, db, tls, administrator) {
                    eprintln!("connection ended: {error}");
                }
            })?;
        }
        Ok(())
    }
    pub fn connection(&self) -> Result<Connection> {
        let db = self
            .owner
            .lock()
            .map_err(|_| anyhow::anyhow!("database lock poisoned"))?
            .try_clone()?;
        Ok(Connection {
            db,
            diagnostics: self.diagnostics.clone(),
            databases: self.databases.clone(),
        })
    }
}
pub fn serve_connection(stream: TcpStream, db: Connection) -> Result<()> {
    serve_connection_configured(stream, db, None, None)
}
fn serve_connection_configured(
    mut stream: TcpStream,
    db: Connection,
    tls: Option<Arc<rustls::ServerConfig>>,
    administrator: Option<Arc<crate::authentication::Administrator>>,
) -> Result<()> {
    stream.set_nodelay(true)?;
    let Some(message) = tds::read_message(&mut stream, 4096)? else {
        return Ok(());
    };
    ensure!(message.kind == 0x12, "expected PRELOGIN");
    let policy = if tls.is_some() {
        tds::EncryptionPolicy::Required
    } else {
        tds::EncryptionPolicy::Unsupported
    };
    let response = tds::prelogin_with_policy(&message.payload, policy)?;
    tds::write_message(&mut stream, &response.payload, 4096)?;
    ensure!(
        response.encryption != tds::EncryptionResult::Reject,
        "client and server encryption policies are incompatible"
    );
    if response.encryption == tds::EncryptionResult::Tls {
        let mut encrypted = crate::tls::accept(
            stream,
            tls.ok_or_else(|| anyhow::anyhow!("missing TLS configuration"))?,
        )?;
        serve_login(&mut encrypted, db, administrator.as_deref())
    } else {
        serve_login(&mut stream, db, administrator.as_deref())
    }
}
fn serve_login(
    mut stream: &mut (impl std::io::Read + std::io::Write),
    db: Connection,
    administrator: Option<&crate::authentication::Administrator>,
) -> Result<()> {
    let mut message =
        tds::read_message(&mut stream, 4096)?.ok_or_else(|| anyhow::anyhow!("missing LOGIN7"))?;
    ensure!(message.kind == 0x10, "expected LOGIN7");
    let decoded = tds::login(&message.payload);
    message.payload.zeroize();
    let mut login = match decoded {
        Ok(login) => login,
        Err(_) => {
            let mut out = vec![];
            login_failure(&mut out);
            tds::done(&mut out, 0xfd, 2, 0, 0);
            tds::write_message(&mut stream, &out, 4096)?;
            return Ok(());
        }
    };
    let authenticated_name = match administrator {
        Some(admin) => admin.authenticate(&login.user_name, &login.password),
        None => Some(if login.user_name.is_empty() {
            "sa".into()
        } else {
            login.user_name.clone()
        }),
    };
    // Authentication must not retain the plaintext password for the session.
    drop(std::mem::take(&mut login.password));
    let Some(authenticated_name) = authenticated_name else {
        let mut out = vec![];
        login_failure(&mut out);
        tds::done(&mut out, 0xfd, 2, 0, 0);
        tds::write_message(&mut stream, &out, login.packet_size)?;
        return Ok(());
    };
    // An unused clone supplies fresh sessions for RESETCONNECTION requests.
    let template = db.try_clone()?;
    let mut session = Session::new(db)?;
    // SQL Server fails the login when the requested database cannot be
    // opened, and reports the database it selected in the login response.
    if let Err(error) = session.use_database(&login.database) {
        if error
            .downcast_ref::<msduck_core::diagnostic::SqlError>()
            .is_none_or(|error| error.number != 911)
        {
            return Err(error);
        }
        let mut out = vec![];
        let mut unavailable = msduck_core::diagnostic::SqlError::new(
            4060,
            1,
            format!(
                "Cannot open database \"{}\" requested by the login. The login failed.",
                login.database
            ),
        );
        unavailable.severity = 11;
        tds::sql_error(&mut out, &unavailable);
        login_failure(&mut out);
        tds::done(&mut out, 0xfd, 2, 0, 0);
        tds::write_message(&mut stream, &out, login.packet_size)?;
        return Ok(());
    }
    login.database = session.database().name.clone();
    let login_database = login.database.clone();
    session.original_login = authenticated_name;
    let mut rpc = crate::rpc::State::default();
    tds::write_message(&mut stream, &tds::login_response(&login), 4096)?;
    while let Some(message) = tds::read_message(&mut stream, login.packet_size)? {
        let mut acknowledgement = vec![];
        let response = if message.status & 2 != 0 {
            let mut out = vec![];
            tds::done(&mut out, 0xfd, 2, 0, 0);
            Ok(out)
        } else if message.status & 0x10 != 0 {
            Err(anyhow::anyhow!(
                "RESETCONNECTIONSKIPTRAN is not implemented"
            ))
        } else {
            (|| -> Result<Vec<u8>> {
                let reset = reset_requested(message.kind, message.status)?;
                if matches!(message.kind, 1 | 3 | 14) {
                    let (_, descriptor) = tds::request_headers(&message.payload)?;
                    ensure!(
                        descriptor.is_none_or(|value| value == session.transaction_descriptor),
                        "invalid transaction descriptor for this session"
                    );
                }
                // The headers name the transaction being reset, so reset after
                // validating them and before running the request.
                if reset {
                    reset_session(
                        &template,
                        &login_database,
                        &mut session,
                        &mut rpc,
                        &mut acknowledgement,
                    )?;
                }
                match message.kind {
                    1 => tds::batch_body(&message.payload)
                        .and_then(tds::decode_text)
                        .map(|sql| session.batch(&sql, &HashMap::new(), false)),
                    3 => rpc.execute(&mut session, &message.payload),
                    14 => tds::transaction_request(&message.payload)
                        .and_then(|request| session.transaction_request(request)),
                    6 => {
                        let mut out = vec![];
                        tds::done(&mut out, 0xfd, 0x20, 253, 0);
                        Ok(out)
                    }
                    _ => Err(anyhow::anyhow!(
                        "unsupported TDS message type {}",
                        message.kind
                    )),
                }
            })()
        };
        let response = response.unwrap_or_else(|error| {
            let mut out = vec![];
            crate::engine::emit_error(&mut out, &error);
            tds::done(
                &mut out,
                if message.kind == 3 { 0xfe } else { 0xfd },
                2,
                0,
                0,
            );
            out
        });
        acknowledgement.extend(response);
        tds::write_message(&mut stream, &acknowledgement, login.packet_size)?;
    }
    // Dropping the connection rolls back any outstanding DuckDB transaction.
    Ok(())
}

/// Whether a message asks for RESETCONNECTION (0x08). MS-TDS defines the bit
/// only for SQL batch, RPC and transaction manager requests; on any other
/// message it is rejected before the session is touched.
fn reset_requested(kind: u8, status: u8) -> Result<bool> {
    if status & 0x08 == 0 {
        return Ok(false);
    }
    ensure!(
        matches!(kind, 1 | 3 | 14),
        "RESETCONNECTION is only valid on SQL batch, RPC and transaction manager requests"
    );
    Ok(true)
}

/// RESETCONNECTION: roll back an open transaction, then replace the session
/// with a fresh one on a new backend connection. Dropping the old connection
/// discards connection-scoped settings and temporary objects; prepared handles
/// are released. Only the authenticated login survives. Writes the rollback
/// ENVCHANGE (if any) and the MS-TDS type 18 acknowledgement to `out`.
fn reset_session(
    template: &Connection,
    login_database: &str,
    session: &mut Session,
    rpc: &mut crate::rpc::State,
    out: &mut Vec<u8>,
) -> Result<()> {
    // A reset returns to the login database even if the session changed it.
    let connection = template.try_clone()?;
    let mut fresh = Session::new(connection)?;
    fresh.use_database(login_database)?;
    if session.transactions > 0 {
        // Tell the client its transaction ended, as an explicit ROLLBACK does.
        out.extend(session.rollback_transaction("")?);
    }
    fresh.original_login = std::mem::take(&mut session.original_login);
    *session = fresh;
    *rpc = crate::rpc::State::default();
    // ENVCHANGE, length 3, type 18 (reset completion), empty new and old values.
    out.extend([0xe3, 3, 0, 18, 0, 0]);
    Ok(())
}

fn login_failure(out: &mut Vec<u8>) {
    tds::sql_error(
        out,
        &msduck_core::diagnostic::SqlError {
            number: 18456,
            state: 1,
            severity: 14,
            message: "Login failed.".into(),
            message_utf16: None,
        },
    );
}

#[cfg(test)]
mod reset_tests {
    use super::reset_requested;

    #[test]
    fn reset_applies_only_to_request_messages() {
        for kind in [1, 3, 14] {
            assert!(reset_requested(kind, 0x09).unwrap());
            assert!(!reset_requested(kind, 0x01).unwrap());
        }
        for kind in [6, 7, 0x12, 0x10] {
            assert!(reset_requested(kind, 0x08).is_err());
            assert!(!reset_requested(kind, 0x01).unwrap());
        }
    }
}

#[cfg(test)]
mod diagnostic_context_tests {
    use super::*;

    #[test]
    fn connection_clones_keep_their_database_diagnostics() {
        let server = Server::open(":memory:").unwrap();
        let connection = server.connection().unwrap();
        let first = Session::new(connection.try_clone().unwrap()).unwrap();
        let second = Session::new(connection).unwrap();
        let a = first.diagnostic_scope().unwrap();
        let b = second.diagnostic_scope().unwrap();
        assert!(
            first
                .db
                .query_row(
                    "SELECT __msduck_observe_null(?,true)",
                    [a.ticket().as_slice()],
                    |r| r.get::<_, bool>(0)
                )
                .unwrap()
        );
        assert!(a.null_eliminated());
        assert!(!b.null_eliminated());
        assert!(
            !second
                .db
                .query_row(
                    "SELECT __msduck_observe_null(?,false)",
                    [b.ticket().as_slice()],
                    |r| r.get::<_, bool>(0)
                )
                .unwrap()
        );
        assert!(!b.null_eliminated());

        let other_server = Server::open(":memory:").unwrap();
        let other = Session::new(other_server.connection().unwrap()).unwrap();
        assert!(
            other
                .db
                .query_row(
                    "SELECT __msduck_observe_null(?,true)",
                    [b.ticket().as_slice()],
                    |r| r.get::<_, bool>(0)
                )
                .is_err()
        );
        let stale = *b.ticket();
        drop(b);
        assert!(
            second
                .db
                .query_row(
                    "SELECT __msduck_observe_null(?,true)",
                    [stale.as_slice()],
                    |r| r.get::<_, bool>(0)
                )
                .is_err()
        );
        let fresh = second.diagnostic_scope().unwrap();
        assert!(!fresh.null_eliminated());
    }
}

#[cfg(test)]
mod database_reset_tests {
    use super::*;

    #[test]
    fn reset_returns_to_the_connection_database() {
        let server = Server::open(":memory:").unwrap();
        let template = server.connection().unwrap();
        let databases = template.databases().clone();
        databases.create(&template, "sales").unwrap();
        for login_database in ["master", "sales"] {
            let connection = template.try_clone().unwrap();
            // The session moved to another database before the reset.
            let other = if login_database == "master" {
                "sales"
            } else {
                "master"
            };
            databases.select(&connection, other).unwrap();
            let mut session = Session::new(connection).unwrap();
            let mut rpc = crate::rpc::State::default();
            let mut out = vec![];
            reset_session(&template, login_database, &mut session, &mut rpc, &mut out).unwrap();
            assert_eq!(databases.current(&session.db).unwrap(), login_database);
            let schema: String = session
                .db
                .query_row("SELECT current_schema()", [], |row| row.get(0))
                .unwrap();
            assert_eq!(schema, "dbo");
        }
    }
}
