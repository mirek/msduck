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

#[test]
fn failures_inside_transactions_never_commit_partial_effects() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    // A user column named RowId hides DuckDB's row id: a failed INSERT
    // cannot be undone by row id, so the transaction is invalidated rather
    // than deleting existing rows.
    ok(
        &mut session,
        "CREATE TABLE r(RowId INT, v INT CONSTRAINT ck_r CHECK (v > 0)); INSERT r VALUES (5, 1)",
    );
    assert_eq!(caught(&mut session, "INSERT r VALUES (1, -1)").0, 547);
    ok(&mut session, "BEGIN TRANSACTION");
    ok(&mut session, "INSERT r VALUES (7, 2)");
    assert!(!run(&mut session, "INSERT r VALUES (2, -2)"));
    assert!(!run(&mut session, "SELECT 1"));
    ok(&mut session, "ROLLBACK");
    assert_eq!(
        ints(&session, "SELECT RowId, v FROM r ORDER BY RowId"),
        [[Some(5), Some(1)]]
    );

    // An action that cannot be applied after the statement wrote: the
    // caller's transaction cannot commit the parent change alone.
    ok(&mut session, "CREATE TABLE p(id INT PRIMARY KEY)");
    ok(
        &mut session,
        "CREATE TABLE c(id INT PRIMARY KEY, pid INT CONSTRAINT uq_c UNIQUE CONSTRAINT fk_c REFERENCES p(id) ON UPDATE SET NULL)",
    );
    ok(
        &mut session,
        "CREATE TABLE g(id INT PRIMARY KEY, cpid INT CONSTRAINT fk_g REFERENCES c(pid) ON UPDATE CASCADE)",
    );
    ok(
        &mut session,
        "INSERT p VALUES (1); INSERT c VALUES (10, 1); INSERT g VALUES (100, 1)",
    );
    ok(&mut session, "BEGIN TRANSACTION");
    assert!(!run(&mut session, "UPDATE p SET id = 2"));
    assert!(!run(&mut session, "SELECT 1"));
    // Like after a native constraint error, COMMIT ends the transaction
    // without its changes.
    let _ = run(&mut session, "COMMIT");
    assert_eq!(ints(&session, "SELECT id FROM p"), [[Some(1)]]);
    assert_eq!(
        ints(&session, "SELECT id, pid FROM c"),
        [[Some(10), Some(1)]]
    );
    assert_eq!(
        ints(&session, "SELECT id, cpid FROM g"),
        [[Some(100), Some(1)]]
    );

    // New key values must be the ones the UPDATE writes.
    ok(
        &mut session,
        "CREATE TABLE k(id UNIQUEIDENTIFIER PRIMARY KEY)",
    );
    ok(
        &mut session,
        "CREATE TABLE kc(id INT, kid UNIQUEIDENTIFIER CONSTRAINT fk_kc REFERENCES k(id) ON UPDATE CASCADE)",
    );
    ok(
        &mut session,
        "INSERT k VALUES ('00000000-0000-0000-0000-000000000001'); INSERT kc VALUES (1, '00000000-0000-0000-0000-000000000001')",
    );
    assert!(!run(&mut session, "UPDATE k SET id = NEWID()"));
    assert_eq!(
        texts(&session, "SELECT CAST(kid AS VARCHAR) FROM kc"),
        ["00000000-0000-0000-0000-000000000001"]
    );
    assert_eq!(
        ints(
            &session,
            "SELECT count(*) FROM k WHERE CAST(id AS VARCHAR) = '00000000-0000-0000-0000-000000000001'"
        ),
        [[Some(1)]]
    );
}

