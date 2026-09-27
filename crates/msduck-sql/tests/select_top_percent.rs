//! SELECT TOP PERCENT and WITH TIES lowering over reference/select-top-percent.json.
use msduck_sql::top;
use sqlparser::{
    ast::{Query, SetExpr, Statement},
    parser::Parser,
};

fn query(sql: &str) -> Box<Query> {
    let mut statement = msduck_sql::batch::parse(sql).unwrap().remove(0);
    let Statement::Query(query) = &mut statement else {
        panic!("query expected: {sql}")
    };
    query.clone()
}

fn lower(sql: &str) -> Result<String, String> {
    let mut query = query(sql);
    top::ranked(&mut query)?;
    Ok(query.to_string())
}

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!("../../../reference/select-top-percent.json")).unwrap()
}

fn captured_error(case: &serde_json::Value) -> Option<(i64, i64, i64, String)> {
    let error = case["result"]["errors"].as_array().unwrap().first()?;
    Some((
        error["number"].as_i64().unwrap(),
        error["state"].as_i64().unwrap(),
        error["class"].as_i64().unwrap(),
        error["message"].as_str().unwrap().to_owned(),
    ))
}

#[test]
fn captured_compile_diagnostics_match_every_retained_run() {
    // Text conversion (8114) and runtime NULL parameters are execution errors.
    let compile = [
        "above full percent",
        "negative percent",
        "null percent",
        "ties without order",
        "percent ties without order",
        "negative count ties",
        "null count ties",
        "distinct invalid order",
    ];
    let fixture = fixture();
    let runs = fixture["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2);
    for run in runs {
        let mut seen = 0;
        for case in run.as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let sql = case["sql"].as_str().unwrap();
            if !sql.contains("TOP") || sql.contains('@') {
                continue;
            }
            let lowered = lower(sql);
            if compile.contains(&name) {
                seen += 1;
                let message = lowered.expect_err(name);
                let error = top::diagnostic(&message).expect(name);
                assert_eq!(
                    Some((
                        i64::from(error.number),
                        i64::from(error.state),
                        i64::from(error.severity),
                        error.message
                    )),
                    captured_error(case),
                    "{name}"
                );
            } else if name != "text percent" {
                assert!(lowered.is_ok(), "{name}: {lowered:?}");
                assert_eq!(captured_error(case), None, "{name}");
            }
        }
        assert_eq!(seen, compile.len());
    }
}

#[test]
fn runtime_count_diagnostics_share_captured_class() {
    for (message, number) in [
        (
            "Invalid Input Error: A TOP or FETCH clause contains an invalid value.",
            1014,
        ),
        (
            "Invalid Input Error: Percent values must be between 0 and 100.",
            1031,
        ),
    ] {
        let error = top::diagnostic(message).unwrap();
        assert_eq!((error.number, error.state, error.severity), (number, 1, 15));
        assert!(!error.message.starts_with("Invalid Input Error"));
    }
    assert!(top::diagnostic("Invalid Input Error: something else").is_none());
}

#[test]
fn percent_is_converted_once_and_bounds_rows_after_grouping() {
    let sql = lower(
        "SELECT TOP (@p) PERCENT score,COUNT(*) AS frequency FROM t GROUP BY score ORDER BY score DESC",
    )
    .unwrap();
    assert_eq!(sql.matches("@p").count(), 1, "{sql}");
    assert_eq!(sql.matches("CAST(@p AS DOUBLE)").count(), 1, "{sql}");
    assert!(
        sql.contains("GROUP BY score QUALIFY row_number() OVER (ORDER BY score DESC) <= ceil("),
        "{sql}"
    );
    assert!(sql.contains("count(*) OVER () / 100"), "{sql}");
    assert!(!sql.contains("SELECT TOP"), "{sql}");
    assert!(sql.ends_with("ORDER BY score DESC"), "{sql}");
}

