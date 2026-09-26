//! Path-import the production adapter to inject a short deadline without
//! changing the server's public API or its 15-second production budget.
use std::{
    io::{self, Write},
    net::{TcpListener, TcpStream},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

mod tds {
    pub use msduck::tds::{MAX_MESSAGE, read_message};

    // The trickle never completes one TDS packet, so the response writer must
    // remain unreachable. The production adapter itself is imported unchanged.
    pub(crate) fn write_message_kind(
        _: &mut impl std::io::Write,
        _: &[u8],
        _: usize,
        _: u8,
    ) -> anyhow::Result<()> {
        unreachable!("incomplete handshake cannot emit a TLS response")
    }
}

#[allow(dead_code)]
#[path = "../src/tls.rs"]
mod tls;

#[test]
fn trickled_prelogin_tls_packet_cannot_extend_the_handshake_budget() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    let sender = thread::spawn(move || {
        let mut packet = vec![0u8; 64];
        packet[0] = 0x12;
        packet[1] = 1;
        packet[3] = 64;
        packet[6] = 1;
        for byte in packet {
            if client.write_all(&[byte]).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(35));
        }
    });

    let start = Instant::now();
    // No certificate is needed: the trickle never completes the first TDS
    // packet, let alone a TLS ClientHello that could select one.
    let config = Arc::new(
        rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS12])
        .unwrap()
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(rustls::server::ResolvesServerCertUsingSni::new())),
    );
    let error = match tls::accept_with_deadline(server, config, Duration::from_millis(180)) {
        Err(error) => error,
        Ok(_) => panic!("trickled TLS handshake outlived the deadline"),
    };
    let elapsed = start.elapsed();
    assert!(
        matches!(
            error.downcast_ref::<io::Error>().map(io::Error::kind),
            Some(io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock)
        ),
        "unexpected handshake error: {error:#}"
    );
    assert!(elapsed < Duration::from_secs(1), "trickle took {elapsed:?}");
    sender.join().unwrap();
}
