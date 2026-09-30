//! SQL Server computed columns in CREATE TABLE: `name AS expr [PERSISTED]`.
//!
//! sqlparser requires a data type after a column name. Before parsing,
//! [`mark`] inserts a placeholder type between the name and `AS` inside a
//! CREATE TABLE column list; the dialect then parses `AS expr [PERSISTED]` as
//! a generated-column option with [`parse`]. The root adapter replaces the
//! placeholder with the type inferred from the expression.
use sqlparser::{
    ast::*,
    keywords::Keyword,
    parser::{Parser, ParserError},
    tokenizer::{Token, TokenWithSpan, Word},
};

use super::key_index_type::{is_word, significant};

/// The placeholder data type; the T-SQL tokenizer cannot produce it as an
/// unquoted word followed by AS in a column list.
pub const PLACEHOLDER: &str = "__msduck_computed";

/// Insert the placeholder type after `name` in `CREATE TABLE t (..., name AS
/// ...)`, at the column list's top level.
pub fn mark(tokens: &mut Vec<TokenWithSpan>) {
    let significant = significant(tokens);
    let token = |position: usize| significant.get(position).map(|&index| &tokens[index].token);
    let mut insert = Vec::new();
    let mut position = 0;
    while position + 1 < significant.len() {
        let create_table = token(position).is_some_and(|t| is_word(t, "CREATE"))
            && token(position + 1).is_some_and(|t| is_word(t, "TABLE"));
        position += 1;
        if !create_table {
            continue;
        }
        // The column list is the first parenthesis after the table name.
        let mut at = position + 1;
        while token(at).is_some_and(|t| *t != Token::LParen && *t != Token::SemiColon) {
            at += 1;
        }
        if token(at) != Some(&Token::LParen) {
            continue;
        }
        let mut depth = 0usize;
        while let Some(current) = token(at) {
            match current {
                Token::LParen => depth += 1,
                Token::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                Token::Word(_)
                    if depth == 1
                        && matches!(token(at - 1), Some(Token::LParen | Token::Comma))
                        && matches!(token(at + 1), Some(Token::Word(w)) if w.quote_style.is_none() && w.keyword == Keyword::AS) =>
                {
                    insert.push(significant[at] + 1);
                }
                _ => {}
            }
            at += 1;
        }
        position = at;
    }
    for index in insert.into_iter().rev() {
        let span = tokens[index - 1].span;
        let placeholder = Token::Word(Word {
            value: PLACEHOLDER.into(),
            quote_style: None,
            keyword: Keyword::NoKeyword,
        });
        tokens.insert(index, TokenWithSpan::new(placeholder, span));
    }
}

/// Whether a column carries the placeholder type.
pub fn is_placeholder(data_type: &DataType) -> bool {
    matches!(data_type, DataType::Custom(name, args)
        if args.is_empty() && matches!(name.0.as_slice(), [ObjectNamePart::Identifier(id)] if id.value == PLACEHOLDER))
}

/// Parse `AS expr [PERSISTED]` after the placeholder type.
pub fn parse(parser: &mut Parser) -> Option<Result<Option<ColumnOption>, ParserError>> {
    let after_placeholder = matches!(&parser.get_current_token().token, Token::Word(w) if w.quote_style.is_none() && w.value == PLACEHOLDER);
    if !after_placeholder || !parser.peek_keyword(Keyword::AS) {
        return None;
    }
    Some((|| {
        parser.expect_keyword(Keyword::AS)?;
        let expr = parser.parse_expr()?;
        let persisted = matches!(&parser.peek_token().token, Token::Word(w) if w.quote_style.is_none() && w.value.eq_ignore_ascii_case("PERSISTED"));
        if persisted {
            parser.next_token();
        }
        Ok(Some(ColumnOption::Generated {
            generated_as: if persisted {
                GeneratedAs::ExpStored
            } else {
                GeneratedAs::Always
            },
            sequence_options: None,
            generation_expr: Some(expr),
            generation_expr_mode: Some(if persisted {
                GeneratedExpressionMode::Stored
            } else {
                GeneratedExpressionMode::Virtual
            }),
            generated_keyword: false,
        }))
    })())
}

