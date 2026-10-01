//! MERGE execution (issue #722): every WHEN family, table hints, TOP,
//! OUTPUT INTO, variables, errors and atomicity. Values and error numbers
//! follow reference/merge-execution.json, reference/merge-top.json,
//! reference/merge-transaction.json and reference/gaps-merge.json.
//! tests/compat/merge.test.mjs covers the same behavior through tedious.
use msduck::engine::Session;
use msduck::server::Server;

fn open() -> (Server, Session) {
    let server = Server::open(":memory:").unwrap();
    let session = Session::new(server.connection().unwrap()).unwrap();
    (server, session)
}

/// Run a batch and return whether it succeeded and `@@ERROR`.
fn run(session: &mut Session, sql: &str) -> (bool, i32) {
    let (_, ok) = session.batch_response(sql, &Default::default(), false, None);
    (ok, session.last_error)
}

fn ok(session: &mut Session, sql: &str) {
    assert_eq!(run(session, sql), (true, 0), "{sql}");
}

fn rows(session: &Session, sql: &str) -> Vec<Vec<Option<i64>>> {
    let mut statement = session.db.prepare(sql).unwrap();
    statement.execute([]).unwrap();
    let width = statement.column_count();
    statement
        .query_map([], |r| {
            (0..width)
                .map(|i| r.get::<_, Option<i64>>(i))
                .collect::<duckdb::Result<Vec<_>>>()
        })
        .unwrap()
        .collect::<duckdb::Result<Vec<_>>>()
        .unwrap()
}

fn pairs(session: &Session, table: &str) -> Vec<(i64, Option<i64>)> {
    rows(
        session,
        &format!("SELECT id, n FROM dbo.{table} ORDER BY id"),
    )
    .into_iter()
    .map(|row| (row[0].unwrap(), row[1]))
    .collect()
}

fn rowcount(session: &mut Session) -> u64 {
    session.rowcount
}

const UPSERT: &str = "MERGE items {hint} AS target USING (VALUES (1)) AS source(id) ON target.id = source.id WHEN NOT MATCHED THEN INSERT (id) VALUES (source.id);";

#[test]
fn generic_upsert_with_and_without_hints() {
    for hint in [
        "",
        "WITH (SERIALIZABLE)",
        "WITH(HOLDLOCK)",
        "WITH (UPDLOCK, ROWLOCK)",
        "WITH (HOLDLOCK, UPDLOCK)",
    ] {
        let (_server, mut session) = open();
        ok(
            &mut session,
            "CREATE TABLE items(id INT NOT NULL PRIMARY KEY)",
        );
        let sql = UPSERT.replace("{hint}", hint);
        ok(&mut session, &sql);
        assert_eq!(rowcount(&mut session), 1, "{hint}");
        // The second run matches and has no WHEN MATCHED clause.
        ok(&mut session, &sql);
        assert_eq!(rowcount(&mut session), 0, "{hint}");
        assert_eq!(rows(&session, "SELECT id FROM dbo.items"), [[Some(1)]]);
    }
}

#[test]
fn every_clause_family_and_action() {
    let (_server, mut session) = open();
    ok(
        &mut session,
        "CREATE TABLE dbo.merge_target(id INT NOT NULL PRIMARY KEY, n INT NOT NULL); INSERT dbo.merge_target(id,n) VALUES (1,10),(2,20),(3,30)",
    );
    // WHEN MATCHED THEN UPDATE
    ok(
        &mut session,
        "MERGE dbo.merge_target AS t USING (VALUES (1,11)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n;",
    );
    assert_eq!(rowcount(&mut session), 1);
    // WHEN NOT MATCHED BY TARGET THEN INSERT
    ok(
        &mut session,
        "MERGE dbo.merge_target AS t USING (VALUES (4,40)) AS s(id,n) ON t.id=s.id WHEN NOT MATCHED BY TARGET THEN INSERT (id,n) VALUES (s.id,s.n);",
    );
    assert_eq!(rowcount(&mut session), 1);
    // WHEN NOT MATCHED BY SOURCE AND ... THEN DELETE
    ok(
        &mut session,
        "MERGE dbo.merge_target AS t USING (VALUES (1),(2),(4)) AS s(id) ON t.id=s.id WHEN NOT MATCHED BY SOURCE AND t.id=3 THEN DELETE;",
    );
    assert_eq!(rowcount(&mut session), 1);
    assert_eq!(
        pairs(&session, "merge_target"),
        [(1, Some(11)), (2, Some(20)), (4, Some(40))]
    );
    // WHEN NOT MATCHED BY SOURCE THEN UPDATE, WHEN MATCHED THEN DELETE
    ok(
        &mut session,
        "MERGE INTO dbo.merge_target t USING (SELECT 2 AS id) s ON t.id=s.id WHEN MATCHED THEN DELETE WHEN NOT MATCHED BY SOURCE THEN UPDATE SET n = t.n + 1;",
    );
    assert_eq!(rowcount(&mut session), 3);
    assert_eq!(
        pairs(&session, "merge_target"),
        [(1, Some(12)), (4, Some(41))]
    );
}

