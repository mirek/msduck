//! TLS and socket effects stay outside the deterministic protocol crate.
use anyhow::{Context, Result, ensure};
use rustls::{
    ServerConfig, ServerConnection, StreamOwned,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
};
use std::{net::TcpStream, path::Path, sync::Arc, time::Duration};

pub fn load(cert: &Path, key: &Path) -> Result<Arc<ServerConfig>> {
    let certificates = CertificateDer::pem_file_iter(cert)
        .context("read TLS certificate chain")?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("decode TLS certificate chain")?;
    ensure!(!certificates.is_empty(), "TLS certificate chain is empty");
    let key = PrivateKeyDer::from_pem_file(key).context("read TLS private key")?;
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS12])?
            .with_no_client_auth()
            .with_single_cert(certificates, key)
            .context("configure TLS certificate and key")?;
    Ok(Arc::new(config))
}

/// Handshake records are PRELOGIN-wrapped, including the final server flight.
/// Only after flushing that flight do ordinary encrypted TDS records begin.
pub(crate) fn accept(
    mut socket: TcpStream,
    config: Arc<ServerConfig>,
) -> Result<StreamOwned<ServerConnection, TcpStream>> {
    socket.set_read_timeout(Some(Duration::from_secs(15)))?;
    socket.set_write_timeout(Some(Duration::from_secs(15)))?;
    let mut tls = ServerConnection::new(config)?;
    let mut received = 0usize;
    while tls.is_handshaking() {
        let message =
            crate::tds::read_message(&mut socket, 4096)?.context("EOF during TLS handshake")?;
        ensure!(message.kind == 0x12, "expected TLS handshake in PRELOGIN");
        ensure!(!message.payload.is_empty(), "empty TLS handshake message");
        received += message.payload.len();
        ensure!(
            received <= crate::tds::MAX_MESSAGE,
            "TLS handshake too large"
        );
        let mut input = message.payload.as_slice();
        while !input.is_empty() {
            ensure!(
                tls.read_tls(&mut input)? > 0,
                "TLS handshake made no progress"
            );
            tls.process_new_packets().context("TLS handshake failed")?;
        }
        let mut output = Vec::new();
        while tls.wants_write() {
            tls.write_tls(&mut output)?;
        }
        if !output.is_empty() {
            crate::tds::write_message_kind(&mut socket, &output, 4096, 0x12)?;
        }
    }
    socket.set_read_timeout(None)?;
    socket.set_write_timeout(None)?;
    Ok(StreamOwned::new(tls, socket))
}