/// A computed column's expression and whether it is PERSISTED. The option
/// keeps `generated_keyword: false` after the adapter replaces the
/// placeholder type, until [`lower`].
pub fn computed(column: &ColumnDef) -> Option<(&Expr, bool)> {
    column
        .options
        .iter()
        .find_map(|option| match &option.option {
            ColumnOption::Generated {
                generation_expr: Some(expr),
                generation_expr_mode,
                generated_keyword: false,
                ..
            } => Some((
                expr,
                *generation_expr_mode == Some(GeneratedExpressionMode::Stored),
            )),
            _ => None,
        })
}

/// Lower a computed column for DuckDB: a VIRTUAL generated column (DuckDB
/// cannot store one) without NULL constraints, which DuckDB rejects on
/// generated columns. An uninferred placeholder type is left to DuckDB.
pub fn lower(column: &mut ColumnDef) {
    if computed(column).is_none() {
        return;
    }
    if is_placeholder(&column.data_type) {
        column.data_type = DataType::Unspecified;
    }
    column
        .options
        .retain(|option| !matches!(option.option, ColumnOption::NotNull | ColumnOption::Null));
    for option in &mut column.options {
        if let ColumnOption::Generated {
            generated_as,
            generation_expr_mode,
            generated_keyword,
            ..
        } = &mut option.option
        {
            *generated_as = GeneratedAs::Always;
            *generation_expr_mode = Some(GeneratedExpressionMode::Virtual);
            *generated_keyword = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create(sql: &str) -> CreateTable {
        let Statement::CreateTable(table) = crate::batch::parse(sql).unwrap().remove(0) else {
            panic!("not CREATE TABLE")
        };
        table
    }

    #[test]
    fn computed_columns_parse_with_placeholder_types() {
        let table = create(
            "create table ComputedProbe (\n  id int not null identity(1, 1) primary key,\n  name varchar(100) null,\n  nameUpper as upper(name) persisted\n);",
        );
        let (expr, persisted) = computed(&table.columns[2]).unwrap();
        assert_eq!(
            (expr.to_string().as_str(), persisted),
            ("upper(name)", true)
        );
        assert!(is_placeholder(&table.columns[2].data_type));
        assert!(computed(&table.columns[1]).is_none());

        let table = create(
            "CREATE TABLE ComputedVirtual (a int NOT NULL, b int NULL, total AS a + b, doubled AS (a * 2), [label] AS CAST(a AS varchar(10)) + 'x', c AS a + 1 PERSISTED NOT NULL)",
        );
        let found: Vec<_> = table
            .columns
            .iter()
            .map(|c| computed(c).map(|(e, p)| (e.to_string(), p)))
            .collect();
        assert_eq!(
            found,
            vec![
                None,
                None,
                Some(("a + b".into(), false)),
                Some(("(a * 2)".into(), false)),
                Some(("CAST(a AS VARCHAR(10)) + 'x'".into(), false)),
                Some(("a + 1".into(), true)),
            ]
        );
        assert!(matches!(
            table.columns[5].options.last().unwrap().option,
            ColumnOption::NotNull
        ));

        let mut column = table.columns[5].clone();
        column.data_type = DataType::Int(None);
        lower(&mut column);
        assert_eq!(
            column.to_string(),
            "c INT GENERATED ALWAYS AS (a + 1) VIRTUAL"
        );
        let mut column = table.columns[3].clone();
        lower(&mut column);
        assert_eq!(
            column.to_string(),
            "doubled GENERATED ALWAYS AS ((a * 2)) VIRTUAL"
        );
    }

    #[test]
    fn other_statements_are_unchanged() {
        for sql in [
            "SELECT a AS b FROM t",
            "CREATE TABLE t AS SELECT 1 AS a",
            "CREATE TABLE t (a int CHECK (a > 0), b int DEFAULT (1))",
            "CREATE VIEW v AS SELECT 1 AS a",
        ] {
            let statements = crate::batch::parse(sql).unwrap();
            assert!(!statements[0].to_string().contains(PLACEHOLDER), "{sql}");
        }
    }
}