#[test]
fn mixed_actions_with_output_into() {
    let (_server, mut session) = open();
    ok(
        &mut session,
        "CREATE TABLE dbo.merge_mix(id INT NOT NULL PRIMARY KEY,n INT NOT NULL); INSERT dbo.merge_mix(id,n) VALUES (1,10),(2,20),(3,30); CREATE TABLE dbo.merge_output([action] VARCHAR(10) NOT NULL,inserted_id INT NULL,deleted_id INT NULL)",
    );
    ok(
        &mut session,
        "MERGE dbo.merge_mix AS t
    USING (VALUES (1,11),(2,22),(4,44)) AS s(id,n) ON t.id=s.id
    WHEN MATCHED AND s.id=1 THEN UPDATE SET n=s.n
    WHEN MATCHED AND s.id=2 THEN DELETE
    WHEN NOT MATCHED BY TARGET THEN INSERT (id,n) VALUES (s.id,s.n)
    WHEN NOT MATCHED BY SOURCE AND t.id=3 THEN DELETE
    OUTPUT $action, inserted.id, deleted.id INTO dbo.merge_output([action],inserted_id,deleted_id);",
    );
    assert_eq!(rowcount(&mut session), 4);
    assert_eq!(pairs(&session, "merge_mix"), [(1, Some(11)), (4, Some(44))]);
    let mut statement = session
        .db
        .prepare("SELECT CAST(\"action\" AS VARCHAR), inserted_id, deleted_id FROM dbo.merge_output ORDER BY 1, COALESCE(inserted_id, deleted_id)")
        .unwrap();
    let output = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i32>>(1)?,
                r.get::<_, Option<i32>>(2)?,
            ))
        })
        .unwrap()
        .collect::<duckdb::Result<Vec<_>>>()
        .unwrap();
    let output = output
        .iter()
        .map(|(a, i, d)| (a.as_str(), *i, *d))
        .collect::<Vec<_>>();
    assert_eq!(
        output,
        [
            ("DELETE", None, Some(2)),
            ("DELETE", None, Some(3)),
            ("INSERT", Some(4), None),
            ("UPDATE", Some(1), Some(1)),
        ]
    );
}

#[test]
fn cte_source_and_variables() {
    let (_server, mut session) = open();
    ok(
        &mut session,
        "CREATE TABLE dbo.merge_target(id INT NOT NULL PRIMARY KEY,n INT NOT NULL); INSERT dbo.merge_target VALUES (1,10)",
    );
    ok(
        &mut session,
        "WITH s AS (SELECT 5 AS id,50 AS n) MERGE dbo.merge_target AS t USING s ON t.id=s.id WHEN NOT MATCHED BY TARGET THEN INSERT (id,n) VALUES (s.id,s.n);",
    );
    assert_eq!(rowcount(&mut session), 1);
    ok(
        &mut session,
        "DECLARE @id INT = 1, @n INT = 15; MERGE dbo.merge_target AS t USING (SELECT @id AS id) AS s ON t.id = s.id WHEN MATCHED THEN UPDATE SET n = @n WHEN NOT MATCHED THEN INSERT (id, n) VALUES (s.id, @n);",
    );
    assert_eq!(
        pairs(&session, "merge_target"),
        [(1, Some(15)), (5, Some(50))]
    );
    // A CTE prefix keeps TOP and target hints.
    ok(
        &mut session,
        "WITH s AS (SELECT id, n + 1 AS n FROM dbo.merge_target) MERGE TOP (1) dbo.merge_target WITH (HOLDLOCK) AS t USING s ON t.id = s.id WHEN MATCHED THEN UPDATE SET n = s.n;",
    );
    assert_eq!(rowcount(&mut session), 1);
}

