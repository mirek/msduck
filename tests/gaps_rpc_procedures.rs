//! RPC requests that name a procedure, and RPC OUTPUT parameters (issue
//! #753). Exact tokens follow the SQL Server 2022 captures in
//! reference/gaps-rpc-procedures.json (docs/gaps-rpc-procedures.md); these
//! requests are encoded by hand, so they also cover forms tedious does not
//! send, such as the fDefaultValue status flag.
use msduck::engine::Session;
use msduck::rpc::State;
use msduck::server::Server;

const COLLATION: [u8; 5] = [0x09, 0x04, 0xd0, 0x00, 0x34];

/// An RPC request body: empty ALL_HEADERS, then the procedure.
fn by_name(name: &str) -> Vec<u8> {
    let mut out = vec![4, 0, 0, 0];
    out.extend((name.encode_utf16().count() as u16).to_le_bytes());
    out.extend(name.encode_utf16().flat_map(u16::to_le_bytes));
    out.extend([0, 0]);
    out
}

fn by_id(id: u16) -> Vec<u8> {
    let mut out = vec![4, 0, 0, 0, 0xff, 0xff];
    out.extend(id.to_le_bytes());
    out.extend([0, 0]);
    out
}

fn header(out: &mut Vec<u8>, name: &str, status: u8) {
    out.push(name.encode_utf16().count() as u8);
    out.extend(name.encode_utf16().flat_map(u16::to_le_bytes));
    out.push(status);
}

fn int(out: &mut Vec<u8>, name: &str, status: u8, value: Option<i32>) {
    header(out, name, status);
    out.extend([0x26, 4]);
    match value {
        Some(value) => {
            out.push(4);
            out.extend(value.to_le_bytes());
        }
        None => out.push(0),
    }
}

fn nvarchar(out: &mut Vec<u8>, name: &str, status: u8, value: &str) {
    header(out, name, status);
    let bytes: Vec<u8> = value.encode_utf16().flat_map(u16::to_le_bytes).collect();
    out.push(0xe7);
    out.extend(8000u16.to_le_bytes());
    out.extend(COLLATION);
    out.extend((bytes.len() as u16).to_le_bytes());
    out.extend(bytes);
}

fn done(id: u8, status: u16, command: u16, count: u64) -> Vec<u8> {
    let mut out = vec![id];
    out.extend(status.to_le_bytes());
    out.extend(command.to_le_bytes());
    out.extend(count.to_le_bytes());
    out
}

fn status(value: i32) -> Vec<u8> {
    let mut out = vec![0x79];
    out.extend(value.to_le_bytes());
    out
}

fn hex(value: &str) -> Vec<u8> {
    (0..value.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&value[i..i + 2], 16).unwrap())
        .collect()
}

