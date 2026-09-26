use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use tiberius::{AuthMethod, Client, Config, EncryptionLevel};
use tokio_util::compat::TokioAsyncWriteCompatExt;

struct ServerProcess {
    child: Child,
    address: SocketAddr,
}

impl ServerProcess {
    fn start(limit: usize) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_msduck"))
            .args([
                "--listen",
                "127.0.0.1:0",
                "--max-connections",
                &limit.to_string(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start server");
        let stderr = child.stderr.take().expect("server stderr");
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            let result = reader.read_line(&mut line).map(|_| line);
            let _ = sender.send(result);
            // Continue draining diagnostics so a long-running server cannot
            // block on a full stderr pipe during this test.
            std::io::copy(&mut reader, &mut std::io::sink()).ok();
        });
        let line = receiver
            .recv_timeout(Duration::from_secs(30))
            .expect("server startup deadline")
            .expect("read server address");
        let address = line
            .strip_prefix("msduck listening on ")
            .and_then(|tail| tail.split_once(' '))
            .expect("listening log line")
            .0
            .parse()
            .expect("socket address");
        Self { child, address }
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn prelogin_idle(address: SocketAddr) -> TcpStream {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(3)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let payload = [
        0x00, 0x00, 0x0b, 0x00, 0x06, 0x01, 0x00, 0x11, 0x00, 0x01, 0xff, 0x10, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00,
    ];
    let length = (payload.len() + 8) as u16;
    stream
        .write_all(&[
            0x12,
            0x01,
            length.to_be_bytes()[0],
            length.to_be_bytes()[1],
            0,
            0,
            1,
            0,
        ])
        .unwrap();
    stream.write_all(&payload).unwrap();
    let mut header = [0; 8];
    stream
        .read_exact(&mut header)
        .expect("admitted PRELOGIN response");
    assert_eq!(header[0], 4);
    let length = u16::from_be_bytes([header[2], header[3]]) as usize;
    assert!(length > 8);
    let mut response = vec![0; length - 8];
    stream.read_exact(&mut response).unwrap();
    assert!(response.len() > 22, "truncated PRELOGIN response");
    assert_eq!(response[22], 2); // Existing plaintext encryption policy.
    stream
}

enum QueryFailure {
    Rejected(String),
    Stalled,
}

fn query_with_deadline(address: SocketAddr) -> Result<Option<i32>, QueryFailure> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = (|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .build()
                .map_err(|error| error.to_string())?;
            runtime.block_on(async {
                let mut config = Config::new();
                config.host("127.0.0.1");
                config.port(address.port());
                config.authentication(AuthMethod::sql_server("sa", "development"));
                config.encryption(EncryptionLevel::NotSupported);
                let stream = tokio::net::TcpStream::connect(address)
                    .await
                    .map_err(|error| error.to_string())?;
                let mut client = Client::connect(config, stream.compat_write())
                    .await
                    .map_err(|error| error.to_string())?;
                let rows = client
                    .simple_query("SELECT 42 AS answer")
                    .await
                    .map_err(|error| error.to_string())?
                    .into_first_result()
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(rows[0].get::<i32, _>(0))
            })
        })();
        let _ = sender.send(result);
    });
    match receiver.recv_timeout(Duration::from_secs(3)) {
        Ok(result) => result.map_err(QueryFailure::Rejected),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(QueryFailure::Stalled),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(QueryFailure::Rejected("query worker exited".into()))
        }
    }
}

#[test]
fn saturated_handshake_closes_socket_then_releases_for_query() {
    let server = ServerProcess::start(1);
    let idle = prelogin_idle(server.address);
    let mut rejected = TcpStream::connect_timeout(&server.address, Duration::from_secs(3)).unwrap();
    rejected
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut byte = [0];
    match rejected.read(&mut byte) {
        Ok(0) => {}
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
        other => panic!("expected overload close, got {other:?}"),
    }
    drop(idle);

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match query_with_deadline(server.address) {
            Ok(value) => {
                assert_eq!(value, Some(42));
                return;
            }
            Err(QueryFailure::Rejected(error)) => {
                assert!(
                    Instant::now() < deadline,
                    "permit was not released: {error}"
                );
            }
            Err(QueryFailure::Stalled) => {
                panic!("login/query stalled after admission");
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn invalid_cli_limits_fail_before_listening() {
    for value in ["0", "1025", "-1", "many"] {
        let output = Command::new(env!("CARGO_BIN_EXE_msduck"))
            .args(["--max-connections", value])
            .output()
            .unwrap();
        assert!(!output.status.success(), "accepted {value}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("--max-connections"),
            "missing limit diagnostic for {value}"
        );
    }
    let output = Command::new(env!("CARGO_BIN_EXE_msduck"))
        .arg("--max-connections")
        .output()
        .unwrap();
    assert!(!output.status.success());
}