#[test]
fn duplicate_source_rows_fail_with_8672_before_any_write() {
    let (_server, mut session) = open();
    ok(
        &mut session,
        "CREATE TABLE dbo.merge_duplicate(id INT NOT NULL PRIMARY KEY,n INT NOT NULL); INSERT dbo.merge_duplicate VALUES (1,10)",
    );
    assert_eq!(
        run(
            &mut session,
            "MERGE dbo.merge_duplicate AS t USING (VALUES (1,11),(1,12),(2,20)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n WHEN NOT MATCHED THEN INSERT (id,n) VALUES (s.id,s.n);"
        ),
        (false, 8672)
    );
    assert_eq!(pairs(&session, "merge_duplicate"), [(1, Some(10))]);
    // Only one of the matching source rows selects an action.
    ok(
        &mut session,
        "MERGE dbo.merge_duplicate AS t USING (VALUES (1,11),(1,12)) AS s(id,n) ON t.id=s.id WHEN MATCHED AND s.n=12 THEN UPDATE SET n=s.n;",
    );
    assert_eq!(pairs(&session, "merge_duplicate"), [(1, Some(12))]);
    // 8672 rolls back the caller's transaction.
    ok(
        &mut session,
        "BEGIN TRANSACTION; INSERT dbo.merge_duplicate VALUES (7,70)",
    );
    assert_eq!(
        run(
            &mut session,
            "MERGE dbo.merge_duplicate AS t USING (VALUES (1,1),(1,2)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n; INSERT dbo.merge_duplicate VALUES (8,80)"
        ),
        (false, 8672)
    );
    // The batch ended at the error and the earlier insert was rolled back.
    assert_eq!(session.transactions, 0);
    assert_eq!(pairs(&session, "merge_duplicate"), [(1, Some(12))]);
}

#[test]
fn constraint_failure_leaves_the_target_unchanged() {
    let (_server, mut session) = open();
    ok(
        &mut session,
        "CREATE TABLE dbo.merge_constraint(id INT NOT NULL PRIMARY KEY,n INT NOT NULL CONSTRAINT CK_merge_positive CHECK (n>0)); INSERT dbo.merge_constraint VALUES (1,10)",
    );
    // The batch continues after the terminated statement.
    let (_, number) = run(
        &mut session,
        "MERGE dbo.merge_constraint AS t USING (VALUES (1,11),(2,-1)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n WHEN NOT MATCHED BY TARGET THEN INSERT (id,n) VALUES (s.id,s.n); INSERT dbo.merge_constraint VALUES (3,30)",
    );
    assert_eq!(number, 0);
    assert_eq!(
        pairs(&session, "merge_constraint"),
        [(1, Some(10)), (3, Some(30))]
    );
    assert_eq!(
        run(
            &mut session,
            "MERGE dbo.merge_constraint AS t USING (VALUES (3,-3)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n;"
        ),
        (false, 547)
    );
    // A duplicate key from an insert arm.
    assert_eq!(
        run(
            &mut session,
            "MERGE dbo.merge_constraint AS t USING (VALUES (8,1),(8,2)) AS s(id,n) ON t.id=s.id WHEN NOT MATCHED THEN INSERT (id,n) VALUES (s.id,s.n);"
        ),
        (false, 2627)
    );
    assert_eq!(
        pairs(&session, "merge_constraint"),
        [(1, Some(10)), (3, Some(30))]
    );
}