#[test]
fn ties_rank_by_order_keys_and_resolve_aliases_and_ordinals() {
    let sql = lower("SELECT TOP (2) WITH TIES id,score AS s FROM t ORDER BY s DESC,2,id").unwrap();
    assert!(
        sql.contains("QUALIFY rank() OVER (ORDER BY score DESC, score, id) <= (SELECT __msduck_top_count(coalesce(2, -1)))"),
        "{sql}"
    );
    // The query ORDER BY keeps SQL Server name resolution for the final sort.
    assert!(sql.ends_with("ORDER BY s DESC, 2, id"), "{sql}");
    let sql = lower(
        "SELECT TOP (1) PERCENT WITH TIES COUNT(*) AS n FROM t GROUP BY g ORDER BY COUNT(*) DESC",
    )
    .unwrap();
    assert!(
        sql.contains("rank() OVER (ORDER BY COUNT(*) DESC)"),
        "{sql}"
    );
}

#[test]
fn distinct_ranks_derived_output_columns() {
    let sql =
        lower("SELECT DISTINCT TOP (25) PERCENT WITH TIES score AS s FROM t ORDER BY score DESC")
            .unwrap();
    assert!(
        sql.starts_with("SELECT * FROM (SELECT DISTINCT score AS s FROM t) AS __msduck_top_distinct QUALIFY rank() OVER (ORDER BY s DESC)"),
        "{sql}"
    );
    assert!(sql.ends_with("ORDER BY s DESC"), "{sql}");
    let sql =
        lower("SELECT DISTINCT TOP (50) PERCENT t.score INTO copy FROM t ORDER BY 1").unwrap();
    assert!(
        sql.starts_with("SELECT * INTO copy FROM (SELECT DISTINCT t.score FROM t)"),
        "{sql}"
    );
    assert!(sql.ends_with("ORDER BY score"), "{sql}");
}

#[test]
fn unsupported_shapes_remain_explicit() {
    for (sql, expected) in [
        (
            "SELECT DISTINCT TOP (1) PERCENT score + 1 FROM t ORDER BY score + 1",
            "unnamed",
        ),
        (
            "SELECT DISTINCT TOP (1) PERCENT a AS x, b AS x FROM t ORDER BY x",
            "Ambiguous column name 'x'.",
        ),
        (
            "SELECT TOP (1) WITH TIES nextval('calls') AS n FROM t ORDER BY n",
            "volatile",
        ),
        (
            "SELECT TOP (1) WITH TIES id FROM t ORDER BY nextval('calls')",
            "volatile",
        ),
        (
            "SELECT TOP (10) PERCENT id FROM t ORDER BY NEWID()",
            "volatile",
        ),
        (
            "SELECT TOP (2) WITH TIES uuidv4() AS k, id FROM t ORDER BY k",
            "volatile",
        ),
        (
            "SELECT TOP (2) WITH TIES id FROM t ORDER BY uuidv7()",
            "volatile",
        ),
        (
            "SELECT TOP (1) WITH TIES nextval('s') AS n, currval('s') AS c FROM t ORDER BY c",
            "volatile",
        ),
        (
            "SELECT TOP (1) WITH TIES ROW_NUMBER() OVER (ORDER BY id) AS rn FROM t ORDER BY rn",
            "window function",
        ),
        (
            "SELECT TOP (1) WITH TIES id FROM t ORDER BY ROW_NUMBER() OVER (ORDER BY id)",
            "window function",
        ),
        ("SELECT TOP (1) PERCENT * FROM t ORDER BY 1", "wildcard"),
        // Positions count expanded columns, which the AST does not have.
        ("SELECT TOP (1) WITH TIES * FROM t ORDER BY 2", "wildcard"),
        (
            "SELECT TOP (1) WITH TIES t.*, id FROM t ORDER BY 2",
            "wildcard",
        ),
        // A ranking copy of a volatile key would be evaluated separately.
        (
            "SELECT TOP (2) WITH TIES NEWID() AS k, id FROM t ORDER BY k",
            "volatile",
        ),
        (
            "SELECT TOP (50) PERCENT id, RAND() * 10 FROM t ORDER BY 2",
            "volatile",
        ),
        ("SELECT DISTINCT TOP (1) PERCENT * FROM t", "wildcard"),
        (
            "SELECT TOP (1) PERCENT id FROM t ORDER BY id OFFSET 0 ROWS",
            "OFFSET",
        ),
    ] {
        let error = lower(sql).unwrap_err();
        assert!(error.contains(expected), "{sql}: {error}");
    }
}

