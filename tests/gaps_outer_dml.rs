//! UPDATE and DELETE whose FROM tree joins the target through LEFT, RIGHT or
//! FULL joins, APPLY or a nested join (issue #723, docs/gaps-outer_dml.md).
//! Expected rows, counts and error numbers come from SQL Server 2022
//! (reference/gaps-outer_dml.json); this checks the session directly, and
//! tests/compat/outer_dml.test.mjs checks the same cases over TDS.
use msduck::{engine::Session, server::Server};
use serde_json::Value;
use std::collections::HashMap;

const TABLES: &str = "
CREATE TABLE items(id INT NOT NULL PRIMARY KEY, value INT NULL, name VARCHAR(10) NOT NULL);
CREATE TABLE foo(id INT NOT NULL, value INT NULL, other INT NULL);
CREATE TABLE bar(id INT NOT NULL, label VARCHAR(10) NOT NULL);
INSERT INTO items VALUES (1, 10, 'a'), (2, 20, 'bb'), (3, 30, 'ccc'), (4, 40, 'dddd');
INSERT INTO foo VALUES (1, 100, 7), (3, 300, 9), (5, 500, 11);
INSERT INTO bar VALUES (7, 'seven'), (11, 'eleven');";

type Row = (i32, Option<i32>, String);

fn session() -> (Server, Session) {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    assert!(run(&mut session, TABLES));
    (server, session)
}

fn run(session: &mut Session, sql: &str) -> bool {
    session.batch_response(sql, &HashMap::new(), false, None).1
}

