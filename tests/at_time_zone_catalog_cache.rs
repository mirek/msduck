//! Concurrent first connection registrations share the validated rule snapshot.
use std::{
    sync::{Arc, Barrier},
    thread,
};

use msduck::{datetimeoffset::DateTimeOffset, engine::Session, server::Server};

fn first_offset(bytes: &[u8]) -> Option<DateTimeOffset> {
    let row = bytes.iter().position(|byte| *byte == 0xd1)?;
    let len = usize::from(*bytes.get(row + 1)?);
    DateTimeOffset::decode(bytes.get(row + 2..row + 2 + len)?, 7).ok()
}

#[test]
fn concurrent_connections_convert_local_and_instant_values() {
    let cases = [
        ("Pacific Standard Time", "-08:00", "04:00:00"),
        ("Central European Standard Time", "+01:00", "13:00:00"),
        ("UTC", "+00:00", "12:00:00"),
        ("Pacific Standard Time", "-08:00", "04:00:00"),
    ];
    let start = Arc::new(Barrier::new(cases.len()));
    let workers: Vec<_> = cases
        .into_iter()
        .map(|(zone, offset, instant_hour)| {
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                let server = Server::open(":memory:").unwrap();
                let mut session = Session::new(server.connection().unwrap()).unwrap();
                for (source, expected) in [
                    (
                        "CAST('2024-01-15T12:00:00' AS DATETIME2(7))",
                        format!("2024-01-15T12:00:00.0000000{offset}"),
                    ),
                    (
                        "CAST('2024-01-15T12:00:00+00:00' AS DATETIMEOFFSET(7))",
                        format!("2024-01-15T{instant_hour}.0000000{offset}"),
                    ),
                ] {
                    let sql = format!("SELECT {source} AT TIME ZONE '{zone}' AS value");
                    let (bytes, ok) =
                        session.batch_response(&sql, &Default::default(), false, None);
                    assert!(ok, "{sql}: {bytes:?}");
                    assert!(bytes.windows(2).any(|pair| pair == [0x2b, 7]), "{sql}");
                    assert_eq!(
                        first_offset(&bytes),
                        Some(DateTimeOffset::parse_iso(&expected).unwrap()),
                        "{sql}"
                    );
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
}