#[test]
fn plain_top_and_other_bodies_are_unchanged() {
    for sql in [
        "SELECT TOP (2) id FROM t ORDER BY id",
        "SELECT id FROM t",
        "SELECT 1 UNION ALL SELECT 2",
    ] {
        let mut expected = query(sql);
        let before = expected.to_string();
        top::ranked(&mut expected).unwrap();
        assert_eq!(expected.to_string(), before);
    }
    // Parsed without the batch front end to show the lowering is AST-only.
    let mut parsed = Parser::parse_sql(
        &msduck_sql::dialect::ServerDialect,
        "SELECT TOP (0) PERCENT id FROM t",
    )
    .unwrap()
    .remove(0);
    let Statement::Query(query) = &mut parsed else {
        unreachable!()
    };
    top::ranked(query).unwrap();
    let SetExpr::Select(select) = query.body.as_ref() else {
        unreachable!()
    };
    assert!(select.top.is_none() && select.qualify.is_some());
    assert!(
        query
            .to_string()
            .contains("row_number() OVER (ORDER BY NULL)")
    );
}

#[test]
fn out_of_range_positions_use_sql_server_error_108() {
    let message = lower("SELECT TOP (1) WITH TIES id, score FROM t ORDER BY 3").unwrap_err();
    let error = top::diagnostic(&message).unwrap();
    assert_eq!(
        (
            error.number,
            error.state,
            error.severity,
            error.message.as_str()
        ),
        (
            108,
            1,
            16,
            "The ORDER BY position number 3 is out of range of the number of items in the select list."
        )
    );
}

#[test]
fn wildcards_and_nonvolatile_aliases_still_rank_by_source_keys() {
    let sql = lower("SELECT TOP (1) WITH TIES * FROM t ORDER BY score DESC").unwrap();
    assert!(sql.contains("rank() OVER (ORDER BY score DESC)"), "{sql}");
    let sql = lower("SELECT TOP (2) WITH TIES GETDATE() AS d, id FROM t ORDER BY d").unwrap();
    assert!(sql.contains("rank() OVER (ORDER BY GETDATE())"), "{sql}");
}

#[test]
fn distinct_keys_match_qualified_and_unqualified_columns() {
    for sql in [
        "SELECT DISTINCT TOP (50) PERCENT t.score FROM t ORDER BY score",
        "SELECT DISTINCT TOP (50) PERCENT score FROM t ORDER BY t.score",
        "SELECT DISTINCT TOP (50) PERCENT t.score FROM t ORDER BY T.Score",
    ] {
        let sql = lower(sql).unwrap();
        assert!(
            sql.contains("QUALIFY row_number() OVER (ORDER BY score)"),
            "{sql}"
        );
    }
    // SQL Server matches a bare column by output name even with two sources,
    // but reports an unqualified reference inside an expression as ambiguous.
    assert!(
        lower("SELECT DISTINCT TOP (50) PERCENT a.score FROM a CROSS JOIN b ORDER BY score")
            .is_ok()
    );
    assert_eq!(
        lower("SELECT DISTINCT TOP (50) PERCENT a.score + 1 AS x FROM a CROSS JOIN b ORDER BY score + 1")
            .unwrap_err(),
        top::DISTINCT_ORDER
    );
    // A parenthesized join is still two sources.
    assert_eq!(
        lower("SELECT DISTINCT TOP (50) PERCENT a.score + 1 AS x FROM (a CROSS JOIN b) ORDER BY score + 1")
            .unwrap_err(),
        top::DISTINCT_ORDER
    );
    // Expressions match when their column references do.
    for sql in [
        "SELECT DISTINCT TOP (50) PERCENT t.score + 1 AS x FROM t ORDER BY score + 1",
        "SELECT DISTINCT TOP (50) PERCENT len(T.Name) AS n FROM t ORDER BY LEN(name)",
    ] {
        assert!(lower(sql).is_ok(), "{sql}");
    }
    assert_eq!(
        lower("SELECT DISTINCT TOP (50) PERCENT a.score + 1 AS x FROM a, b ORDER BY b.score + 1")
            .unwrap_err(),
        top::DISTINCT_ORDER
    );
    assert_eq!(
        lower("SELECT DISTINCT TOP (50) PERCENT score + 1 AS x FROM t ORDER BY score + 2")
            .unwrap_err(),
        top::DISTINCT_ORDER
    );
    // Different qualifiers name different columns.
    assert_eq!(
        lower("SELECT DISTINCT TOP (50) PERCENT a.score FROM a, b ORDER BY b.score").unwrap_err(),
        top::DISTINCT_ORDER
    );
}