#[test]
fn explicit_transaction_rollback_discards_the_merge() {
    let (_server, mut session) = open();
    ok(
        &mut session,
        "CREATE TABLE dbo.merge_rollback(id INT NOT NULL PRIMARY KEY,n INT NOT NULL); INSERT dbo.merge_rollback VALUES (1,10)",
    );
    ok(&mut session, "BEGIN TRANSACTION");
    ok(
        &mut session,
        "MERGE dbo.merge_rollback AS t USING (VALUES (1,11),(2,20)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n WHEN NOT MATCHED BY TARGET THEN INSERT (id,n) VALUES (s.id,s.n);",
    );
    assert_eq!(rowcount(&mut session), 2);
    assert_eq!(
        pairs(&session, "merge_rollback"),
        [(1, Some(11)), (2, Some(20))]
    );
    ok(&mut session, "ROLLBACK TRANSACTION");
    assert_eq!(pairs(&session, "merge_rollback"), [(1, Some(10))]);
}

#[test]
fn top_limits_the_actions() {
    let (_server, mut session) = open();
    ok(
        &mut session,
        "CREATE TABLE dbo.merge_top(id INT NOT NULL PRIMARY KEY,n INT NOT NULL)",
    );
    ok(
        &mut session,
        "MERGE TOP (0) dbo.merge_top AS t USING (VALUES (1,10)) AS s(id,n) ON t.id=s.id WHEN NOT MATCHED BY TARGET THEN INSERT (id,n) VALUES (s.id,s.n);",
    );
    assert_eq!(rowcount(&mut session), 0);
    ok(
        &mut session,
        "MERGE TOP (1) dbo.merge_top AS t USING (VALUES (1,10)) AS s(id,n) ON t.id=s.id WHEN NOT MATCHED BY TARGET THEN INSERT (id,n) VALUES (s.id,s.n);",
    );
    assert_eq!(rowcount(&mut session), 1);
    assert_eq!(
        run(
            &mut session,
            "MERGE TOP (-1) dbo.merge_top AS t USING (VALUES (2,20)) AS s(id,n) ON t.id=s.id WHEN NOT MATCHED BY TARGET THEN INSERT (id,n) VALUES (s.id,s.n);"
        ),
        (false, 127)
    );
    ok(
        &mut session,
        "DECLARE @n INT = 2; MERGE TOP (@n) dbo.merge_top AS t USING (VALUES (2,20),(3,30),(4,40)) AS s(id,n) ON t.id=s.id WHEN NOT MATCHED BY TARGET THEN INSERT (id,n) VALUES (s.id,s.n);",
    );
    assert_eq!(rowcount(&mut session), 2);
    ok(
        &mut session,
        "MERGE TOP (50) PERCENT dbo.merge_top AS t USING (VALUES (5,50),(6,60),(7,70)) AS s(id,n) ON t.id=s.id WHEN NOT MATCHED BY TARGET THEN INSERT (id,n) VALUES (s.id,s.n);",
    );
    assert_eq!(rowcount(&mut session), 2);
    assert_eq!(
        rows(&session, "SELECT count(*) FROM dbo.merge_top"),
        [[Some(5)]]
    );
    // Two source rows for one target: TOP (1) updates it once.
    ok(
        &mut session,
        "MERGE TOP (1) dbo.merge_top AS t USING (VALUES (1,11),(1,12)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n;",
    );
    assert_eq!(rowcount(&mut session), 1);
}