fn items(session: &Session) -> Vec<Row> {
    session
        .db
        .prepare("SELECT id, value, name FROM items ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap()
}

fn rows(values: &[(i32, Option<i32>, &str)]) -> Vec<Row> {
    values
        .iter()
        .map(|(id, value, name)| (*id, *value, name.to_string()))
        .collect()
}

/// Run one statement on fresh tables; return its @@ROWCOUNT and the rows.
fn changed(sql: &str) -> (u64, Vec<Row>) {
    let (_server, mut session) = session();
    assert!(run(&mut session, sql), "{sql}");
    assert_eq!(session.last_error, 0, "{sql}");
    (session.rowcount, items(&session))
}

/// Run one failing statement on fresh tables; return its error number and
/// the (unchanged) rows.
fn failed(sql: &str) -> (i32, Vec<Row>) {
    let (_server, mut session) = session();
    assert!(!run(&mut session, sql), "{sql}");
    (session.last_error, items(&session))
}

const ORIGINAL: [(i32, Option<i32>, &str); 4] = [
    (1, Some(10), "a"),
    (2, Some(20), "bb"),
    (3, Some(30), "ccc"),
    (4, Some(40), "dddd"),
];

#[test]
fn generic_left_join_update_and_anti_join_delete() {
    assert_eq!(
        changed(
            "UPDATE target SET value=COALESCE(source.value,0) FROM items target LEFT JOIN foo source ON source.id=target.id"
        ),
        (
            4,
            rows(&[
                (1, Some(100), "a"),
                (2, Some(0), "bb"),
                (3, Some(300), "ccc"),
                (4, Some(0), "dddd")
            ])
        )
    );
    assert_eq!(
        changed(
            "DELETE target FROM items target LEFT JOIN foo source ON source.id=target.id WHERE source.id IS NULL"
        ),
        (2, rows(&[(1, Some(10), "a"), (3, Some(30), "ccc")]))
    );
}

#[test]
fn right_full_and_null_extended_targets_change_only_present_rows() {
    let matched = rows(&[
        (1, Some(100), "a"),
        (2, Some(20), "bb"),
        (3, Some(300), "ccc"),
        (4, Some(40), "dddd"),
    ]);
    assert_eq!(
        changed("UPDATE t SET value = s.value FROM items t RIGHT JOIN foo s ON s.id = t.id"),
        (2, matched)
    );
    assert_eq!(
        changed(
            "UPDATE t SET value = ISNULL(s.value, -1) FROM items t FULL JOIN foo s ON s.id = t.id"
        ),
        (
            4,
            rows(&[
                (1, Some(100), "a"),
                (2, Some(-1), "bb"),
                (3, Some(300), "ccc"),
                (4, Some(-1), "dddd")
            ])
        )
    );
    assert_eq!(
        changed("UPDATE t SET value = s.value + 1 FROM foo s LEFT JOIN items t ON t.id = s.id"),
        (
            2,
            rows(&[
                (1, Some(101), "a"),
                (2, Some(20), "bb"),
                (3, Some(301), "ccc"),
                (4, Some(40), "dddd")
            ])
        )
    );
    assert_eq!(
        changed("DELETE t FROM items t RIGHT JOIN foo s ON s.id = t.id"),
        (2, rows(&[(2, Some(20), "bb"), (4, Some(40), "dddd")]))
    );
    assert_eq!(
        changed("DELETE t FROM items t FULL JOIN foo s ON s.id = t.id WHERE s.id IS NULL"),
        (2, rows(&[(1, Some(10), "a"), (3, Some(30), "ccc")]))
    );
}

#[test]
fn on_predicates_stay_in_the_join_and_where_filters_joined_rows() {
    assert_eq!(
        changed(
            "UPDATE t SET value = ISNULL(s.value, -1) FROM items t LEFT JOIN foo s ON s.id = t.id AND s.value > 200"
        ),
        (
            4,
            rows(&[
                (1, Some(-1), "a"),
                (2, Some(-1), "bb"),
                (3, Some(300), "ccc"),
                (4, Some(-1), "dddd")
            ])
        )
    );
    assert_eq!(
        changed(
            "UPDATE t SET value = 0 FROM items t LEFT JOIN foo s ON s.id = t.id WHERE t.id > 1 AND s.id IS NULL"
        ),
        (
            2,
            rows(&[
                (1, Some(10), "a"),
                (2, Some(0), "bb"),
                (3, Some(30), "ccc"),
                (4, Some(0), "dddd")
            ])
        )
    );
}

#[test]
fn apply_forms_update_and_delete_like_joins() {
    assert_eq!(
        changed(
            "UPDATE t SET value = ISNULL(x.v, 0) FROM items t OUTER APPLY (SELECT TOP (1) s.value AS v FROM foo s WHERE s.id = t.id ORDER BY s.value DESC) x"
        ),
        (
            4,
            rows(&[
                (1, Some(100), "a"),
                (2, Some(0), "bb"),
                (3, Some(300), "ccc"),
                (4, Some(0), "dddd")
            ])
        )
    );
    assert_eq!(
        changed(
            "UPDATE t SET value = x.v FROM items t CROSS APPLY (SELECT s.value + t.value AS v FROM foo s WHERE s.id = t.id) x"
        ),
        (
            2,
            rows(&[
                (1, Some(110), "a"),
                (2, Some(20), "bb"),
                (3, Some(330), "ccc"),
                (4, Some(40), "dddd")
            ])
        )
    );
    assert_eq!(
        changed(
            "DELETE t FROM items t OUTER APPLY (SELECT TOP (1) s.id FROM foo s WHERE s.id = t.id) x WHERE x.id IS NULL"
        ),
        (2, rows(&[(1, Some(10), "a"), (3, Some(30), "ccc")]))
    );
}

#[test]
fn rows_matched_several_times_change_once() {
    let extra = " INSERT INTO foo VALUES (3, 300, 9);";
    assert_eq!(
        changed(&format!(
            "{extra} UPDATE t SET value = ISNULL(s.value, 0) FROM items t LEFT JOIN foo s ON s.id = t.id"
        )),
        (
            4,
            rows(&[
                (1, Some(100), "a"),
                (2, Some(0), "bb"),
                (3, Some(300), "ccc"),
                (4, Some(0), "dddd")
            ])
        )
    );
    assert_eq!(
        changed(&format!(
            "{extra} DELETE t FROM items t LEFT JOIN foo s ON s.id = t.id WHERE s.id IS NOT NULL OR t.id = 4"
        )),
        (3, rows(&[(2, Some(20), "bb")]))
    );
}

#[test]
fn chained_nested_and_self_joins() {
    assert_eq!(
        changed(
            "UPDATE t SET name = ISNULL(b.label, 'none') FROM items t LEFT JOIN (foo s JOIN bar b ON b.id = s.other) ON s.id = t.id"
        ),
        (
            4,
            rows(&[
                (1, Some(10), "seven"),
                (2, Some(20), "none"),
                (3, Some(30), "none"),
                (4, Some(40), "none")
            ])
        )
    );
    assert_eq!(
        changed(
            "UPDATE t SET name = b.label FROM items t LEFT JOIN foo s ON s.id = t.id INNER JOIN bar b ON b.id = s.other"
        ),
        (
            1,
            rows(&[
                (1, Some(10), "seven"),
                (2, Some(20), "bb"),
                (3, Some(30), "ccc"),
                (4, Some(40), "dddd")
            ])
        )
    );
    assert_eq!(
        changed(
            "UPDATE t SET value = ISNULL(p.value, 0) FROM items t LEFT JOIN items p ON p.id = t.id - 1"
        ),
        (
            4,
            rows(&[
                (1, Some(0), "a"),
                (2, Some(10), "bb"),
                (3, Some(20), "ccc"),
                (4, Some(30), "dddd")
            ])
        )
    );
}

#[test]
fn unqualified_qualified_compound_and_default_assignments() {
    assert_eq!(
        changed(
            "UPDATE t SET value = LEN(name) * 1000 + ISNULL(other, 0) FROM items t LEFT JOIN foo s ON s.id = t.id"
        ),
        (
            4,
            rows(&[
                (1, Some(1007), "a"),
                (2, Some(2000), "bb"),
                (3, Some(3009), "ccc"),
                (4, Some(4000), "dddd")
            ])
        )
    );
    assert_eq!(
        changed(
            "UPDATE t SET t.value = COALESCE(s.other, t.value) FROM items t LEFT JOIN foo s ON s.id = t.id"
        ),
        (
            4,
            rows(&[
                (1, Some(7), "a"),
                (2, Some(20), "bb"),
                (3, Some(9), "ccc"),
                (4, Some(40), "dddd")
            ])
        )
    );
    assert_eq!(
        changed(
            "UPDATE t SET value += ISNULL(s.value, 0) FROM items t LEFT JOIN foo s ON s.id = t.id"
        ),
        (
            4,
            rows(&[
                (1, Some(110), "a"),
                (2, Some(20), "bb"),
                (3, Some(330), "ccc"),
                (4, Some(40), "dddd")
            ])
        )
    );
    assert_eq!(
        changed(
            "UPDATE t SET value = DEFAULT FROM items t LEFT JOIN foo s ON s.id = t.id WHERE s.id IS NULL"
        ),
        (
            2,
            rows(&[
                (1, Some(10), "a"),
                (2, None, "bb"),
                (3, Some(30), "ccc"),
                (4, None, "dddd")
            ])
        )
    );
    assert_eq!(
        changed(
            "UPDATE t SET id = t.id + 10 FROM items t LEFT JOIN foo s ON s.id = t.id WHERE s.id IS NULL"
        ),
        (
            2,
            rows(&[
                (1, Some(10), "a"),
                (3, Some(30), "ccc"),
                (12, Some(20), "bb"),
                (14, Some(40), "dddd")
            ])
        )
    );
}

#[test]
fn errors_keep_sql_server_numbers_and_leave_rows_unchanged() {
    let original = rows(&ORIGINAL);
    for (sql, number) in [
        (
            "UPDATE t SET value = value FROM items t LEFT JOIN foo s ON s.id = t.id",
            209,
        ),
        (
            "UPDATE t SET value = s.missing FROM items t LEFT JOIN foo s ON s.id = t.id",
            207,
        ),
        (
            "UPDATE t SET value = nosuch.value FROM items t LEFT JOIN foo s ON s.id = t.id",
            4104,
        ),
        (
            "UPDATE t SET value = 1 / (s.value - 100) FROM items t LEFT JOIN foo s ON s.id = t.id",
            8134,
        ),
    ] {
        assert_eq!(failed(sql), (number, original.clone()), "{sql}");
    }
    // A divide-by-zero error ends only the statement, as in SQL Server.
    let (_server, mut session) = session();
    run(
        &mut session,
        "UPDATE t SET value = 1 / (s.value - 100) FROM items t LEFT JOIN foo s ON s.id = t.id; INSERT INTO bar VALUES (@@ROWCOUNT, 'after')",
    );
    let after: i32 = session
        .db
        .query_row("SELECT id FROM bar WHERE label = 'after'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(after, 0);
}

#[test]
fn transactions_roll_back_outer_join_writes() {
    let (_server, mut session) = session();
    assert!(run(
        &mut session,
        "BEGIN TRANSACTION; UPDATE t SET value = COALESCE(s.value, 0) FROM items t LEFT JOIN foo s ON s.id = t.id; DELETE t FROM items t LEFT JOIN foo s ON s.id = t.id WHERE s.id IS NULL; ROLLBACK"
    ));
    assert_eq!(items(&session), rows(&ORIGINAL));
}

#[test]
fn preparation_binds_without_writing() {
    let (_server, session) = session();
    let declarations = [("@p".to_string(), msduck_core::types::Type::Int)];
    for sql in [
        "UPDATE t SET value = ISNULL(s.value, @p) FROM items t LEFT JOIN foo s ON s.id = t.id",
        "DELETE t FROM items t LEFT JOIN foo s ON s.id = t.id WHERE s.id IS NULL AND name <> 'x' AND t.id > @p",
    ] {
        session.validate_prepared_sql(sql, &declarations).unwrap();
    }
    assert_eq!(items(&session), rows(&ORIGINAL));
}

#[test]
fn reference_capture_matches_the_session_readback() {
    let reference: Value =
        serde_json::from_str(include_str!("../reference/gaps-outer_dml.json")).unwrap();
    let cases = reference["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 41);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let server = Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        assert!(run(&mut session, case["setup"].as_str().unwrap()), "{name}");
        let expected_error = case["result"]["dml"]["errors"]
            .as_array()
            .unwrap()
            .first()
            .map(|error| error["number"].as_i64().unwrap() as i32);
        // Without the trailing SELECT @@ROWCOUNT, @@ERROR keeps the error.
        let dml = case["dml"].as_str().unwrap();
        run(
            &mut session,
            dml.strip_suffix(" SELECT @@ROWCOUNT AS row_count;")
                .unwrap(),
        );
        if let Some(number) = expected_error {
            assert_eq!(session.last_error, number, "{name}");
        }
        let expected = &case["result"]["readback"]["sets"][0]["rows"];
        let actual = items(&session)
            .into_iter()
            .map(|(id, value, name)| serde_json::json!([id, value, name]))
            .collect::<Vec<_>>();
        assert_eq!(&Value::Array(actual), expected, "{name}");
    }
}
