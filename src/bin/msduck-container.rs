//! Container launcher that accepts the Microsoft SQL Server image environment.
//! Unlike the development CLI it listens on all interfaces by default, so it
//! always requires TLS and the `sa` administrator password.
use anyhow::{Context, Result, bail, ensure};
use msduck::{authentication::Administrator, server::Server};
use std::{
    env,
    net::{IpAddr, SocketAddr, TcpListener},
    path::{Path, PathBuf},
};

/// Settings this launcher interprets; any other `MSSQL_*` or `ACCEPT_EULA`
/// value is accepted for compatibility and reported as having no effect.
const HANDLED: &[&str] = &[
    "MSSQL_SA_PASSWORD",
    "SA_PASSWORD",
    "MSSQL_TCP_PORT",
    "MSSQL_DATA_DIR",
    "MSSQL_IP_ADDRESS",
];

fn main() -> Result<()> {
    if env::args().len() > 1 {
        bail!(
            "msduck-container takes no arguments; configure it with MSSQL_* environment variables"
        );
    }
    let mut ignored: Vec<_> = env::vars_os()
        .filter_map(|(name, _)| name.into_string().ok())
        .filter(|name| {
            (name.starts_with("MSSQL_") || name == "ACCEPT_EULA")
                && !HANDLED.contains(&name.as_str())
        })
        .collect();
    ignored.sort();
    for name in ignored {
        eprintln!("msduck: accepted {name}; it has no effect on msduck");
    }

    let password = env::var("MSSQL_SA_PASSWORD")
        .or_else(|_| env::var("SA_PASSWORD"))
        .context("the sa password must be set with MSSQL_SA_PASSWORD (or SA_PASSWORD)")?;
    check_policy(&password)?;

    let port: u16 = setting("MSSQL_TCP_PORT", "1433")
        .parse()
        .ok()
        .filter(|port| *port != 0)
        .context("MSSQL_TCP_PORT must be a port number between 1 and 65535")?;
    let ip: IpAddr = setting("MSSQL_IP_ADDRESS", "0.0.0.0")
        .parse()
        .context("MSSQL_IP_ADDRESS must be an IP address")?;
    let data = PathBuf::from(setting("MSSQL_DATA_DIR", "/var/opt/mssql/data"));
    let secrets = PathBuf::from(setting("MSDUCK_SECRETS_DIR", "/var/opt/mssql/secrets"));
    let cert = PathBuf::from(env::var("MSDUCK_TLS_CERT").context("MSDUCK_TLS_CERT is not set")?);
    let key = PathBuf::from(env::var("MSDUCK_TLS_KEY").context("MSDUCK_TLS_KEY is not set")?);

    std::fs::create_dir_all(&data).context("create MSSQL_DATA_DIR")?;
    std::fs::create_dir_all(&secrets).context("create secrets directory")?;
    let hashed = msduck::authentication::credential_file("sa", &password)?;
    drop(password);

    let tls = msduck::tls::load(&cert, &key)?;
    let database = data.join("msduck.duckdb");
    let database = database
        .to_str()
        .context("MSSQL_DATA_DIR must be valid UTF-8")?;
    // Acquire the database lock and the port before publishing the credential:
    // a running server sharing this volume reloads it for every login, so a
    // replacement that cannot start must not change its password.
    let server = Server::open(database)?.with_tls(tls);
    let listener = TcpListener::bind(SocketAddr::new(ip, port))?;
    let credentials = secrets.join("msduck-admin.json");
    write_private(&credentials, &hashed)?;
    let server = server.with_administrator(Administrator::load(&credentials)?)?;
    eprintln!(
        "msduck listening on {} (required TLS, sa authentication, database {database})",
        listener.local_addr()?
    );
    // Readiness lines that tooling written for the SQL Server image waits for.
    eprintln!(
        "Recovery is complete. This is an informational message only. No user action is required."
    );
    eprintln!(
        "SQL Server is now ready for client connections. This is an informational message; no user action is required."
    );
    server.serve(listener)
}

fn setting(name: &str, default: &str) -> String {
    env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.into())
}

/// SQL Server's password policy for `sa`: at least eight characters from at
/// least three of uppercase, lowercase, digit and symbol classes.
fn check_policy(password: &str) -> Result<()> {
    ensure!(
        password.chars().count() >= 8,
        "the sa password does not meet SQL Server password policy requirements: it must be at least 8 characters"
    );
    let classes = [
        password.chars().any(char::is_uppercase),
        password.chars().any(char::is_lowercase),
        password.chars().any(|c| c.is_ascii_digit()),
        password.chars().any(|c| !c.is_alphanumeric()),
    ];
    ensure!(
        classes.iter().filter(|present| **present).count() >= 3,
        "the sa password does not meet SQL Server password policy requirements: it must contain characters from three of uppercase letters, lowercase letters, digits and symbols"
    );
    Ok(())
}

/// Atomically replace `path` with owner-only contents.
fn write_private(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;
    let temporary = path.with_extension("json.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options
        .open(&temporary)
        .context("write administrator credentials")?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&temporary, path).context("write administrator credentials")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::check_policy;

    #[test]
    fn sa_password_follows_sql_server_policy() {
        for accepted in [
            "Duckdb123",
            "duck!db123",
            "DUCK!DB12",
            "Duck db!x",
            "Kačka🦆12",
        ] {
            assert!(check_policy(accepted).is_ok(), "{accepted}");
        }
        for rejected in ["Duck!1", "duckdbduck", "duckdb123", "DUCKDB!!!", ""] {
            assert!(check_policy(rejected).is_err(), "{rejected}");
        }
    }
}