#[test]
fn invalid_clauses_fail_before_execution() {
    let (_server, mut session) = open();
    ok(
        &mut session,
        "CREATE TABLE dbo.merge_top(id INT NOT NULL PRIMARY KEY,n INT NOT NULL); INSERT dbo.merge_top VALUES (1,10)",
    );
    for (sql, number) in [
        (
            "MERGE dbo.merge_top AS t USING (VALUES (1,11)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n WHEN MATCHED AND s.n>0 THEN DELETE;",
            5324,
        ),
        (
            "MERGE dbo.merge_top AS t USING (VALUES (1,11)) AS s(id,n) ON t.id=s.id WHEN MATCHED AND s.n>0 THEN UPDATE SET n=s.n WHEN MATCHED THEN UPDATE SET n=s.n;",
            10714,
        ),
        (
            "MERGE dbo.merge_top AS t USING (VALUES (1,11)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN DELETE",
            10713,
        ),
        (
            "MERGE dbo.merge_top WITH (NOLOCK) AS t USING (VALUES (1,11)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN DELETE;",
            1065,
        ),
        (
            "MERGE dbo.merge_top WITH (bogus) AS t USING (VALUES (1,11)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN DELETE;",
            321,
        ),
        (
            "MERGE dbo.merge_missing AS t USING (VALUES (1,11)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN DELETE;",
            208,
        ),
        (
            "MERGE dbo.merge_top AS t USING (VALUES (1,11)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET missing=s.n;",
            207,
        ),
        (
            "MERGE dbo.merge_top AS t WITH (HOLDLOCK) USING (VALUES (1,11)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN DELETE;",
            156,
        ),
        (
            "MERGE dbo.merge_top AS t USING (VALUES (1,11)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n OUTPUT t.id;",
            4104,
        ),
        (
            "MERGE dbo.merge_top AS t USING (VALUES (1,11)) AS t(id,n) ON t.id=t.id WHEN MATCHED THEN DELETE;",
            1011,
        ),
        (
            "MERGE dbo.merge_top AS t USING (VALUES (1,11)) AS s(id,n) ON t.id=s.id WHEN NOT MATCHED THEN INSERT (id,n) VALUES (s.id);",
            109,
        ),
    ] {
        assert_eq!(run(&mut session, sql), (false, number), "{sql}");
    }
    assert_eq!(pairs(&session, "merge_top"), [(1, Some(10))]);
}

#[test]
fn snapshot_classification_identity_defaults_and_character_keys() {
    let (_server, mut session) = open();
    ok(
        &mut session,
        "CREATE TABLE dbo.people(id INT IDENTITY(10,5) PRIMARY KEY, name VARCHAR(20) NOT NULL, visits INT NOT NULL DEFAULT 0, note VARCHAR(5) NULL)",
    );
    ok(
        &mut session,
        "INSERT dbo.people(name) VALUES ('Ann'), ('Bob')",
    );
    // An update that changes the join key does not make the source row look
    // unmatched: classification uses the state before the statement.
    ok(
        &mut session,
        "MERGE dbo.people AS p USING (VALUES ('Ann', 3), ('Cy', 1)) AS s(name, visits) ON p.name = s.name WHEN MATCHED THEN UPDATE SET visits = p.visits + s.visits, name = UPPER(s.name) WHEN NOT MATCHED THEN INSERT (name) VALUES (s.name);",
    );
    assert_eq!(rowcount(&mut session), 2);
    let mut statement = session
        .db
        .prepare("SELECT id, CAST(name AS VARCHAR), visits FROM dbo.people ORDER BY id")
        .unwrap();
    let people = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, i32>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i32>(2)?,
            ))
        })
        .unwrap()
        .collect::<duckdb::Result<Vec<_>>>()
        .unwrap();
    let people = people
        .iter()
        .map(|(id, name, visits)| (*id, name.as_str(), *visits))
        .collect::<Vec<_>>();
    assert_eq!(people, [(10, "ANN", 3), (15, "Bob", 0), (20, "Cy", 0)]);
    // Storage conversion rejects a value too long for its column (2628).
    assert_eq!(
        run(
            &mut session,
            "MERGE dbo.people AS p USING (VALUES ('Bob')) AS s(name) ON p.name = s.name WHEN MATCHED THEN UPDATE SET note = 'too long';"
        ),
        (false, 2628)
    );
    // An explicit identity value and an identity update are rejected.
    assert_eq!(
        run(
            &mut session,
            "MERGE dbo.people AS p USING (VALUES ('Dee')) AS s(name) ON p.name = s.name WHEN NOT MATCHED THEN INSERT (id, name) VALUES (1, s.name);"
        ),
        (false, 544)
    );
    assert_eq!(
        run(
            &mut session,
            "MERGE dbo.people AS p USING (VALUES ('Bob')) AS s(name) ON p.name = s.name WHEN MATCHED THEN UPDATE SET id = 1;"
        ),
        (false, 8102)
    );
}

