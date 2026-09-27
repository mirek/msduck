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
        })
    }
    pub(crate) fn into_parts(self) -> (BackendConnection, crate::statement_diagnostics::Registry) {
        (self.db, self.diagnostics)
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
        owner.execute_batch("CREATE SCHEMA IF NOT EXISTS dbo")?;
        crate::schema_catalog::register(&owner)?;
        crate::object_catalog::register(&owner)?;
        crate::type_catalog::register(&owner)?;
        crate::declared_columns::register(&owner)?;
        crate::query_catalog::register(&owner)?;
        crate::index_catalog::register(&owner)?;
        crate::index_catalog::sync(&owner)?;
        crate::index_catalog::publish_views(&owner)?;
        Ok(Self {
            owner: Arc::new(Mutex::new(owner)),
            diagnostics,
            tls: None,
            administrator: None,
            max_connections: DEFAULT_MAX_CONNECTIONS,
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
    let mut session = Session::new(db)?;
    session.original_login = authenticated_name;
    let mut rpc = crate::rpc::State::default();
    tds::write_message(&mut stream, &tds::login_response(&login), 4096)?;
    while let Some(message) = tds::read_message(&mut stream, login.packet_size)? {
        let response = if message.status & 2 != 0 {
            let mut out = vec![];
            tds::done(&mut out, 0xfd, 2, 0, 0);
            Ok(out)
        } else if message.status & 0x18 != 0 {
            Err(anyhow::anyhow!("connection reset is not implemented"))
        } else {
            (|| -> Result<Vec<u8>> {
                if matches!(message.kind, 1 | 3 | 14) {
                    let (_, descriptor) = tds::request_headers(&message.payload)?;
                    ensure!(
                        descriptor.is_none_or(|value| value == session.transaction_descriptor),
                        "invalid transaction descriptor for this session"
                    );
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
        tds::write_message(&mut stream, &response, login.packet_size)?;
    }
    // Dropping the connection rolls back any outstanding DuckDB transaction.
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
