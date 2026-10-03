//! Correlated APPLY over FULL OUTER JOIN bodies with in-process sessions
//! (issue #867, docs/apply-full-join.md). Rows, error numbers, states and
//! classes follow reference/apply-full-join.json; the tedious view is covered
//! by tests/compat/apply_full_join.test.mjs.
use msduck::engine::Session;
use msduck::server::Server;

fn run(session: &mut Session, sql: &str) -> (Vec<u8>, bool) {
    session.batch_response(sql, &Default::default(), false, None)
}

fn ok(session: &mut Session, sql: &str) {
    let (tokens, success) = run(session, sql);
    assert!(success, "{sql}: {:?}", errors(&tokens));
}

/// ERROR tokens (number, state, class, message) up to the first result set.
fn errors(tokens: &[u8]) -> Vec<(i32, u8, u8, String)> {
    let mut found = Vec::new();
    let mut at = 0;
    while at < tokens.len() {
        match tokens[at] {
            kind @ (0xaa | 0xab) => {
                let length = u16::from_le_bytes([tokens[at + 1], tokens[at + 2]]) as usize;
                let body = &tokens[at + 3..at + 3 + length];
                let units = u16::from_le_bytes([body[6], body[7]]) as usize;
                let message: Vec<u16> = body[8..8 + units * 2]
                    .chunks(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect();
                if kind == 0xaa {
                    found.push((
                        i32::from_le_bytes(body[..4].try_into().unwrap()),
                        body[4],
                        body[5],
                        String::from_utf16_lossy(&message),
                    ));
                }
                at += 3 + length;
            }
            0xe3 => at += 3 + u16::from_le_bytes([tokens[at + 1], tokens[at + 2]]) as usize,
            0xfd..=0xff => at += 13,
            _ => break,
        }
    }
    found
}

/// Per-item summaries written by T-SQL into an INT table and read natively.
fn summary(session: &mut Session, apply: &str) -> Vec<String> {
    ok(session, "DELETE FROM summary");
    ok(
        session,
        &format!(
            "INSERT INTO summary SELECT i.id, COUNT(x.[key]),
               SUM(CASE WHEN x.old_value IS NOT NULL AND x.new_value IS NOT NULL THEN 1 ELSE 0 END),
               SUM(CASE WHEN x.new_value IS NULL AND x.[key] IS NOT NULL THEN 1 ELSE 0 END),
               SUM(CASE WHEN x.old_value IS NULL AND x.[key] IS NOT NULL THEN 1 ELSE 0 END),
               SUM(CAST(x.old_value AS INT)), SUM(CAST(x.new_value AS INT))
             FROM items i {apply} dbo.foo(i.lhs, i.rhs) x GROUP BY i.id"
        ),
    );
    let mut statement = session
        .db
        .prepare("SELECT id, n, matched, left_only, right_only, old_sum, new_sum FROM dbo.summary ORDER BY id")
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((0..7)
                .map(|column| {
                    row.get::<_, Option<i32>>(column)
                        .unwrap()
                        .map_or("-".to_string(), |value| value.to_string())
                })
                .collect::<Vec<_>>()
                .join(" "))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn session() -> (Server, Session) {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    ok(
        &mut session,
        r#"CREATE TABLE items(id INT NOT NULL PRIMARY KEY, lhs NVARCHAR(MAX) NULL, rhs NVARCHAR(MAX) NULL);
           INSERT INTO items VALUES (1, N'{"a":1,"b":2}', N'{"b":3,"c":4}'), (2, NULL, N'{"x":1}'),
             (3, N'{"y":5}', NULL), (4, NULL, NULL), (5, N'{}', N'{"z":7}');
           CREATE TABLE summary(id INT, n INT, matched INT, left_only INT, right_only INT, old_sum INT, new_sum INT);"#,
    );
    ok(
        &mut session,
        "CREATE FUNCTION dbo.foo(@lhs NVARCHAR(MAX), @rhs NVARCHAR(MAX)) RETURNS TABLE AS RETURN (SELECT COALESCE(l.[key], r.[key]) AS [key], l.[value] AS old_value, r.[value] AS new_value FROM OPENJSON(@lhs) l FULL OUTER JOIN OPENJSON(@rhs) r ON l.[key] = r.[key])",
    );
    (server, session)
}

#[test]
fn inline_function_full_join_runs_under_cross_and_outer_apply() {
    let (_server, mut session) = session();
    // id n matched left_only right_only old_sum new_sum
    assert_eq!(
        summary(&mut session, "CROSS APPLY"),
        [
            "1 3 1 1 1 3 7",
            "2 1 0 0 1 - 1",
            "3 1 0 1 0 5 -",
            "5 1 0 0 1 - 7"
        ]
    );
    assert_eq!(
        summary(&mut session, "OUTER APPLY"),
        [
            "1 3 1 1 1 3 7",
            "2 1 0 0 1 - 1",
            "3 1 0 1 0 5 -",
            "4 0 0 0 0 - -",
            "5 1 0 0 1 - 7"
        ]
    );
}

#[test]
fn errors_inside_the_rewritten_body_keep_their_sql_server_identity() {
    let (_server, mut session) = session();
    let (tokens, success) = run(
        &mut session,
        // INSERT ... SELECT so the error is not preceded by a result set.
        "INSERT INTO summary(id, n) SELECT i.id, x.v FROM items i CROSS APPLY (SELECT 1 / (LEN(COALESCE(l.[key], r.[key])) - 1) AS v FROM OPENJSON(i.lhs) l FULL JOIN OPENJSON(i.rhs) r ON l.[key] = r.[key]) x",
    );
    assert!(!success);
    assert_eq!(
        errors(&tokens),
        [(8134, 1, 16, "Divide by zero error encountered.".to_string())]
    );
}

#[test]
fn after_update_trigger_diffs_json_snapshots_through_the_function() {
    let (_server, mut session) = session();
    ok(
        &mut session,
        "CREATE TABLE docs(id INT NOT NULL PRIMARY KEY, name NVARCHAR(20) NULL, qty INT NULL, note NVARCHAR(20) NULL);
         CREATE TABLE audit(id INT NOT NULL, [key] NVARCHAR(4000) NULL, old_value NVARCHAR(MAX) NULL, new_value NVARCHAR(MAX) NULL);
         INSERT INTO docs VALUES (1, N'one', 10, NULL), (2, N'two', 20, N'x'), (3, N'three', 30, N'y');",
    );
    ok(
        &mut session,
        "CREATE TRIGGER docs_audit ON docs AFTER UPDATE AS
         BEGIN
           SET NOCOUNT ON;
           INSERT INTO audit(id, [key], old_value, new_value)
           SELECT i.id, x.[key], x.old_value, x.new_value
           FROM inserted i
           JOIN deleted d ON d.id = i.id
           CROSS APPLY dbo.foo(
             (SELECT d.name, d.qty, d.note FOR JSON PATH, WITHOUT_ARRAY_WRAPPER),
             (SELECT i.name, i.qty, i.note FOR JSON PATH, WITHOUT_ARRAY_WRAPPER)) x
           WHERE x.old_value IS NULL OR x.new_value IS NULL OR x.old_value <> x.new_value;
         END",
    );
    ok(
        &mut session,
        "UPDATE docs SET qty = qty + 1, note = CASE id WHEN 2 THEN NULL ELSE N'z' END WHERE id IN (1, 2)",
    );
    // SQL Server: (1,note,NULL,z) (1,qty,10,11) (2,note,x,NULL) (2,qty,20,21).
    ok(
        &mut session,
        "INSERT INTO summary SELECT id, COUNT(*), SUM(CASE WHEN [key] = N'qty' THEN 1 ELSE 0 END),
           SUM(CASE WHEN new_value IS NULL THEN 1 ELSE 0 END), SUM(CASE WHEN old_value IS NULL THEN 1 ELSE 0 END),
           SUM(CASE WHEN [key] = N'qty' THEN CAST(old_value AS INT) END), SUM(CASE WHEN [key] = N'qty' THEN CAST(new_value AS INT) END)
         FROM audit GROUP BY id",
    );
    let rows: Vec<(i32, i32, i32, i32, i32, i32, i32)> = session
        .db
        .prepare("SELECT * FROM dbo.summary ORDER BY id")
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows, [(1, 2, 1, 0, 1, 10, 11), (2, 2, 1, 1, 0, 20, 21)]);

    // Unchanged rows produce no audit rows.
    ok(
        &mut session,
        "DELETE FROM audit; UPDATE docs SET name = name",
    );
    let count: i64 = session
        .db
        .query_row("SELECT COUNT(*) FROM dbo.audit", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}
