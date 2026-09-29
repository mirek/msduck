use msduck::{
    engine::{Parameter, Session},
    server::Server,
};
use msduck_core::{types::Type, value::Value};
use std::collections::HashMap;

fn session() -> Session {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    session.batch("CREATE TABLE dbo.bucket(n INT NOT NULL,b INT NULL); INSERT dbo.bucket VALUES(1,NULL),(2,NULL),(3,NULL)",&HashMap::new(),false);
    session
}

// Decode the tested BIGINT metadata/error shape; do not scan arbitrary payload
// bytes for token markers. Client tests cover rows and complete descriptors.
fn error(tokens: &[u8]) -> (bool, i32, u8, u8, String) {
    let mut cursor = msduck_tds::Cursor::new(tokens);
    let first = cursor.u8().unwrap();
    let metadata = first == 0x81;
    let token = if metadata {
        assert_eq!(cursor.u16().unwrap(), 1);
        cursor.u32().unwrap();
        assert_eq!(cursor.u16().unwrap(), 1);
        assert_eq!(cursor.u8().unwrap(), 0x26);
        assert_eq!(cursor.u8().unwrap(), 8);
        let count = cursor.u8().unwrap();
        cursor.text(count as usize).unwrap();
        cursor.u8().unwrap()
    } else {
        first
    };
    assert_eq!(token, 0xaa);
    let length = cursor.u16().unwrap() as usize;
    let bytes = cursor.take(length).unwrap();
    let mut diagnostic = msduck_tds::Cursor::new(bytes);
    let number = diagnostic.u32().unwrap() as i32;
    let state = diagnostic.u8().unwrap();
    let severity = diagnostic.u8().unwrap();
    let count = diagnostic.u16().unwrap();
    let message = diagnostic.text(count as usize).unwrap();
    (metadata, number, state, severity, message)
}

#[test]
fn constant_invalid_counts_bind_before_metadata_even_for_empty_sources() {
    let mut session = session();
    for value in [
        "NULL",
        "CAST(NULL AS INT)",
        "CAST(NULL AS BIGINT)",
        "0",
        "-1",
    ] {
        for filter in ["", " WHERE 1=0"] {
            let tokens = session.batch(
                &format!("SELECT NTILE({value}) OVER(ORDER BY n) AS tile FROM dbo.bucket{filter}"),
                &HashMap::new(),
                false,
            );
            let (metadata, number, state, severity, message) = error(&tokens);
            assert!(!metadata, "{value}{filter}");
            assert_eq!((number, state, severity), (4116, 1, 15));
            assert_eq!(
                message,
                "The function 'ntile' takes only a positive int or bigint expression as its input."
            );
        }
    }
    let tokens = session.batch(
        "SELECT NTILE(b) OVER(ORDER BY n) AS tile FROM dbo.bucket",
        &HashMap::new(),
        false,
    );
    let (metadata, number, state, severity, message) = error(&tokens);
    assert!(!metadata);
    assert_eq!((number, state, severity), (4195, 1, 15));
    assert_eq!(
        message,
        "The reference to column \"b\" is not allowed in an argument to the NTILE function. Only references to columns at an outer scope or standalone expressions and subqueries are allowed here."
    );
}

#[test]
fn dynamic_counts_keep_metadata_and_recover_without_compile_value_leaks() {
    let mut session = session();
    let sql = "SELECT NTILE(@b) OVER(ORDER BY n) AS tile FROM dbo.bucket ORDER BY n";
    session
        .validate_prepared_sql(sql, &[("@b".into(), Type::Int)])
        .unwrap();
    for value in [Value::Null, Value::Int(0), Value::Int(-1), Value::Null] {
        let parameters = HashMap::from([(
            "@b".into(),
            Parameter {
                value,
                data_type: Type::Int,
            },
        )]);
        let (metadata, number, state, severity, message) =
            error(&session.prepared_batch(sql, &parameters));
        assert!(metadata);
        assert_eq!((number, state, severity), (4116, 1, 15));
        assert!(!message.contains("Invalid Input Error:"));
        let valid = HashMap::from([(
            "@b".into(),
            Parameter {
                value: Value::Int(2),
                data_type: Type::Int,
            },
        )]);
        let tokens = session.prepared_batch(sql, &valid);
        assert_eq!(tokens[0], 0x81);
    }
    let tokens = session.batch(
        "SELECT NTILE((SELECT MAX(b) FROM dbo.bucket)) OVER(ORDER BY n) AS tile FROM dbo.bucket",
        &HashMap::new(),
        false,
    );
    assert_eq!(error(&tokens).0, true);
    let tokens=session.batch("THROW 50001,'The function ''ntile'' takes only a positive int or bigint expression as its input.',7",&HashMap::new(),false);
    let (_, number, state, severity, _) = error(&tokens);
    assert_eq!((number, state, severity), (50001, 7, 16));
}