#[test]
fn ambiguous_aliases_use_sql_server_error_209() {
    let message = lower("SELECT TOP (1) WITH TIES a AS x, b AS x FROM t ORDER BY x").unwrap_err();
    let error = top::diagnostic(&message).unwrap();
    assert_eq!(
        (
            error.number,
            error.state,
            error.severity,
            error.message.as_str()
        ),
        (209, 1, 16, "Ambiguous column name 'x'.")
    );
    // A repeated alias that the ORDER BY does not name is not ambiguous.
    assert!(lower("SELECT TOP (1) WITH TIES a AS x, b AS x, c FROM t ORDER BY c").is_ok());
}

#[test]
fn distinct_string_literal_aliases_are_ranked_as_columns() {
    let sql = lower("SELECT DISTINCT TOP (1) WITH TIES score AS 'x' FROM t ORDER BY x").unwrap();
    assert!(
        sql.contains("QUALIFY rank() OVER (ORDER BY \"x\")"),
        "{sql}"
    );
    assert!(sql.ends_with("ORDER BY \"x\""), "{sql}");
}

#[test]
fn column_references_in_top_quantities_use_sql_server_error_4115() {
    // Captured from SQL Server 2022: Msg 4115, Level 15, State 1.
    let message = lower("SELECT TOP (score) PERCENT id FROM t ORDER BY id").unwrap_err();
    let error = top::diagnostic(&message).unwrap();
    assert_eq!(
        (
            error.number,
            error.state,
            error.severity,
            error.message.as_str()
        ),
        (
            4115,
            1,
            15,
            "The reference to column \"score\" is not allowed in an argument to a TOP, OFFSET, or FETCH clause. Only references to columns at an outer scope or standalone expressions and subqueries are allowed here."
        )
    );
    assert!(lower("SELECT TOP (t.id + 1) WITH TIES id FROM t ORDER BY id").is_err());
    // Aggregates and window functions, captured from SQL Server 2022.
    for (sql, number, message) in [
        (
            "SELECT TOP (COUNT(*)) PERCENT id FROM t ORDER BY id",
            162,
            "Invalid expression in a TOP or OFFSET clause.",
        ),
        (
            "SELECT TOP (MAX(1)) WITH TIES id FROM t ORDER BY id",
            162,
            "Invalid expression in a TOP or OFFSET clause.",
        ),
        (
            "SELECT TOP (ROW_NUMBER() OVER (ORDER BY (SELECT 1))) PERCENT id FROM t ORDER BY id",
            4108,
            "Windowed functions can only appear in the SELECT or ORDER BY clauses.",
        ),
    ] {
        let error = top::diagnostic(&lower(sql).unwrap_err()).unwrap();
        assert_eq!(
            (
                error.number,
                error.state,
                error.severity,
                error.message.as_str()
            ),
            (number, 1, 15, message),
            "{sql}"
        );
    }
    // Variables and self-contained subqueries are allowed.
    assert!(lower("SELECT TOP (@n) PERCENT id FROM t ORDER BY id").is_ok());
    assert!(
        lower(
            "SELECT TOP ((SELECT count(*) FROM u WHERE u.k = 1)) WITH TIES id FROM t ORDER BY id"
        )
        .is_ok()
    );
}