fn ending(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

fn session(server: &Server) -> Session {
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    let (_, ok) = session.batch_response(
        "CREATE PROCEDURE p_out @a int, @b int = 5, @c int OUTPUT AS\nBEGIN\n  SET @c = @a + @b;\n  SELECT @a AS a, @b AS b;\n  RETURN 3;\nEND",
        &Default::default(),
        false,
        None,
    );
    assert!(ok);
    session
}

#[test]
fn named_call_returns_status_then_output_value() {
    let server = Server::open(":memory:").unwrap();
    let mut session = session(&server);
    let mut request = by_name("p_out");
    int(&mut request, "@a", 0, Some(1));
    int(&mut request, "@c", 1, None);
    let response = State::default().execute(&mut session, &request).unwrap();
    // Captured "named output": SET, SELECT and RETURN, then RETURNSTATUS 3,
    // RETURNVALUE @c = 6 (ordinal 1) and DONEPROC.
    let tail = ending(&[
        done(0xff, 0x11, 193, 1),
        status(3),
        hex("ac010002400063000100000000000026040406000000"),
        done(0xfe, 0, 224, 0),
    ]);
    assert!(response.ends_with(&tail), "{response:02x?}");
    assert!(response.starts_with(&done(0xff, 0x11, 193, 1)));
}

#[test]
fn default_flag_positional_after_named_and_lowercase_names() {
    let server = Server::open(":memory:").unwrap();
    let mut session = session(&server);
    // fDefaultValue on @b: its default 5 applies.
    let mut request = by_name("P_OUT");
    int(&mut request, "@A", 0, Some(1));
    int(&mut request, "@b", 2, Some(100));
    int(&mut request, "@c", 1, None);
    let response = State::default().execute(&mut session, &request).unwrap();
    assert!(response.ends_with(&ending(&[
        status(3),
        hex("ac020002400063000100000000000026040406000000"),
        done(0xfe, 0, 224, 0),
    ])));
    // A positional parameter after a named one binds by its ordinal
    // (captured "positional after named": @b = 2).
    let mut request = by_name("dbo.p_out");
    int(&mut request, "@a", 0, Some(1));
    int(&mut request, "", 0, Some(2));
    int(&mut request, "@c", 1, None);
    let response = State::default().execute(&mut session, &request).unwrap();
    assert!(response.ends_with(&ending(&[
        status(3),
        hex("ac020002400063000100000000000026040403000000"),
        done(0xfe, 0, 224, 0),
    ])));
}

#[test]
fn call_errors_send_only_the_error_and_doneproc() {
    let server = Server::open(":memory:").unwrap();
    let mut session = session(&server);
    for (name, parameters, number) in [
        ("p_out", vec![("@a", 0, Some(1))], 201),
        (
            "p_out",
            vec![("@a", 0, Some(1)), ("@zz", 0, Some(2)), ("@c", 1, None)],
            8145,
        ),
        ("p_out", vec![("@a", 1, Some(1)), ("@c", 1, None)], 8162),
        ("no_such_proc", vec![], 2812),
        ("p_out; DROP TABLE x", vec![], 2812),
    ] {
        let mut request = by_name(name);
        for (parameter, status, value) in parameters {
            int(&mut request, parameter, status, value);
        }
        let response = State::default().execute(&mut session, &request).unwrap();
        assert_eq!(response[0], 0xaa, "{name}");
        assert_eq!(
            i32::from_le_bytes(response[3..7].try_into().unwrap()),
            number,
            "{name}"
        );
        // The ERROR token, then DONEPROC: no RETURNSTATUS or RETURNVALUE.
        let error = 3 + u16::from_le_bytes([response[1], response[2]]) as usize;
        assert_eq!(&response[error..], done(0xfe, 2, 224, 0), "{name}");
    }
}

#[test]
fn encrypted_parameters_are_refused() {
    let server = Server::open(":memory:").unwrap();
    let mut session = session(&server);
    let mut request = by_name("p_out");
    int(&mut request, "@a", 8, Some(1));
    assert!(State::default().execute(&mut session, &request).is_err());
}

#[test]
fn executesql_output_parameters_return_values_and_input_values_after_failure() {
    let server = Server::open(":memory:").unwrap();
    let mut session = session(&server);
    let mut state = State::default();
    let request = |sql: &str, value: Option<i32>| {
        let mut request = by_id(10);
        nvarchar(&mut request, "@statement", 0, sql);
        nvarchar(&mut request, "@params", 0, "@x int OUTPUT");
        int(&mut request, "@x", 1, value);
        request
    };
    // Captured "sql output input value".
    let response = state
        .execute(&mut session, &request("SET @x = @x + 1", Some(5)))
        .unwrap();
    assert_eq!(
        response,
        ending(&[
            done(0xff, 0x11, 193, 1),
            status(0),
            hex("ac020002400078000100000000000026040406000000"),
            done(0xfe, 0, 224, 0),
        ])
    );
    // Captured "sql output missing table input": the error number is the
    // status and @x returns the value it was sent with.
    let response = state
        .execute(
            &mut session,
            &request(
                "SET @x = 1; SELECT * FROM no_such_table; SET @x = 2",
                Some(5),
            ),
        )
        .unwrap();
    assert!(response.ends_with(&ending(&[
        status(208),
        hex("ac020002400078000100000000000026040405000000"),
        done(0xfe, 2, 224, 0),
    ])));
    // Captured "sql output throw": the batch ends without status or values.
    let response = state
        .execute(
            &mut session,
            &request("SET @x = 1; THROW 50002, 't', 1", None),
        )
        .unwrap();
    assert!(!response.contains(&0xac));
    assert!(response.ends_with(&done(0xfe, 2, 224, 0)));
}

#[test]
fn prepexec_duplicate_key_returns_the_handle_and_output_values() {
    let server = Server::open(":memory:").unwrap();
    let mut session = session(&server);
    let (_, ok) = session.batch_response(
        "CREATE TABLE t (id int CONSTRAINT pk_t PRIMARY KEY); INSERT t VALUES (1)",
        &Default::default(),
        false,
        None,
    );
    assert!(ok);
    let mut state = State::default();
    let mut request = by_id(13);
    int(&mut request, "@handle", 1, None);
    nvarchar(&mut request, "@params", 0, "@v int");
    nvarchar(&mut request, "@stmt", 0, "INSERT INTO t VALUES (@v)");
    int(&mut request, "@v", 0, Some(1));
    let response = state.execute(&mut session, &request).unwrap();
    // Captured "prepexec duplicate parameter".
    assert!(response.ends_with(&ending(&[
        done(0xff, 3, 195, 0),
        status(2627),
        hex("ac0000074000680061006e0064006c0065000100000000000026040401000000"),
        done(0xfe, 0, 224, 0),
    ])));
    // OUTPUT parameters of a prepared statement (captured "prepexec output"
    // and "execute output").
    let mut request = by_id(13);
    int(&mut request, "@handle", 1, None);
    nvarchar(&mut request, "@params", 0, "@x int OUTPUT");
    nvarchar(&mut request, "@stmt", 0, "SET @x = 11");
    int(&mut request, "@x", 1, None);
    let response = state.execute(&mut session, &request).unwrap();
    assert!(response.ends_with(&ending(&[
        status(0),
        hex("ac0000074000680061006e0064006c0065000100000000000026040402000000"),
        hex("ac03000240007800010000000000002604040b000000"),
        done(0xfe, 0, 224, 0),
    ])));
    let mut request = by_id(12);
    int(&mut request, "", 0, Some(2));
    int(&mut request, "", 1, None);
    let response = state.execute(&mut session, &request).unwrap();
    assert!(response.ends_with(&ending(&[
        status(0),
        hex("ac010000010000000000002604040b000000"),
        done(0xfe, 0, 224, 0),
    ])));
}
