//! DML triggers through the engine: definitions in the module store, firing
//! with multirow `inserted`/`deleted` images, UPDATE()/COLUMNS_UPDATED(),
//! nesting, error and ROLLBACK propagation, DISABLE/ENABLE and DROP TABLE.
//! Expected values and error numbers come from SQL Server 2022 (see
//! docs/gaps-triggers.md and reference/gaps-triggers.json).
use msduck::engine::Session;
use msduck::server::Server;

fn session(server: &Server) -> Session {
    Session::new(server.connection().unwrap()).unwrap()
}

fn memory() -> (Server, Session) {
    let server = Server::open(":memory:").unwrap();
    let session = session(&server);
    (server, session)
}

/// Run a batch; return its tokens and success.
fn run(session: &mut Session, sql: &str) -> (Vec<u8>, bool) {
    session.batch_response(sql, &Default::default(), false, None)
}

fn ok(session: &mut Session, sql: &str) -> Vec<u8> {
    let (out, ok) = run(session, sql);
    assert!(ok, "{sql}: {:?}", errors(&out));
    out
}

/// ERROR tokens (number, state, class, message), decoded only where their
/// fields fill the token exactly.
fn errors(out: &[u8]) -> Vec<(i32, u8, u8, String)> {
    let mut found = Vec::new();
    let mut i = 0;
    while i + 3 < out.len() {
        if out[i] == 0xAA {
            let length = u16::from_le_bytes([out[i + 1], out[i + 2]]) as usize;
            if let Some(body) = out.get(i + 3..i + 3 + length)
                && body.len() >= 8
            {
                let number = i32::from_le_bytes(body[0..4].try_into().unwrap());
                let characters = u16::from_le_bytes([body[6], body[7]]) as usize;
                let mut at = 8 + characters * 2;
                let mut valid = at <= body.len();
                for _ in 0..2 {
                    if valid && at < body.len() {
                        at += 1 + body[at] as usize * 2;
                    } else {
                        valid = false;
                    }
                }
                if valid && at + 4 == body.len() {
                    let units: Vec<u16> = body[8..8 + characters * 2]
                        .chunks(2)
                        .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
                        .collect();
                    found.push((number, body[4], body[5], String::from_utf16_lossy(&units)));
                    i += 3 + length;
                    continue;
                }
            }
        }
        i += 1;
    }
    found
}

fn error_numbers(out: &[u8]) -> Vec<i32> {
    errors(out).into_iter().map(|error| error.0).collect()
}

/// The single error of a failed batch.
fn fails(session: &mut Session, sql: &str) -> (i32, u8, u8, String) {
    let (out, ok) = run(session, sql);
    assert!(!ok, "{sql} should fail");
    let mut found = errors(&out);
    assert_eq!(found.len(), 1, "{sql}: {found:?}");
    found.remove(0)
}

fn texts(session: &Session, sql: &str) -> Vec<String> {
    let mut statement = session.db.prepare(sql).unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap()
}

fn number(session: &Session, sql: &str) -> i64 {
    session.db.query_row(sql, [], |row| row.get(0)).unwrap()
}