#[test]
fn source_tables_and_self_merge() {
    let (_server, mut session) = open();
    ok(
        &mut session,
        "CREATE TABLE dbo.stock(id INT PRIMARY KEY, qty INT NOT NULL); CREATE TABLE dbo.delta(id INT, qty INT); INSERT dbo.stock VALUES (1,5),(2,6); INSERT dbo.delta VALUES (2,1),(3,7)",
    );
    ok(
        &mut session,
        "MERGE dbo.stock USING dbo.delta ON stock.id = delta.id WHEN MATCHED THEN UPDATE SET qty = stock.qty + delta.qty WHEN NOT MATCHED THEN INSERT VALUES (delta.id, delta.qty) WHEN NOT MATCHED BY SOURCE THEN DELETE;",
    );
    assert_eq!(rowcount(&mut session), 3);
    assert_eq!(
        rows(&session, "SELECT id, qty FROM dbo.stock ORDER BY id"),
        [[Some(2), Some(7)], [Some(3), Some(7)]]
    );
    // The source reads the target's state before the statement.
    ok(
        &mut session,
        "MERGE dbo.stock AS t USING (SELECT id + 1 AS id, qty FROM dbo.stock) AS s ON t.id = s.id WHEN MATCHED THEN UPDATE SET qty = s.qty WHEN NOT MATCHED THEN INSERT (id, qty) VALUES (s.id, s.qty);",
    );
    assert_eq!(
        rows(&session, "SELECT id, qty FROM dbo.stock ORDER BY id"),
        [[Some(2), Some(7)], [Some(3), Some(7)], [Some(4), Some(7)]]
    );
}

#[test]
fn delete_and_reinsert_of_the_same_key() {
    let (_server, mut session) = open();
    ok(
        &mut session,
        "CREATE TABLE dbo.k(id INT PRIMARY KEY, v INT); INSERT dbo.k VALUES (1, 1), (2, 2)",
    );
    // Row 2 is deleted by source while a source row inserts key 3 and the
    // matched row moves its key; all of it is one statement.
    ok(
        &mut session,
        "MERGE dbo.k AS t USING (VALUES (1, 10), (3, 30)) AS s(id, v) ON t.id = s.id WHEN MATCHED THEN UPDATE SET id = 2, v = s.v WHEN NOT MATCHED BY TARGET THEN INSERT (id, v) VALUES (s.id, s.v) WHEN NOT MATCHED BY SOURCE THEN DELETE;",
    );
    assert_eq!(
        rows(&session, "SELECT id, v FROM dbo.k ORDER BY id"),
        [[Some(2), Some(10)], [Some(3), Some(30)]]
    );
}

#[test]
fn constraint_failures_inside_a_transaction_keep_it_usable() {
    let (_server, mut session) = open();
    ok(
        &mut session,
        "CREATE TABLE dbo.t(id INT NOT NULL PRIMARY KEY, n INT NOT NULL CHECK (n > 0)); CREATE TABLE dbo.prior(id INT); INSERT dbo.t VALUES (1,10),(2,20)",
    );
    ok(
        &mut session,
        "BEGIN TRANSACTION; INSERT dbo.prior VALUES (1)",
    );
    for (sql, number) in [
        (
            "MERGE dbo.t AS t USING (VALUES (1,-1),(9,9)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n WHEN NOT MATCHED THEN INSERT (id,n) VALUES (s.id,s.n);",
            547,
        ),
        (
            "MERGE dbo.t AS t USING (VALUES (1,CAST(NULL AS INT))) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n;",
            515,
        ),
        (
            "MERGE dbo.t AS t USING (VALUES (1,2)) AS s(id,k) ON t.id=s.id WHEN MATCHED THEN UPDATE SET id=s.k;",
            2627,
        ),
        (
            "MERGE dbo.t AS t USING (VALUES (8,1),(8,2)) AS s(id,n) ON t.id=s.id WHEN NOT MATCHED THEN INSERT (id,n) VALUES (s.id,s.n);",
            2627,
        ),
    ] {
        // The statement ends; the batch and the transaction continue.
        let (_, error) = run(
            &mut session,
            &format!("{sql} INSERT dbo.prior VALUES ({number})"),
        );
        assert_eq!(error, 0, "{sql}");
        assert_eq!(session.transactions, 1, "{sql}");
    }
    // Swapping keys inside one statement is not a violation.
    ok(
        &mut session,
        "MERGE dbo.t AS t USING (VALUES (1,2),(2,1)) AS s(id,k) ON t.id=s.id WHEN MATCHED THEN UPDATE SET id=s.k;",
    );
    ok(&mut session, "COMMIT TRANSACTION");
    assert_eq!(pairs(&session, "t"), [(1, Some(20)), (2, Some(10))]);
    assert_eq!(
        rows(&session, "SELECT id FROM dbo.prior ORDER BY id"),
        [
            [Some(1)],
            [Some(515)],
            [Some(547)],
            [Some(2627)],
            [Some(2627)]
        ]
    );
}

