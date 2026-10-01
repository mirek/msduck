//! In-process coverage for ALTER TABLE constraints and foreign-key actions
//! (docs/gaps-constraints.md): stored constraints survive a restart and work
//! in user databases, key rebuilds keep table identity, and errors carry
//! SQL Server numbers. Client-level coverage, including a replay of the SQL
//! Server reference, is in tests/compat/constraints.test.mjs.
use msduck::engine::Session;
use msduck::server::Server;

fn run(session: &mut Session, sql: &str) -> bool {
    session
        .batch_response(sql, &Default::default(), false, None)
        .1
}

fn ok(session: &mut Session, sql: &str) {
    assert!(run(session, sql), "{sql}");
}

/// The number and message SQL Server's TRY...CATCH reports for `sql`.
fn caught(session: &mut Session, sql: &str) -> (i32, String) {
    ok(
        session,
        "IF OBJECT_ID('dbo.caught') IS NULL CREATE TABLE dbo.caught(n INT, m VARCHAR(4000)); DELETE dbo.caught",
    );
    ok(
        session,
        &format!(
            "BEGIN TRY {sql} END TRY BEGIN CATCH INSERT dbo.caught VALUES (ERROR_NUMBER(), ERROR_MESSAGE()) END CATCH"
        ),
    );
    session
        .db
        .query_row("SELECT n, m FROM dbo.caught", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap_or_else(|error| panic!("{sql} did not fail: {error}"))
}

fn ints(session: &Session, sql: &str) -> Vec<Vec<Option<i64>>> {
    let mut statement = session.db.prepare(sql).unwrap();
    let mut rows = statement.query([]).unwrap();
    let mut result = Vec::new();
    while let Some(row) = rows.next().unwrap() {
        let count = row.as_ref().column_count();
        result.push(
            (0..count)
                .map(|i| row.get::<_, Option<i64>>(i).unwrap())
                .collect(),
        );
    }
    result
}

fn texts(session: &Session, sql: &str) -> Vec<String> {
    let mut statement = session.db.prepare(sql).unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// Catalog names of the table's ordinary indexes, which must be live natively.
fn indexes(session: &Session) -> Vec<String> {
    texts(
        session,
        "SELECT c.name FROM main.__msduck_index_catalog c JOIN duckdb_indexes() i ON i.index_name = c.backend_name WHERE i.table_name = 'items' AND c.object_id = __msduck_object_id('items','U')",
    )
}

fn temporary_directory(name: &str) -> std::path::PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "msduck-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

#[test]
fn constraints_survive_restart_in_user_databases() {
    let directory = temporary_directory("constraints-restart");
    let path = directory.join("primary.duckdb");
    let path = path.to_str().unwrap();
    {
        let server = Server::open(path).unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        ok(&mut session, "CREATE DATABASE shop");
        session.use_database("shop").unwrap();
        ok(
            &mut session,
            "CREATE TABLE orders(id INT PRIMARY KEY, total INT CONSTRAINT ck_total CHECK (total >= 0))",
        );
        ok(
            &mut session,
            "CREATE TABLE lines(id INT PRIMARY KEY, order_id INT, CONSTRAINT fk_lines FOREIGN KEY (order_id) REFERENCES orders(id) ON DELETE CASCADE ON UPDATE CASCADE)",
        );
        ok(
            &mut session,
            "ALTER TABLE lines ADD qty INT NOT NULL CONSTRAINT df_qty DEFAULT 1 WITH VALUES",
        );
        ok(
            &mut session,
            "ALTER TABLE lines WITH NOCHECK ADD CONSTRAINT ck_qty CHECK (qty > 0)",
        );
        ok(
            &mut session,
            "INSERT orders VALUES (1, 10), (2, 20); INSERT lines(id, order_id) VALUES (10, 1), (11, 1), (20, 2)",
        );
    }
    // Bootstrap runs again on every start: catalogs and enforcement return.
    for _ in 0..2 {
        let server = Server::open(path).unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        session.use_database("shop").unwrap();
        assert_eq!(
            texts(
                &session,
                "SELECT name FROM sys.objects WHERE rtrim(type) IN ('C','F','PK','D') AND name NOT LIKE 'PK%' ORDER BY name"
            ),
            ["ck_qty", "ck_total", "df_qty", "fk_lines"]
        );
        assert_eq!(
            ints(
                &session,
                "SELECT CAST(is_not_trusted AS INT) FROM sys.check_constraints WHERE name = 'ck_qty'"
            ),
            [[Some(1)]]
        );
        assert_eq!(caught(&mut session, "INSERT orders VALUES (3, -1)").0, 547);
        assert_eq!(
            caught(&mut session, "INSERT lines(id, order_id) VALUES (30, 3)").0,
            547
        );
        assert_eq!(
            caught(
                &mut session,
                "INSERT lines(id, order_id, qty) VALUES (31, 1, 0)"
            )
            .0,
            547
        );
        assert_eq!(caught(&mut session, "DROP TABLE orders").0, 3726);
        assert_eq!(caught(&mut session, "TRUNCATE TABLE orders").0, 4712);
    }
    {
        let server = Server::open(path).unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        session.use_database("shop").unwrap();
        ok(&mut session, "UPDATE orders SET id = id + 100");
        assert_eq!(
            ints(&session, "SELECT id, order_id, qty FROM lines ORDER BY id"),
            [
                [Some(10), Some(101), Some(1)],
                [Some(11), Some(101), Some(1)],
                [Some(20), Some(102), Some(1)]
            ]
        );
        ok(&mut session, "DELETE orders WHERE id = 101");
        assert_eq!(
            ints(&session, "SELECT id FROM lines ORDER BY id"),
            [[Some(20)]]
        );
        // master has its own, empty, constraint catalog.
        session.use_database("master").unwrap();
        assert_eq!(
            ints(&session, "SELECT count(*) FROM main.__msduck_constraints"),
            [[Some(0)]]
        );
    }
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn key_rebuilds_keep_identity_indexes_and_rows() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    ok(
        &mut session,
        "CREATE TABLE items(id INT IDENTITY(10,5) NOT NULL, code VARCHAR(10) NOT NULL, note NVARCHAR(20) CONSTRAINT df_note DEFAULT N'none')",
    );
    ok(&mut session, "CREATE INDEX ix_code ON items(code)");
    ok(&mut session, "INSERT items(code) VALUES ('a'), ('b')");
    let before = ints(
        &session,
        "SELECT object_id, (SELECT max(column_id) FROM sys.columns WHERE object_id = __msduck_object_id('items','U')) FROM sys.objects WHERE name = 'items'",
    );
    ok(
        &mut session,
        "ALTER TABLE items ADD CONSTRAINT uq_code UNIQUE (code)",
    );
    ok(
        &mut session,
        "ALTER TABLE items ADD CONSTRAINT pk_items PRIMARY KEY (id)",
    );
    assert_eq!(
        before,
        ints(
            &session,
            "SELECT object_id, (SELECT max(column_id) FROM sys.columns WHERE object_id = __msduck_object_id('items','U')) FROM sys.objects WHERE name = 'items'",
        )
    );
    ok(&mut session, "INSERT items(code) VALUES ('c')");
    assert_eq!(
        ints(&session, "SELECT id FROM dbo.items ORDER BY id"),
        [[Some(10)], [Some(15)], [Some(20)]]
    );
    assert_eq!(
        caught(&mut session, "INSERT items(code) VALUES ('a')").0,
        2627
    );
    assert_eq!(indexes(&session), ["ix_code"]);
    ok(&mut session, "ALTER TABLE items DROP CONSTRAINT uq_code");
    ok(&mut session, "INSERT items(code) VALUES ('a')");
    assert_eq!(indexes(&session), ["ix_code"]);
    assert_eq!(
        texts(
            &session,
            "SELECT name FROM sys.key_constraints WHERE parent_object_id = __msduck_object_id('items','U')"
        ),
        ["pk_items"]
    );
    // The named default and the rows stay.
    assert_eq!(
        ints(
            &session,
            "SELECT count(*) FROM dbo.items WHERE note.__msduck_utf16le IS NOT NULL"
        ),
        [[Some(4)]]
    );
    assert_eq!(
        caught(
            &mut session,
            "ALTER TABLE items ADD CONSTRAINT pk_again PRIMARY KEY (code)"
        )
        .0,
        1750
    );
}

#[test]
fn referential_actions_follow_update_from_and_report_numbers() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    ok(
        &mut session,
        "CREATE TABLE p(id INT PRIMARY KEY, label VARCHAR(10))",
    );
    ok(
        &mut session,
        "CREATE TABLE c(id INT PRIMARY KEY, pid INT CONSTRAINT fk_c REFERENCES p(id) ON UPDATE CASCADE ON DELETE SET NULL)",
    );
    ok(&mut session, "CREATE TABLE shift(old INT, new INT)");
    ok(
        &mut session,
        "INSERT p VALUES (1, 'a'), (2, 'b'), (3, 'c'); INSERT c VALUES (10, 1), (20, 2), (30, 3); INSERT shift VALUES (1, 101), (2, 102)",
    );
    // UPDATE ... FROM with an alias target.
    ok(
        &mut session,
        "UPDATE t SET id = s.new FROM p t JOIN shift s ON s.old = t.id",
    );
    assert_eq!(
        ints(&session, "SELECT id, pid FROM c ORDER BY id"),
        [
            [Some(10), Some(101)],
            [Some(20), Some(102)],
            [Some(30), Some(3)]
        ]
    );
    ok(
        &mut session,
        "DECLARE @k INT = 3; UPDATE p SET id = @k * 100 WHERE id = @k",
    );
    assert_eq!(
        ints(&session, "SELECT pid FROM c WHERE id = 30"),
        [[Some(300)]]
    );
    ok(&mut session, "DELETE p WHERE label = 'a'");
    assert_eq!(ints(&session, "SELECT pid FROM c WHERE id = 10"), [[None]]);
    let (number, message) = caught(&mut session, "INSERT c VALUES (40, 7)");
    assert_eq!(number, 547);
    assert_eq!(
        message,
        "The INSERT statement conflicted with the FOREIGN KEY constraint \"fk_c\". The conflict occurred in database \"master\", table \"dbo.p\", column 'id'."
    );
    assert_eq!(
        caught(&mut session, "ALTER TABLE c DROP CONSTRAINT missing").0,
        3727
    );
    assert_eq!(
        caught(&mut session, "ALTER TABLE c NOCHECK CONSTRAINT missing").0,
        4916
    );
    ok(&mut session, "ALTER TABLE c NOCHECK CONSTRAINT fk_c");
    ok(&mut session, "INSERT c VALUES (40, 7)");
    // Disabled keys take no action.
    ok(&mut session, "DELETE p WHERE id = 102");
    assert_eq!(
        ints(&session, "SELECT pid FROM c WHERE id = 20"),
        [[Some(102)]]
    );
    assert_eq!(
        caught(
            &mut session,
            "ALTER TABLE c WITH CHECK CHECK CONSTRAINT fk_c"
        )
        .0,
        547
    );
}
