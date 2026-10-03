//! APPLY lowering of correlated FULL OUTER JOIN bodies (issue #867,
//! docs/apply-full-join.md), checked on the syntax tree.
use msduck_sql::{apply, dialect::ServerDialect};
use sqlparser::ast::{SetExpr, Statement};
use sqlparser::parser::Parser;

/// Lowers the APPLY joins of the outermost SELECT, as the translator does.
fn lowered(sql: &str) -> String {
    let mut statement = Parser::parse_sql(&ServerDialect, sql).unwrap().remove(0);
    let Statement::Query(query) = &mut statement else {
        panic!("not a query")
    };
    let SetExpr::Select(select) = query.body.as_mut() else {
        panic!("not a SELECT")
    };
    for table in &mut select.from {
        apply::lower(table);
    }
    statement.to_string()
}

const BODY: &str = "SELECT COALESCE(l.[key], r.[key]) AS [key], l.[value] AS old_value, r.[value] AS new_value FROM OPENJSON(i.lhs) l FULL OUTER JOIN OPENJSON(i.rhs) r ON l.[key] = r.[key]";

#[test]
fn cross_and_outer_apply_bodies_lose_the_correlated_full_join() {
    for (apply, join) in [
        ("CROSS APPLY", "CROSS JOIN LATERAL"),
        ("OUTER APPLY", "LEFT OUTER JOIN LATERAL"),
    ] {
        let sql = lowered(&format!("SELECT i.id, x.* FROM items i {apply} ({BODY}) x"));
        assert!(sql.contains(join), "{sql}");
        assert!(!sql.contains("FULL"), "{sql}");
        // Side 1 keeps every left row; side 2 adds right rows no left row matches.
        assert!(sql.contains("LEFT OUTER JOIN OPENJSON(i.lhs) l ON __msduck_full_join.__msduck_full_join_side = 1"), "{sql}");
        assert!(sql.contains("LEFT OUTER JOIN LATERAL (SELECT * FROM OPENJSON(i.rhs) r WHERE (__msduck_full_join.__msduck_full_join_side = 1 AND (l.[key] = r.[key])) OR __msduck_full_join.__msduck_full_join_side = 2) AS r ON true"), "{sql}");
        assert!(sql.contains("WHERE (__msduck_full_join.__msduck_full_join_side = 1 OR NOT EXISTS (SELECT 1 FROM OPENJSON(i.lhs) l WHERE l.[key] = r.[key])"), "{sql}");
    }
}

#[test]
fn nested_apply_bodies_such_as_folded_functions_are_rewritten() {
    let sql = lowered(&format!(
        "SELECT x.* FROM items i CROSS APPLY (SELECT rows.* FROM (SELECT i.lhs AS lhs, i.rhs AS rhs) AS p CROSS APPLY ({}) AS rows) x",
        BODY.replace("i.lhs", "p.lhs").replace("i.rhs", "p.rhs")
    ));
    assert!(!sql.contains("FULL"), "{sql}");
    assert!(
        sql.contains("NOT EXISTS (SELECT 1 FROM OPENJSON(p.lhs) l"),
        "{sql}"
    );
}

#[test]
fn uncorrelated_full_joins_and_full_joins_outside_apply_are_unchanged() {
    for sql in [
        "SELECT i.id, x.* FROM items i CROSS APPLY (SELECT a.k, b.k AS k2 FROM (VALUES (1)) a(k) FULL JOIN (VALUES (2)) b(k) ON a.k = b.k WHERE i.id = 1) x",
        "SELECT * FROM OPENJSON(N'[1]') l FULL JOIN OPENJSON(N'[2]') r ON l.[value] = r.[value]",
        // Volatile operands would be evaluated more than once.
        "SELECT i.id, x.* FROM items i CROSS APPLY (SELECT l.[key] FROM OPENJSON(i.lhs) l FULL JOIN OPENJSON(i.rhs) r ON l.[key] = r.[key] AND NEWID() IS NOT NULL) x",
        // A later RIGHT join would NULL-extend rows the side filter drops.
        "SELECT i.id, x.* FROM items i CROSS APPLY (SELECT l.[key] FROM OPENJSON(i.lhs) l FULL JOIN OPENJSON(i.rhs) r ON l.[key] = r.[key] RIGHT JOIN t ON 1 = 1) x",
    ] {
        assert!(lowered(sql).contains("FULL JOIN"), "{sql}");
    }
}

#[test]
fn star_hides_the_side_column_and_existing_filters_are_kept() {
    let sql = lowered(
        "SELECT x.* FROM items i CROSS APPLY (SELECT * FROM OPENJSON(i.lhs) l FULL JOIN OPENJSON(i.rhs) r ON l.[key] = r.[key] WHERE l.[key] IS NULL) x",
    );
    assert!(
        sql.contains("SELECT * EXCLUDE (__msduck_full_join_side) FROM"),
        "{sql}"
    );
    assert!(
        sql.ends_with("WHERE l.[key] = r.[key])) AND (l.[key] IS NULL)) x"),
        "{sql}"
    );
}