#[test]
fn definitions_live_in_the_module_store() {
    let (_server, mut session) = memory();
    ok(
        &mut session,
        "CREATE TABLE items(id INT PRIMARY KEY, qty INT)",
    );
    ok(
        &mut session,
        "CREATE TRIGGER items_audit ON items AFTER INSERT, DELETE AS\nBEGIN\n  SELECT 1;\nEND;",
    );
    let (definition, properties, parent, disabled): (String, String, i32, bool) = session
        .db
        .query_row(
            "SELECT definition, properties, parent_object_id, is_disabled FROM main.__msduck_modules WHERE name = 'items_audit' AND type_code = 'TR'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        definition,
        "CREATE TRIGGER items_audit ON items AFTER INSERT, DELETE AS\nBEGIN\n  SELECT 1;\nEND;"
    );
    assert_eq!(
        properties,
        r#"{"events":["INSERT","DELETE"],"instead_of":false}"#
    );
    assert_eq!(
        i64::from(parent),
        number(&session, "SELECT __msduck_object_id('dbo.items', 'U')")
    );
    assert!(!disabled);
    assert_eq!(
        texts(
            &session,
            "SELECT type FROM sys.objects WHERE name = 'items_audit'"
        ),
        ["TR"]
    );

    // ALTER keeps the object id and is stored as CREATE; CREATE OR ALTER drops OR ALTER.
    let id = number(
        &session,
        "SELECT object_id FROM main.__msduck_modules WHERE name = 'items_audit'",
    );
    ok(
        &mut session,
        "ALTER TRIGGER items_audit ON items INSTEAD OF UPDATE AS SELECT 2",
    );
    ok(&mut session, "DISABLE TRIGGER items_audit ON items");
    ok(
        &mut session,
        "CREATE OR ALTER TRIGGER dbo.items_audit ON dbo.items FOR UPDATE, INSERT AS SELECT 3",
    );
    let (definition, properties, disabled): (String, String, bool) = session
        .db
        .query_row(
            "SELECT definition, properties, is_disabled FROM main.__msduck_modules WHERE object_id = ?",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        definition,
        "CREATE   TRIGGER dbo.items_audit ON dbo.items FOR UPDATE, INSERT AS SELECT 3"
    );
    assert_eq!(
        properties,
        r#"{"events":["INSERT","UPDATE"],"instead_of":false}"#
    );
    assert!(disabled, "ALTER keeps a disabled trigger disabled");

    // CREATE TRIGGER errors.
    for (sql, expected) in [
        (
            "CREATE TRIGGER t ON nosuch AFTER INSERT AS SELECT 1",
            (
                8197,
                4,
                16,
                "The object 'nosuch' does not exist or is invalid for this operation.",
            ),
        ),
        (
            "SELECT 1\nCREATE TRIGGER t ON items AFTER INSERT AS SELECT 1",
            (
                111,
                6,
                15,
                "'CREATE TRIGGER' must be the first statement in a query batch.",
            ),
        ),
        (
            "CREATE TRIGGER items ON items AFTER INSERT AS SELECT 1",
            (
                2714,
                2,
                16,
                "There is already an object named 'items' in the database.",
            ),
        ),
        (
            "CREATE TRIGGER items_audit ON items AFTER INSERT AS SELECT 1",
            (
                2714,
                2,
                16,
                "There is already an object named 'items_audit' in the database.",
            ),
        ),
        (
            "CREATE TRIGGER t ON items AFTER INSERT, INSERT AS SELECT 1",
            (
                1034,
                1,
                15,
                "Syntax error: Duplicate specification of the action \"INSERT\" in the trigger declaration.",
            ),
        ),
        (
            "CREATE TRIGGER t ON items AFTER INSERT AS",
            (102, 1, 15, "Incorrect syntax near 'AS'."),
        ),
        (
            "ALTER TRIGGER nope ON items AFTER INSERT AS SELECT 1",
            (208, 6, 16, "Invalid object name 'nope'."),
        ),
        (
            "CREATE TRIGGER other.t ON items AFTER INSERT AS SELECT 1",
            (
                2103,
                1,
                15,
                "Cannot create trigger 'other.t' because its schema is different from the schema of the target table or view.",
            ),
        ),
    ] {
        let (number, state, class, message) = fails(&mut session, sql);
        assert_eq!((number, state, class, message.as_str()), expected, "{sql}");
    }
    ok(&mut session, "CREATE TABLE log(id INT)");
    let error = fails(
        &mut session,
        "ALTER TRIGGER items_audit ON log AFTER INSERT AS SELECT 1",
    );
    assert_eq!((error.0, error.2), (2110, 15));
    ok(&mut session, "CREATE VIEW v AS SELECT id FROM items");
    assert_eq!(
        fails(
            &mut session,
            "CREATE TRIGGER vt ON v AFTER INSERT AS SELECT 1"
        )
        .0,
        8197
    );
    ok(
        &mut session,
        "CREATE TRIGGER io1 ON items INSTEAD OF INSERT AS SELECT 1",
    );
    let error = fails(
        &mut session,
        "CREATE TRIGGER io2 ON items INSTEAD OF DELETE, INSERT AS SELECT 1",
    );
    assert_eq!(
        (error.0, error.3.as_str()),
        (
            2111,
            "Cannot create trigger 'io2' on table 'items' because an INSTEAD OF INSERT trigger already exists on this object."
        )
    );

    // DROP TRIGGER drops every name it finds and reports the rest (3701).
    let (out, success) = run(&mut session, "DROP TRIGGER io1, nosuch, items_audit");
    assert!(!success);
    assert_eq!(
        errors(&out),
        [(3701, 5, 11, "Cannot drop the trigger 'nosuch', because it does not exist or you do not have permission.".to_string())]
    );
    assert_eq!(
        number(
            &session,
            "SELECT count(*) FROM main.__msduck_modules WHERE type_code = 'TR'"
        ),
        0
    );
    ok(&mut session, "DROP TRIGGER IF EXISTS nosuch");

    // DROP TABLE drops the table's triggers.
    ok(
        &mut session,
        "CREATE TRIGGER t1 ON items AFTER INSERT AS SELECT 1",
    );
    ok(
        &mut session,
        "CREATE TRIGGER t2 ON log AFTER INSERT AS SELECT 1",
    );
    ok(&mut session, "DROP TABLE items");
    assert_eq!(
        texts(
            &session,
            "SELECT name FROM main.__msduck_modules WHERE type_code = 'TR'"
        ),
        ["t2"]
    );
    ok(&mut session, "CREATE TABLE items(id INT)");
    ok(
        &mut session,
        "CREATE TRIGGER t1 ON items AFTER INSERT AS SELECT 1",
    );
}

