use msduck_sql::{ddl_syntax, dialect::ServerDialect, expression_metadata::storage};
use sqlparser::{ast::*, parser::Parser};

#[test]
fn extrema_keep_exact_character_family_and_width() {
    for declaration in [
        "CHAR(8)",
        "VARCHAR(MAX)",
        "NVARCHAR(8)",
        "NVARCHAR(MAX)",
        "NCHAR(8)",
    ] {
        for aggregate in ["MIN", "MAX"] {
            let expr = Parser::new(&ServerDialect)
                .try_with_sql(&format!(
                    "{aggregate}(CAST('x' AS {declaration}) COLLATE Latin1_General_100_BIN2)"
                ))
                .unwrap()
                .parse_expr()
                .unwrap();
            let cast = Parser::new(&ServerDialect)
                .try_with_sql(&format!("CAST('x' AS {declaration})"))
                .unwrap()
                .parse_expr()
                .unwrap();
            assert_eq!(
                storage::kind(&expr, &Default::default(), &|_| None),
                storage::kind(&cast, &Default::default(), &|_| None)
            );
        }
    }
    for invalid in [
        "MAX(missing)",
        "MIN()",
        "MAX(DISTINCT CAST('x' AS NVARCHAR(8))) OVER()",
        "MAX(MIN(CAST('x' AS NVARCHAR(8))))",
    ] {
        let expr = Parser::new(&ServerDialect)
            .try_with_sql(invalid)
            .unwrap()
            .parse_expr()
            .unwrap();
        assert!(
            storage::kind(&expr, &Default::default(), &|_| None).is_none(),
            "{invalid}"
        );
    }
}

#[test]
fn add_column_accepts_collation_but_keeps_constraint_checks() {
    for (sql, valid) in [
        (
            "ALTER TABLE t ADD s NVARCHAR(8) COLLATE Latin1_General_100_BIN2",
            true,
        ),
        (
            "ALTER TABLE t ADD s NVARCHAR(8) COLLATE Latin1_General_100_BIN2 NOT NULL DEFAULT N'x'",
            true,
        ),
        (
            "ALTER TABLE t ADD s NVARCHAR(8) COLLATE Latin1_General_100_BIN2 COLLATE Latin1_General_100_CI_AS",
            false,
        ),
        (
            "ALTER TABLE t ADD s NVARCHAR(8) COLLATE Latin1_General_100_BIN2 UNIQUE",
            false,
        ),
    ] {
        let Statement::AlterTable(table) =
            Parser::parse_sql(&ServerDialect, sql).unwrap().remove(0)
        else {
            panic!("alter")
        };
        assert_eq!(ddl_syntax::alter_table(&table).is_ok(), valid, "{sql}");
    }
}
