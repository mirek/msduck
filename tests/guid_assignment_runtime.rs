use msduck::{engine::Session, server::Server};
const GUID: &str = "00112233-4455-6677-8899-aabbccddeeff";

fn run(session: &mut Session, sql: &str) -> (Vec<u8>, bool) {
    session.batch_response(sql, &Default::default(), false, None)
}
fn ok(session: &mut Session, sql: &str) {
    let (tokens, success) = run(session, sql);
    assert!(success, "{sql}: {tokens:?}");
}
fn ids(session: &Session) -> Vec<i32> {
    session
        .db
        .prepare("SELECT id FROM dbo.guid_runtime ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap()
}

#[test]
fn guid_assignment_character_parameters_nulls_and_updates_use_core_rules() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    ok(
        &mut session,
        "CREATE TABLE dbo.guid_runtime(id INT,g UNIQUEIDENTIFIER,s VARCHAR(1),u NVARCHAR(3))",
    );
    ok(
        &mut session,
        &format!(
            "DECLARE @g NVARCHAR(100)=N'{{{GUID}}}EXTRA'; INSERT dbo.guid_runtime VALUES(1,@g,'x',N'🦆'),(2,NULL,NULL,NULL); UPDATE dbo.guid_runtime SET g='{GUID}EXTRA' WHERE id=1"
        ),
    );
    let values: Vec<(i32, Option<String>, Option<String>, Option<Vec<u8>>)> = session
        .db
        .prepare(
            "SELECT id,CAST(g AS VARCHAR),s,u.__msduck_utf16le FROM dbo.guid_runtime ORDER BY id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    assert_eq!(
        values,
        vec![
            (
                1,
                Some(GUID.into()),
                Some("x".into()),
                Some(vec![0x3e, 0xd8, 0x86, 0xdd])
            ),
            (2, None, None, None)
        ]
    );
}

#[test]
fn malformed_multirow_guid_insert_and_update_have_no_partial_effects() {
    for write in [
        format!("INSERT dbo.guid_runtime VALUES(2,'{GUID}'),(3,'bad')"),
        format!("UPDATE dbo.guid_runtime SET g=CASE WHEN id=1 THEN '{GUID}EXTRA' ELSE 'bad' END"),
    ] {
        let server = Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        ok(
            &mut session,
            &format!(
                "CREATE TABLE dbo.guid_runtime(id INT,g UNIQUEIDENTIFIER); INSERT dbo.guid_runtime VALUES(1,'{GUID}'),(4,NULL)"
            ),
        );
        assert!(!run(&mut session, &write).1);
        assert_eq!(session.last_error, 8169, "{write}");
        assert_eq!(ids(&session), vec![1, 4]);
        let value: Option<String> = session
            .db
            .query_row(
                "SELECT CAST(g AS VARCHAR) FROM dbo.guid_runtime WHERE id=4",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(value, None, "failed UPDATE must preserve NULL");
    }
}

#[test]
fn uncaught_guid_failure_rolls_back_prior_explicit_writes_with_xact_abort_off() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    ok(
        &mut session,
        &format!(
            "CREATE TABLE dbo.guid_runtime(id INT,g UNIQUEIDENTIFIER); SET XACT_ABORT OFF; BEGIN TRAN; INSERT dbo.guid_runtime VALUES(20,'{GUID}')"
        ),
    );
    assert_eq!(session.transactions, 1);
    assert!(!run(&mut session, "INSERT dbo.guid_runtime VALUES(21,'bad')").1);
    assert_eq!(session.last_error, 8169);
    assert_eq!(session.transactions, 0);
    assert_eq!(ids(&session), Vec::<i32>::new());
    assert!(!run(&mut session, "COMMIT").1);
    assert_eq!(session.last_error, 3902);
    ok(
        &mut session,
        &format!("BEGIN TRAN; INSERT dbo.guid_runtime VALUES(22,'{GUID}EXTRA'); COMMIT"),
    );
    assert_eq!(ids(&session), vec![22]);
}

#[test]
fn caught_guid_failure_allows_reads_then_rejects_commit_and_rolls_back_at_batch_end() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    ok(
        &mut session,
        "CREATE TABLE dbo.guid_runtime(id INT,g UNIQUEIDENTIFIER); SET XACT_ABORT OFF",
    );
    let (tokens, success) = run(
        &mut session,
        &format!(
            "BEGIN TRAN; INSERT dbo.guid_runtime VALUES(30,'{GUID}'); BEGIN TRY INSERT dbo.guid_runtime VALUES(31,'bad'); END TRY BEGIN CATCH SELECT ERROR_NUMBER(),ERROR_STATE(),@@TRANCOUNT,XACT_STATE(); END CATCH; IF @@TRANCOUNT>0 COMMIT; SELECT id,g FROM dbo.guid_runtime WHERE id=30"
        ),
    );
    assert!(!success, "{tokens:?}");
    assert_eq!(
        session.last_error, 3998,
        "CATCH and post-COMMIT reads must remain executable: {tokens:?}"
    );
    assert_eq!(session.transactions, 0);
    assert_eq!(ids(&session), Vec::<i32>::new());
}