#[test]
fn after_triggers_see_multirow_images_once_per_statement() {
    let (_server, mut session) = memory();
    ok(
        &mut session,
        "CREATE TABLE items(id INT PRIMARY KEY, name VARCHAR(20), qty INT DEFAULT 5);
         CREATE TABLE audit(op VARCHAR(10), id INT, old_qty INT, new_qty INT, rows_in INT, rows_out INT, qty_updated INT, mask VARBINARY(4), level INT);
         CREATE TABLE counts(label VARCHAR(20), n INT);",
    );
    ok(
        &mut session,
        "CREATE TRIGGER items_iud ON items AFTER INSERT, UPDATE, DELETE AS
         BEGIN
           DECLARE @rows INT = @@ROWCOUNT;
           INSERT counts VALUES ('trigger', @rows);
           INSERT audit
             SELECT CASE WHEN d.id IS NULL THEN 'insert' WHEN i.id IS NULL THEN 'delete' ELSE 'update' END,
                    COALESCE(i.id, d.id), d.qty, i.qty,
                    (SELECT COUNT(*) FROM inserted), (SELECT COUNT(*) FROM deleted),
                    CASE WHEN UPDATE(qty) THEN 1 ELSE 0 END, COLUMNS_UPDATED(), TRIGGER_NESTLEVEL()
             FROM inserted i FULL JOIN deleted d ON d.id = i.id;
         END",
    );
    ok(
        &mut session,
        "INSERT items(id, name) VALUES (1, 'a'), (2, 'b'), (3, 'c');
         INSERT counts VALUES ('insert', @@ROWCOUNT);
         UPDATE items SET qty = qty + 1 WHERE id >= 2;
         INSERT counts VALUES ('update', @@ROWCOUNT);
         UPDATE items SET name = 'x' WHERE id = 99;
         INSERT counts VALUES ('none', @@ROWCOUNT);
         DELETE FROM items WHERE id = 1;
         INSERT counts VALUES ('delete', @@ROWCOUNT);",
    );
    // op, id, old and new qty, |inserted|, |deleted|, UPDATE(qty), mask, level
    assert_eq!(
        texts(
            &session,
            "SELECT op || ',' || id || ',' || coalesce(CAST(old_qty AS VARCHAR), '-') || ',' || coalesce(CAST(new_qty AS VARCHAR), '-')
               || ',' || rows_in || ',' || rows_out || ',' || qty_updated || ',' || hex(mask) || ',' || level
             FROM dbo.audit ORDER BY op, id"
        ),
        [
            "delete,1,5,-,0,1,0,,1",
            "insert,1,-,5,3,0,1,07,1",
            "insert,2,-,5,3,0,1,07,1",
            "insert,3,-,5,3,0,1,07,1",
            "update,2,5,6,2,2,1,04,1",
            "update,3,5,6,2,2,1,04,1",
        ]
    );
    // The trigger saw the statement's row count; the statement's @@ROWCOUNT
    // survives the trigger's own statements. A zero-row UPDATE still fires.
    assert_eq!(
        texts(
            &session,
            "SELECT label || '=' || n FROM dbo.counts ORDER BY rowid"
        ),
        [
            "trigger=3",
            "insert=3",
            "trigger=2",
            "update=2",
            "trigger=0",
            "none=0",
            "trigger=1",
            "delete=1"
        ]
    );
}

