use anyhow::{Result, ensure};
use msduck::server::Server;
use std::net::TcpListener;
fn main() -> Result<()> {
    let mut address = "127.0.0.1:1433".to_string();
    let mut database = ":memory:".to_string();
    let mut tls_cert = None;
    let mut tls_key = None;
    let mut administrator_file = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => {
                address = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--listen requires an address"))?
            }
            "--database" => {
                database = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--database requires a path"))?
            }
            "--tls-cert" => {
                tls_cert = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--tls-cert requires a PEM path"))?,
                )
            }
            "--tls-key" => {
                tls_key = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--tls-key requires a PEM path"))?,
                )
            }
            "--admin-credentials" => {
                administrator_file =
                    Some(args.next().ok_or_else(|| {
                        anyhow::anyhow!("--admin-credentials requires a JSON path")
                    })?)
            }
            "--help" | "-h" => {
                println!(
                    "msduck [--listen 127.0.0.1:1433] [--database :memory:|path]\nDevelopment server: loopback only; SQL logins require --admin-credentials for authentication.\nOptional required TLS 1.2: --tls-cert chain.pem --tls-key key.pem\nBootstrap SQL administrator: --admin-credentials credentials.json (requires TLS)"
                );
                return Ok(());
            }
            _ => anyhow::bail!("unknown argument {arg}"),
        }
    }
    let tls = match (tls_cert, tls_key) {
        (Some(cert), Some(key)) => Some(msduck::tls::load(
            std::path::Path::new(&cert),
            std::path::Path::new(&key),
        )?),
        (None, None) => None,
        _ => anyhow::bail!("--tls-cert and --tls-key must be supplied together"),
    };
    ensure!(
        administrator_file.is_none() || tls.is_some(),
        "password authentication requires TLS"
    );
    let administrator = administrator_file
        .map(|path| msduck::authentication::Administrator::load(std::path::Path::new(&path)))
        .transpose()?;
    let listener = TcpListener::bind(&address)?;
    ensure!(
        listener.local_addr()?.ip().is_loopback(),
        "development listener must use a loopback address"
    );
    let mut server = Server::open(&database)?;
    let mode = if let Some(tls) = tls {
        server = server.with_tls(tls);
        "required TLS development mode"
    } else {
        "plaintext development mode"
    };
    if let Some(administrator) = administrator {
        server = server.with_administrator(administrator)?;
    }
    eprintln!("msduck listening on {} ({mode})", listener.local_addr()?);
    server.serve(listener)
}