#[test]
fn foreign_keys_reference_keys_of_every_storage() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    // An NVARCHAR primary key is enforced by the keys feature's own index;
    // foreign keys and their actions still reference it.
    ok(
        &mut session,
        "CREATE TABLE p(code NVARCHAR(10) NOT NULL CONSTRAINT pk_p PRIMARY KEY)",
    );
    ok(
        &mut session,
        "CREATE TABLE q(id INT, code NVARCHAR(10) CONSTRAINT fk_q REFERENCES p(code) ON DELETE CASCADE)",
    );
    ok(
        &mut session,
        "INSERT p VALUES (N'x'), (N'y'); INSERT q VALUES (1, N'x'), (2, N'y')",
    );
    assert_eq!(caught(&mut session, "INSERT q VALUES (3, N'z')").0, 547);
    // (nvarchar comparison with a literal is not lowered yet; compare columns)
    ok(
        &mut session,
        "DELETE p WHERE code IN (SELECT code FROM q WHERE id = 1)",
    );
    assert_eq!(ints(&session, "SELECT id FROM q"), [[Some(2)]]);
    assert_eq!(
        caught(&mut session, "ALTER TABLE p DROP CONSTRAINT pk_p").0,
        3727
    );
    assert_eq!(
        texts(
            &session,
            "SELECT name FROM sys.key_constraints WHERE parent_object_id = __msduck_object_id('p','U')"
        ),
        ["pk_p"]
    );
    // A self-reference to a key the keys feature records after CREATE TABLE.
    ok(
        &mut session,
        "CREATE TABLE emp(id NVARCHAR(10) NOT NULL CONSTRAINT pk_emp PRIMARY KEY, boss NVARCHAR(10) CONSTRAINT fk_emp REFERENCES emp(id))",
    );
    ok(&mut session, "INSERT emp VALUES (N'a', NULL), (N'b', N'a')");
    assert_eq!(
        caught(&mut session, "INSERT emp VALUES (N'c', N'z')").0,
        547
    );
    assert_eq!(caught(&mut session, "DELETE emp WHERE boss IS NULL").0, 547);
    // A unique index is not a candidate key: DROP INDEX cannot see foreign keys.
    ok(
        &mut session,
        "CREATE TABLE ux(code INT NOT NULL); CREATE UNIQUE INDEX ix_ux ON ux(code)",
    );
    assert_eq!(
        caught(
            &mut session,
            "CREATE TABLE uxc(code INT REFERENCES ux(code))"
        )
        .0,
        1750
    );
    // Dropping a constraint and a column of a table with indexes.
    ok(
        &mut session,
        "CREATE TABLE mixed(id INT NOT NULL, x INT, v INT CONSTRAINT ck_mixed CHECK (v > 0), w INT); CREATE INDEX ix_mixed ON mixed(w)",
    );
    ok(
        &mut session,
        "ALTER TABLE mixed DROP CONSTRAINT ck_mixed, COLUMN x",
    );
    ok(&mut session, "INSERT mixed(id, v, w) VALUES (1, -1, 1)");
    // ALTER TABLE adds keys as native DuckDB constraints, which cannot cover
    // that storage: the statement fails explicitly and changes nothing.
    ok(
        &mut session,
        "CREATE TABLE u(id INT NOT NULL, code NVARCHAR(10) NOT NULL)",
    );
    assert!(!run(
        &mut session,
        "ALTER TABLE u ADD CONSTRAINT uq_u UNIQUE (code)"
    ));
    assert_eq!(
        ints(
            &session,
            "SELECT count(*) FROM sys.key_constraints WHERE parent_object_id = __msduck_object_id('u','U')"
        ),
        [[Some(0)]]
    );
    ok(
        &mut session,
        "ALTER TABLE u ADD CONSTRAINT pk_u PRIMARY KEY (id)",
    );
    assert_eq!(
        texts(
            &session,
            "SELECT name FROM sys.key_constraints WHERE parent_object_id = __msduck_object_id('u','U')"
        ),
        ["pk_u"]
    );
}

/// `name, delete action, update action, is_system_named` of `table`'s keys.
fn foreign_keys(session: &Session, table: &str) -> Vec<String> {
    texts(
        session,
        &format!(
            "SELECT concat_ws(',', CASE WHEN is_system_named THEN 'system' ELSE f.name END, delete_referential_action_desc, update_referential_action_desc, pc.name || '->' || rc.name) FROM sys.foreign_keys f JOIN sys.foreign_key_columns k ON k.constraint_object_id = f.object_id JOIN sys.columns pc ON pc.object_id = k.parent_object_id AND pc.column_id = k.parent_column_id JOIN sys.columns rc ON rc.object_id = k.referenced_object_id AND rc.column_id = k.referenced_column_id WHERE f.parent_object_id = __msduck_object_id('{table}','U') ORDER BY 1"
        ),
    )
}