#[test]
fn key_updates_and_joined_statements_capture_their_rows() {
    let (_server, mut session) = memory();
    ok(
        &mut session,
        "CREATE TABLE k(id INT PRIMARY KEY, code VARCHAR(10) UNIQUE, n INT);
         CREATE TABLE src(id INT, code VARCHAR(10));
         CREATE TABLE klog(op CHAR(1), id INT, code VARCHAR(10), n INT);
         INSERT k VALUES (1, 'a', 10), (2, 'b', 20), (3, 'c', 30);
         INSERT src VALUES (2, 'zz'), (3, 'yy');",
    );
    ok(
        &mut session,
        "CREATE TRIGGER k_log ON k AFTER UPDATE, DELETE AS
           SET NOCOUNT ON;
           INSERT klog SELECT 'D', id, code, n FROM deleted;
           INSERT klog SELECT 'I', id, code, n FROM inserted;",
    );
    // Key updates rewrite rows; their new images are still captured.
    ok(
        &mut session,
        "UPDATE k SET id = id + 100, code = code + 'x' WHERE id = 1",
    );
    ok(
        &mut session,
        "UPDATE x SET code = s.code FROM k AS x JOIN src s ON s.id = x.id",
    );
    ok(
        &mut session,
        "WITH doomed AS (SELECT id FROM src WHERE code = 'yy') DELETE k FROM k JOIN doomed ON doomed.id = k.id",
    );
    assert_eq!(
        texts(
            &session,
            "SELECT op || ':' || id || ':' || code || ':' || n FROM dbo.klog ORDER BY rowid"
        ),
        [
            "D:1:a:10",
            "I:101:ax:10",
            "D:2:b:20",
            "D:3:c:30",
            "I:2:zz:20",
            "I:3:yy:30",
            "D:3:yy:30",
        ]
    );
    assert_eq!(
        texts(&session, "SELECT id || ':' || code FROM dbo.k ORDER BY id"),
        ["2:zz", "101:ax"]
    );
}

