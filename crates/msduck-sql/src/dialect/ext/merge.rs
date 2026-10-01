//! Syntax for: MERGE statements, including table hints.
//!
//! `crate::merge::parse` owns the grammar: `MERGE [TOP (n) [PERCENT]] [INTO]
//! target [WITH (hints)] [[AS] alias] USING source ON condition WHEN ...;`.
//! A CTE-prefixed MERGE (`WITH s AS (...) MERGE ...`) is parsed by sqlparser
//! as a query body; the runtime accepts both shapes.
use sqlparser::{
    ast::{SetExpr, Statement, With},
    keywords::Keyword,
    parser::{Parser, ParserError},
};

/// Parse a statement this feature owns, or decline without consuming tokens.
pub fn parse(parser: &mut Parser) -> Option<Result<Statement, ParserError>> {
    if parser.peek_keyword(Keyword::MERGE) {
        return Some(crate::merge::parse(parser));
    }
    // `WITH cte AS (...) MERGE ...`: sqlparser would parse the MERGE body
    // itself, without TOP or target hints.
    if parser.peek_keyword(Keyword::WITH) {
        let with = parser
            .try_parse(|parser| {
                parser.expect_keyword(Keyword::WITH)?;
                let with = With {
                    with_token: parser.get_current_token().clone().into(),
                    recursive: false,
                    cte_tables: parser.parse_comma_separated(Parser::parse_cte)?,
                };
                if parser.peek_keyword(Keyword::MERGE) {
                    Ok(with)
                } else {
                    Err(ParserError::ParserError("not a MERGE".into()))
                }
            })
            .ok()?;
        return Some((|| {
            let merge = crate::merge::parse(parser)?;
            let mut template = Parser::parse_sql(
                &crate::dialect::ServerDialect,
                "WITH __msduck_cte AS (SELECT 1 AS x) SELECT 1",
            )?;
            let Statement::Query(mut query) = template.remove(0) else {
                unreachable!("query template")
            };
            query.with = Some(with);
            query.body = Box::new(SetExpr::Merge(merge));
            Ok(Statement::Query(query))
        })());
    }
    None
}