#[test]
fn repeated_deletes_of_one_target_row_delete_it_once() {
    let (_server, mut session) = open();
    ok(
        &mut session,
        "CREATE TABLE dbo.t(id INT NOT NULL PRIMARY KEY, n INT NOT NULL); INSERT dbo.t VALUES (1,10),(2,20),(3,30)",
    );
    ok(
        &mut session,
        "MERGE dbo.t AS t USING (VALUES (1),(1),(2)) AS s(id) ON t.id=s.id WHEN MATCHED THEN DELETE;",
    );
    assert_eq!(rowcount(&mut session), 2);
    assert_eq!(pairs(&session, "t"), [(3, Some(30))]);
    // An UPDATE among the actions is still an error.
    ok(&mut session, "INSERT dbo.t VALUES (1,10)");
    assert_eq!(
        run(
            &mut session,
            "MERGE dbo.t AS t USING (VALUES (1,17),(1,18)) AS s(id,n) ON t.id=s.id WHEN MATCHED AND s.n=17 THEN UPDATE SET n=s.n WHEN MATCHED THEN DELETE;"
        ),
        (false, 8672)
    );
    assert_eq!(pairs(&session, "t"), [(1, Some(10)), (3, Some(30))]);
}

#[test]
fn computed_columns_are_derived_from_the_new_image() {
    let (_server, mut session) = open();
    ok(
        &mut session,
        "CREATE TABLE dbo.c(id INT NOT NULL PRIMARY KEY, n INT NOT NULL, twice AS n * 2); CREATE TABLE dbo.log(id INT, twice INT); INSERT dbo.c(id,n) VALUES (1,1)",
    );
    ok(
        &mut session,
        "MERGE dbo.c AS c USING (VALUES (1,5),(2,7)) AS s(id,n) ON c.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n WHEN NOT MATCHED THEN INSERT (id,n) VALUES (s.id,s.n) OUTPUT inserted.id, inserted.twice INTO dbo.log(id, twice);",
    );
    assert_eq!(
        rows(&session, "SELECT id, twice FROM dbo.log ORDER BY id"),
        [[Some(1), Some(10)], [Some(2), Some(14)]]
    );
    assert_eq!(
        run(
            &mut session,
            "MERGE dbo.c AS c USING (VALUES (1,2)) AS s(id,n) ON c.id=s.id WHEN MATCHED THEN UPDATE SET twice=s.n;"
        ),
        (false, 271)
    );
}

#[test]
fn insert_default_values() {
    let (_server, mut session) = open();
    ok(
        &mut session,
        "CREATE TABLE dbo.d(id INT IDENTITY(1,1) PRIMARY KEY, n INT NOT NULL DEFAULT 7, note VARCHAR(10) NULL)",
    );
    ok(
        &mut session,
        "MERGE dbo.d AS d USING (VALUES (1),(2)) AS s(k) ON 1 = 0 WHEN NOT MATCHED THEN INSERT DEFAULT VALUES;",
    );
    assert_eq!(rowcount(&mut session), 2);
    assert_eq!(
        rows(&session, "SELECT id, n, note FROM dbo.d ORDER BY id"),
        [[Some(1), Some(7), None], [Some(2), Some(7), None]]
    );
}