#[test]
fn instead_of_triggers_replace_the_statement() {
    let (_server, mut session) = memory();
    ok(
        &mut session,
        "CREATE TABLE t(id INT IDENTITY(10, 1) PRIMARY KEY, name VARCHAR(20), qty INT DEFAULT 7, stamp INT);
         CREATE TABLE log(msg VARCHAR(100));
         CREATE TABLE counts(n INT);",
    );
    ok(
        &mut session,
        "CREATE TRIGGER t_ins ON t INSTEAD OF INSERT AS
           INSERT log SELECT 'id=' + CAST(id AS VARCHAR) + ' qty=' + CAST(qty AS VARCHAR) + ' name=' + name FROM inserted;
           INSERT t(name, qty, stamp) SELECT UPPER(name), qty, 1 FROM inserted;",
    );
    ok(
        &mut session,
        "CREATE TRIGGER t_upd ON t INSTEAD OF UPDATE, DELETE AS
           INSERT log SELECT 'upd/del i=' + CAST((SELECT COUNT(*) FROM inserted) AS VARCHAR) + ' d=' + CAST((SELECT COUNT(*) FROM deleted) AS VARCHAR)
             + ' new=' + ISNULL((SELECT CAST(SUM(qty) AS VARCHAR) FROM inserted), 'none');",
    );
    ok(
        &mut session,
        "INSERT t(name) VALUES ('a'), ('b'); INSERT counts VALUES (@@ROWCOUNT);
         UPDATE t SET qty = qty * 10; INSERT counts VALUES (@@ROWCOUNT);
         DELETE t WHERE id = 10; INSERT counts VALUES (@@ROWCOUNT);",
    );
    assert_eq!(
        texts(&session, "SELECT msg FROM dbo.log ORDER BY rowid"),
        [
            "id=0 qty=7 name=a",
            "id=0 qty=7 name=b",
            "upd/del i=2 d=2 new=140",
            "upd/del i=0 d=1 new=none",
        ]
    );
    // The INSTEAD OF trigger's own INSERT ran normally; UPDATE and DELETE did not.
    assert_eq!(
        texts(
            &session,
            "SELECT id || ':' || name || ':' || qty || ':' || stamp FROM dbo.t ORDER BY id"
        ),
        ["10:A:7:1", "11:B:7:1"]
    );
    assert_eq!(
        texts(
            &session,
            "SELECT CAST(n AS VARCHAR) FROM dbo.counts ORDER BY rowid"
        ),
        ["2", "2", "1"]
    );
}