/// Whether this feature validates `statement` itself, so the generic batch
/// checks (target-shape canonicalization and variable preflight) skip it.
pub fn owns(statement: &Statement) -> bool {
    match statement {
        Statement::Merge(_) => true,
        Statement::Query(query) => matches!(query.body.as_ref(), SetExpr::Merge(_)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::ast::{Expr, TableFactor};

    fn merge(sql: &str) -> sqlparser::ast::Merge {
        match crate::batch::parse(sql).unwrap().remove(0) {
            Statement::Merge(merge) => merge,
            other => panic!("{other}"),
        }
    }

    #[test]
    fn hints_before_the_alias_and_top_are_retained() {
        let parsed = merge(
            "MERGE items WITH(SERIALIZABLE) AS target USING(VALUES(1)) AS source(id) ON target.id=source.id WHEN NOT MATCHED THEN INSERT(id) VALUES(source.id);",
        );
        let TableFactor::Table {
            name,
            alias,
            with_hints,
            ..
        } = &parsed.table
        else {
            panic!()
        };
        assert_eq!(name.to_string(), "items");
        assert_eq!(alias.as_ref().unwrap().name.value, "target");
        assert!(matches!(&with_hints[..], [Expr::Identifier(id)] if id.value == "SERIALIZABLE"));
        assert!(crate::merge::top_clause(&parsed).unwrap().is_none());

        for sql in [
            "MERGE TOP (2) INTO dbo.items WITH (HOLDLOCK, UPDLOCK) t USING src s ON t.id=s.id WHEN MATCHED THEN DELETE;",
            "MERGE TOP (50) PERCENT dbo.items WITH (ROWLOCK) AS t USING src AS s ON t.id=s.id WHEN MATCHED THEN DELETE;",
            "MERGE dbo.items WITH (HOLDLOCK) USING src AS s ON items.id=s.id WHEN MATCHED THEN DELETE;",
        ] {
            let parsed = merge(sql);
            let TableFactor::Table { with_hints, .. } = &parsed.table else {
                panic!()
            };
            assert!(!with_hints.is_empty(), "{sql}");
        }
        let parsed =
            merge("MERGE TOP (@n) PERCENT t USING s ON t.id=s.id WHEN MATCHED THEN DELETE;");
        let top = crate::merge::top_clause(&parsed).unwrap().unwrap();
        assert!(top.percent);
        assert_eq!(top.to_string(), "TOP (@n) PERCENT");
    }

    #[test]
    fn terminator_and_target_hint_errors_keep_their_numbers() {
        for (sql, number) in [
            (
                "MERGE t WITH (NOLOCK) AS a USING s ON a.id=s.id WHEN MATCHED THEN DELETE;",
                1065,
            ),
            (
                "MERGE TOP (1) t USING s ON t.id=s.id WHEN MATCHED THEN DELETE",
                10713,
            ),
            (
                "MERGE t USING s ON t.id=s.id WHEN MATCHED THEN DELETE SELECT 1;",
                10713,
            ),
            (
                "MERGE t USING s ON t.id=s.id WHEN MATCHED AND s.n>0 THEN UPDATE SET n=1 WHEN MATCHED THEN UPDATE SET n=2;",
                10714,
            ),
            // SQL Server accepts MERGE target hints only before the alias.
            (
                "MERGE INTO dbo.items AS t WITH (HOLDLOCK) USING src AS s ON t.id=s.id WHEN MATCHED THEN DELETE;",
                156,
            ),
        ] {
            let error = crate::batch::parse(sql).unwrap_err().to_string();
            assert_eq!(
                crate::merge::error_number(&error),
                Some(number),
                "{sql}: {error}"
            );
        }
        let error = crate::batch::parse(
            "MERGE t USING s ON t.id=s.id WHEN MATCHED AND s.n>0 THEN UPDATE SET n=1 WHEN MATCHED THEN UPDATE SET n=2;",
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.ends_with("An action of type 'WHEN MATCHED' cannot appear more than once in a 'UPDATE' clause of a MERGE statement."),
            "{error}"
        );
    }

    #[test]
    fn cte_prefixed_merge_keeps_top_and_hints() {
        let statement = crate::batch::parse(
            "WITH c(id) AS (SELECT 1) MERGE TOP (1) t WITH (HOLDLOCK) AS x USING c ON x.id = c.id WHEN MATCHED THEN DELETE;",
        )
        .unwrap()
        .remove(0);
        assert!(owns(&statement));
        let Statement::Query(query) = &statement else {
            panic!("{statement}")
        };
        assert_eq!(query.with.as_ref().unwrap().cte_tables.len(), 1);
        let SetExpr::Merge(Statement::Merge(merge)) = query.body.as_ref() else {
            panic!("{statement}")
        };
        assert!(crate::merge::top_clause(merge).unwrap().is_some());
        assert!(
            matches!(&merge.table, TableFactor::Table { with_hints, .. } if with_hints.len() == 1)
        );
        // Other WITH statements are left to the built-in parser.
        let statement = crate::batch::parse("WITH c AS (SELECT 1 AS id) SELECT id FROM c")
            .unwrap()
            .remove(0);
        assert!(!owns(&statement));
    }

    #[test]
    fn following_statements_still_parse() {
        let statements = crate::batch::parse(
            "MERGE t WITH (HOLDLOCK) AS a USING (SELECT 1 AS id) s ON a.id=s.id WHEN NOT MATCHED THEN INSERT (id) VALUES (s.id) OUTPUT $action, inserted.id; SELECT 1",
        )
        .unwrap();
        assert_eq!(statements.len(), 2);
        assert!(owns(&statements[0]));
        assert!(!owns(&statements[1]));
    }
}