#[test]
fn column_foreign_key_words_behave_like_references() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    ok(&mut session, "CREATE TABLE p(id INT PRIMARY KEY)");
    ok(
        &mut session,
        "CREATE TABLE c(id INT PRIMARY KEY, pid INT FOREIGN KEY REFERENCES p(id) ON UPDATE CASCADE ON DELETE CASCADE)",
    );
    ok(
        &mut session,
        "CREATE TABLE d(id INT PRIMARY KEY, pid INT CONSTRAINT fk2 FOREIGN KEY REFERENCES p(id) ON DELETE SET NULL NOT FOR REPLICATION)",
    );
    ok(
        &mut session,
        "CREATE TABLE r(id INT PRIMARY KEY, pid INT REFERENCES p(id) ON UPDATE CASCADE ON DELETE CASCADE)",
    );
    assert_eq!(
        foreign_keys(&session, "c"),
        ["system,CASCADE,CASCADE,pid->id"]
    );
    assert_eq!(foreign_keys(&session, "c"), foreign_keys(&session, "r"));
    assert_eq!(
        foreign_keys(&session, "d"),
        ["fk2,SET_NULL,NO_ACTION,pid->id"]
    );
    ok(
        &mut session,
        "INSERT p VALUES (1), (2); INSERT c VALUES (10, 1), (20, 2); INSERT d VALUES (10, 2), (20, NULL)",
    );
    let (number, message) = caught(&mut session, "INSERT d VALUES (30, 7)");
    assert_eq!(number, 547);
    assert_eq!(
        message,
        "The INSERT statement conflicted with the FOREIGN KEY constraint \"fk2\". The conflict occurred in database \"master\", table \"dbo.p\", column 'id'."
    );
    assert_eq!(caught(&mut session, "INSERT c VALUES (30, 7)").0, 547);
    ok(&mut session, "UPDATE p SET id = 101 WHERE id = 1");
    assert_eq!(
        ints(&session, "SELECT id, pid FROM c ORDER BY id"),
        [[Some(10), Some(101)], [Some(20), Some(2)]]
    );
    // d's key has no update action.
    assert_eq!(
        caught(&mut session, "UPDATE p SET id = 102 WHERE id = 2").0,
        547
    );
    ok(&mut session, "DELETE p WHERE id = 2");
    assert_eq!(
        ints(&session, "SELECT id, pid FROM d ORDER BY id"),
        [[Some(10), None], [Some(20), None]]
    );
    assert_eq!(
        ints(&session, "SELECT id, pid FROM c ORDER BY id"),
        [[Some(10), Some(101)]]
    );

    // ALTER TABLE ADD column, with and without a name.
    ok(&mut session, "CREATE TABLE a(id INT PRIMARY KEY)");
    ok(&mut session, "INSERT a VALUES (1)");
    ok(
        &mut session,
        "ALTER TABLE a ADD pid INT CONSTRAINT fk_a FOREIGN KEY REFERENCES p(id) ON DELETE CASCADE, qid INT FOREIGN KEY REFERENCES p NOT FOR REPLICATION",
    );
    assert_eq!(
        foreign_keys(&session, "a"),
        [
            "fk_a,CASCADE,NO_ACTION,pid->id",
            "system,NO_ACTION,NO_ACTION,qid->id"
        ]
    );
    ok(&mut session, "UPDATE a SET pid = 101, qid = 101");
    assert_eq!(caught(&mut session, "UPDATE a SET qid = 9").0, 547);
    ok(&mut session, "UPDATE a SET pid = NULL");
    assert_eq!(caught(&mut session, "DELETE p WHERE id = 101").0, 547);
    assert_eq!(ints(&session, "SELECT count(*) FROM c"), [[Some(1)]]);
    ok(
        &mut session,
        "UPDATE a SET pid = 101, qid = NULL; DELETE p WHERE id = 101",
    );
    assert_eq!(ints(&session, "SELECT count(*) FROM a"), [[Some(0)]]);
    assert_eq!(ints(&session, "SELECT count(*) FROM c"), [[Some(0)]]);
}