#[test]
fn errors_and_rollback_in_triggers_end_the_batch() {
    let (_server, mut session) = memory();
    ok(
        &mut session,
        "CREATE TABLE t(id INT PRIMARY KEY, v INT); CREATE TABLE log(msg VARCHAR(50));",
    );
    ok(
        &mut session,
        "CREATE TRIGGER t_check ON t AFTER INSERT AS
           IF EXISTS (SELECT 1 FROM inserted WHERE v < 0)
           BEGIN
             RAISERROR('negative value', 16, 1);
             ROLLBACK TRANSACTION;
             INSERT log SELECT 'after rollback: ' + CAST(COUNT(*) AS VARCHAR) FROM inserted;
             RETURN;
           END",
    );
    ok(&mut session, "INSERT t VALUES (1, 1), (2, 2)");
    let (out, success) = run(
        &mut session,
        "INSERT t VALUES (3, 3), (4, -4); INSERT log VALUES ('not reached')",
    );
    assert!(!success);
    assert_eq!(
        errors(&out)
            .into_iter()
            .map(|e| (e.0, e.3))
            .collect::<Vec<_>>(),
        [
            (50000, "negative value".to_string()),
            (
                3609,
                "The transaction ended in the trigger. The batch has been aborted.".to_string()
            )
        ]
    );
    // The statement was rolled back; the trigger continued after ROLLBACK,
    // seeing empty images, and its later write was kept.
    assert_eq!(number(&session, "SELECT count(*) FROM dbo.t"), 2);
    assert_eq!(
        texts(&session, "SELECT msg FROM dbo.log"),
        ["after rollback: 0"]
    );
    assert_eq!(session.transactions, 0);

    // In an explicit transaction ROLLBACK undoes everything before the statement too.
    let (out, _) = run(
        &mut session,
        "BEGIN TRAN; INSERT log VALUES ('in tran'); INSERT t VALUES (5, -5);",
    );
    assert_eq!(error_numbers(&out), [50000, 3609]);
    assert_eq!(session.transactions, 0);
    assert_eq!(
        number(
            &session,
            "SELECT count(*) FROM dbo.log WHERE msg = 'in tran'"
        ),
        0
    );

    // THROW and runtime errors end the batch and roll back the statement
    // (and the transaction), without 3609.
    ok(&mut session, "DROP TRIGGER t_check; DELETE log");
    ok(
        &mut session,
        "CREATE TRIGGER t_throw ON t AFTER UPDATE AS
           INSERT log VALUES ('before');
           IF EXISTS (SELECT 1 FROM inserted WHERE v = 13) THROW 50001, 'thirteen', 1;
           DECLARE @x INT = (SELECT 1 / MIN(v) FROM inserted);
           INSERT log VALUES ('after');",
    );
    for (sql, expected) in [
        (
            "UPDATE t SET v = 13 WHERE id = 1; INSERT log VALUES ('next')",
            50001,
        ),
        (
            "BEGIN TRAN; UPDATE t SET v = v + 100; UPDATE t SET v = 13 WHERE id = 1; INSERT log VALUES ('next')",
            50001,
        ),
        (
            "UPDATE t SET v = 0 WHERE id = 2; INSERT log VALUES ('next')",
            8134,
        ),
    ] {
        let (out, success) = run(&mut session, sql);
        assert!(!success, "{sql}");
        assert_eq!(error_numbers(&out), [expected], "{sql}");
        assert_eq!(session.transactions, 0, "{sql}");
    }
    assert_eq!(
        texts(&session, "SELECT CAST(v AS VARCHAR) FROM dbo.t ORDER BY id"),
        ["1", "2"]
    );
    assert_eq!(number(&session, "SELECT count(*) FROM dbo.log"), 0);

    // Inside TRY an error in a trigger reaches CATCH with the explicit
    // transaction doomed (XACT_STATE() = -1), as in SQL Server.
    ok(&mut session, "CREATE TABLE caught(n INT, tc INT, xs INT)");
    let out = ok(
        &mut session,
        "BEGIN TRAN;
         BEGIN TRY
           UPDATE t SET v = 13 WHERE id = 1;
         END TRY
         BEGIN CATCH
           DECLARE @n INT = ERROR_NUMBER(), @tc INT = @@TRANCOUNT, @xs INT = XACT_STATE();
           ROLLBACK;
           INSERT caught VALUES (@n, @tc, @xs);
         END CATCH",
    );
    assert!(errors(&out).is_empty());
    assert_eq!(
        texts(
            &session,
            "SELECT n || ',' || tc || ',' || xs FROM dbo.caught"
        ),
        ["50001,1,-1"]
    );
    assert_eq!(session.transactions, 0);

    // RAISERROR alone does not stop the trigger or undo the statement.
    ok(&mut session, "DROP TRIGGER t_throw; DELETE log");
    ok(
        &mut session,
        "CREATE TRIGGER t_warn ON t AFTER DELETE AS RAISERROR('warned', 16, 1); INSERT log VALUES ('kept');",
    );
    let (out, _) = run(
        &mut session,
        "DELETE t WHERE id = 2; INSERT log VALUES ('next')",
    );
    assert_eq!(error_numbers(&out), [50000]);
    assert_eq!(number(&session, "SELECT count(*) FROM dbo.t"), 1);
    assert_eq!(
        texts(&session, "SELECT msg FROM dbo.log ORDER BY rowid"),
        ["kept", "next"]
    );
}

#[test]
fn nested_triggers_fire_but_do_not_recurse_directly() {
    let (_server, mut session) = memory();
    ok(
        &mut session,
        "CREATE TABLE a(id INT, v INT); CREATE TABLE b(id INT, v INT); CREATE TABLE log(msg VARCHAR(50));",
    );
    ok(
        &mut session,
        "CREATE TRIGGER a_t ON a AFTER INSERT, UPDATE AS
           INSERT log SELECT 'a level ' + CAST(TRIGGER_NESTLEVEL() AS VARCHAR) + ' rows ' + CAST(COUNT(*) AS VARCHAR) FROM inserted;
           INSERT b SELECT id, v FROM inserted;
           UPDATE a SET v = v + 100 WHERE id IN (SELECT id FROM inserted);",
    );
    ok(
        &mut session,
        "CREATE TRIGGER b_t ON b AFTER INSERT AS INSERT log SELECT 'b level ' + CAST(TRIGGER_NESTLEVEL() AS VARCHAR) + ' nest ' + CAST(@@NESTLEVEL AS VARCHAR) FROM inserted WHERE id = 1",
    );
    ok(
        &mut session,
        "CREATE TRIGGER b_t2 ON b AFTER INSERT AS INSERT log VALUES ('b second')",
    );
    ok(&mut session, "INSERT a VALUES (1, 1), (2, 2)");
    assert_eq!(
        texts(&session, "SELECT msg FROM dbo.log ORDER BY rowid"),
        ["a level 1 rows 2", "b level 2 nest 2", "b second"]
    );
    assert_eq!(
        texts(&session, "SELECT CAST(v AS VARCHAR) FROM dbo.a ORDER BY id"),
        ["101", "102"]
    );

    // Indirect recursion stops at the nesting limit and undoes everything.
    ok(
        &mut session,
        "DELETE log; DELETE a; DELETE b; DROP TRIGGER a_t, b_t2",
    );
    ok(
        &mut session,
        "CREATE TRIGGER a_back ON a AFTER INSERT AS INSERT b SELECT id, v + 1 FROM inserted",
    );
    ok(
        &mut session,
        "ALTER TRIGGER b_t ON b AFTER INSERT AS INSERT a SELECT id, v + 1 FROM inserted",
    );
    let (out, success) = run(
        &mut session,
        "INSERT a VALUES (1, 0); INSERT log VALUES ('next')",
    );
    assert!(!success);
    assert_eq!(
        errors(&out).last().map(|e| (e.0, e.3.clone())),
        Some((217, "Maximum stored procedure, function, trigger, or view nesting level exceeded (limit 32).".to_string()))
    );
    assert_eq!(
        number(&session, "SELECT count(*) FROM dbo.a")
            + number(&session, "SELECT count(*) FROM dbo.b")
            + number(&session, "SELECT count(*) FROM dbo.log"),
        0
    );
}

#[test]
fn disable_enable_and_drop_table_lifecycle() {
    let (_server, mut session) = memory();
    ok(
        &mut session,
        "CREATE TABLE items(id INT); CREATE TABLE log(msg VARCHAR(20));",
    );
    ok(
        &mut session,
        "CREATE TRIGGER one ON items AFTER INSERT AS INSERT log VALUES ('one')",
    );
    ok(
        &mut session,
        "CREATE TRIGGER two ON items AFTER INSERT AS INSERT log VALUES ('two')",
    );
    let fired = |session: &mut Session| {
        ok(session, "DELETE log; INSERT items VALUES (1)");
        texts(session, "SELECT msg FROM dbo.log ORDER BY rowid")
    };
    assert_eq!(fired(&mut session), ["one", "two"]);
    ok(&mut session, "DISABLE TRIGGER ALL ON items");
    assert!(fired(&mut session).is_empty());
    ok(&mut session, "ENABLE TRIGGER two ON dbo.items");
    assert_eq!(fired(&mut session), ["two"]);
    ok(&mut session, "ALTER TABLE items ENABLE TRIGGER ALL");
    ok(
        &mut session,
        "ALTER TABLE dbo.items DISABLE TRIGGER two, one",
    );
    assert!(fired(&mut session).is_empty());
    ok(&mut session, "ALTER TABLE items ENABLE TRIGGER one");
    assert_eq!(fired(&mut session), ["one"]);
    // DDL triggers do not exist, so ALL ON DATABASE succeeds.
    ok(
        &mut session,
        "DISABLE TRIGGER ALL ON DATABASE; ENABLE TRIGGER ALL ON ALL SERVER",
    );
    for (sql, expected) in [
        (
            "DISABLE TRIGGER nosuch ON items",
            (
                1088,
                119,
                "Cannot find the object \"nosuch\" because it does not exist or you do not have permissions.",
            ),
        ),
        (
            "ENABLE TRIGGER ALL ON nosuch",
            (
                1088,
                21,
                "Cannot find the object \"nosuch\" because it does not exist or you do not have permissions.",
            ),
        ),
        (
            "ALTER TABLE items DISABLE TRIGGER nosuch",
            (
                4920,
                0,
                "ALTER TABLE failed because trigger 'nosuch' on table 'items' does not exist.",
            ),
        ),
        (
            "DROP TRIGGER nosuch",
            (
                3701,
                5,
                "Cannot drop the trigger 'nosuch', because it does not exist or you do not have permission.",
            ),
        ),
    ] {
        let error = fails(&mut session, sql);
        assert_eq!((error.0, error.1, error.3.as_str()), expected, "{sql}");
    }
    // OUTPUT without INTO is refused while an enabled trigger would fire.
    let error = fails(&mut session, "INSERT items OUTPUT inserted.id VALUES (2)");
    assert_eq!(error.0, 334);
    ok(&mut session, "CREATE TABLE sink(id INT)");
    ok(
        &mut session,
        "INSERT items OUTPUT inserted.id INTO sink VALUES (3)",
    );
    assert_eq!(number(&session, "SELECT count(*) FROM dbo.sink"), 1);
    // A transaction that drops the table can be rolled back with its triggers.
    ok(&mut session, "BEGIN TRAN; DROP TABLE items; ROLLBACK");
    assert_eq!(
        number(
            &session,
            "SELECT count(*) FROM main.__msduck_modules WHERE type_code = 'TR'"
        ),
        2
    );
    ok(&mut session, "DROP TABLE items");
    assert_eq!(
        number(
            &session,
            "SELECT count(*) FROM main.__msduck_modules WHERE type_code = 'TR'"
        ),
        0
    );
    // No image tables are left behind.
    assert_eq!(
        number(
            &session,
            "SELECT count(*) FROM duckdb_tables() WHERE table_name LIKE '__msduck_trigger%'"
        ),
        0
    );
}

#[test]
fn concurrent_sessions_and_restart_keep_triggers_working() {
    let directory = std::env::temp_dir().join(format!("msduck-triggers-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("triggers.duckdb");
    let path = path.to_str().unwrap();
    {
        let server = Server::open(path).unwrap();
        let mut first = session(&server);
        let mut second = session(&server);
        ok(
            &mut first,
            "CREATE TABLE a(id INT, name VARCHAR(10)); CREATE TABLE b(id INT, name VARCHAR(10));
             CREATE TABLE alog(msg VARCHAR(30)); CREATE TABLE blog(msg VARCHAR(30));",
        );
        ok(
            &mut first,
            "CREATE TRIGGER a_t ON a AFTER INSERT AS INSERT alog SELECT name + '!' FROM inserted",
        );
        ok(
            &mut first,
            "CREATE TRIGGER b_t ON b AFTER INSERT AS INSERT blog SELECT name + '?' FROM inserted",
        );
        // Both sessions fire triggers while the first transaction is open.
        ok(&mut first, "BEGIN TRAN; INSERT a VALUES (1, 'x')");
        ok(&mut second, "BEGIN TRAN; INSERT b VALUES (1, 'y')");
        ok(&mut first, "INSERT a VALUES (2, 'z'); COMMIT");
        ok(&mut second, "COMMIT");
        assert_eq!(
            texts(&first, "SELECT msg FROM dbo.alog ORDER BY msg"),
            ["x!", "z!"]
        );
        assert_eq!(texts(&first, "SELECT msg FROM dbo.blog"), ["y?"]);
    }
    // After a restart the stored definitions fire, and images still bind
    // with the table's declarations (string concatenation above).
    let server = Server::open(path).unwrap();
    let mut session = session(&server);
    ok(&mut session, "INSERT a VALUES (3, 'w')");
    assert_eq!(
        texts(&session, "SELECT msg FROM dbo.alog ORDER BY msg"),
        ["w!", "x!", "z!"]
    );
    drop(session);
    drop(server);
    let _ = std::fs::remove_dir_all(&directory);
}
