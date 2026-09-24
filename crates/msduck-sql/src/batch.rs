//! Database-independent batch parsing and syntax normalization.
use anyhow::{Result, bail, ensure};
use msduck_core::types::Type as SqlType;
use sqlparser::{ast::*, parser::Parser};

/// SQL Server allows adjacent statements without semicolons. Let the parser
/// determine each statement boundary rather than splitting on lines or strings.
pub fn parse(sql: &str) -> Result<Vec<Statement>> {
    use sqlparser::tokenizer::Token;
    let dialect = crate::dialect::ServerDialect;
    let (tokens, fetch_expressions) =
        crate::top::fetch_tokens(crate::group_all::tokens(crate::dialect::tokenize(sql)?))?;
    let mut parser = Parser::new(&dialect).with_tokens_with_locations(tokens);
    let mut statements = Vec::new();
    loop {
        while parser.consume_token(&Token::SemiColon) {}
        if parser.peek_token().token == Token::EOF {
            return Ok(statements);
        }
        ensure!(statements.len() < 10000, "too many statements in batch");
        let mut statement = parser
            .parse_statement()
            .map_err(crate::drop_index_syntax::parse_error)?;
        explicit_defaults(&mut statement);
        crate::variant_cast::mark(&mut statement);
        crate::window_frame::validate_syntax(&statement).map_err(anyhow::Error::msg)?;
        crate::top::restore_fetch(&mut statement, &fetch_expressions);
        crate::dialect::canonicalize_select_assignments(&mut statement)
            .map_err(anyhow::Error::msg)?;
        canonicalize_insert(&mut statement)?;
        // Check supported target shapes now, retaining ON scopes for binding.
        let mut checked = statement.clone();
        if crate::output::joined_update(&checked).is_none() {
            crate::update::canonicalize(&mut checked)?;
        }
        crate::delete::canonicalize(&mut checked)?;
        statements.push(statement);
    }
}

pub fn parameter_declarations(source: &str) -> Result<Vec<(String, SqlType)>> {
    if source.trim().is_empty() {
        return Ok(Vec::new());
    }
    let statements = parse(&format!("DECLARE {source}"))?;
    ensure!(statements.len() == 1, "invalid RPC parameter declarations");
    let Statement::Declare { stmts } = &statements[0] else {
        bail!("invalid parameter declarations");
    };
    let mut result = Vec::new();
    for declaration in stmts {
        ensure!(
            declaration.assignment.is_none() && declaration.declare_type.is_none(),
            "unsupported parameter declaration"
        );
        let kind = declaration
            .data_type
            .clone()
            .ok_or_else(|| anyhow::anyhow!("missing parameter type"))?;
        let kind = variable_type(kind)?;
        for name in &declaration.names {
            result.push((name.value.to_lowercase(), kind));
        }
    }
    Ok(result)
}

pub fn variable_type(kind: DataType) -> Result<SqlType> {
    let kind = crate::sql_type::declaration(&kind)?;
    ensure!(
        kind != SqlType::Variant,
        "SQL_VARIANT variables are not yet supported"
    );
    Ok(kind)
}

fn explicit_defaults(statement: &mut Statement) {
    struct Defaults;
    impl VisitorMut for Defaults {
        type Break = ();
        fn pre_visit_expr(&mut self, e: &mut Expr) -> std::ops::ControlFlow<()> {
            match e {
                Expr::Cast {
                    data_type:
                        kind @ (DataType::Varchar(None)
                        | DataType::Char(None)
                        | DataType::Character(None)),
                    ..
                }
                | Expr::Convert {
                    data_type:
                        Some(
                            kind @ (DataType::Varchar(None)
                            | DataType::Char(None)
                            | DataType::Character(None)),
                        ),
                    ..
                } => {
                    let length = Some(CharacterLength::IntegerLength {
                        length: 30,
                        unit: None,
                    });
                    *kind = if matches!(kind, DataType::Varchar(_)) {
                        DataType::Varchar(length)
                    } else {
                        DataType::Char(length)
                    }
                }
                _ => {}
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    let _ = VisitMut::visit(statement, &mut Defaults);
}
fn canonicalize_insert(statement: &mut Statement) -> Result<()> {
    struct Normalize;
    impl VisitorMut for Normalize {
        type Break = String;
        fn pre_visit_statement(
            &mut self,
            statement: &mut Statement,
        ) -> std::ops::ControlFlow<String> {
            use std::ops::ControlFlow;
            let recursion_limit = match statement {
                Statement::Query(query) if matches!(query.body.as_ref(), SetExpr::Insert(_)) => {
                    crate::query_options::take(query)
                }
                _ => None,
            };
            if let Statement::Query(query) = statement
                && let SetExpr::Insert(Statement::Insert(insert)) = query.body.as_mut()
            {
                if query.order_by.is_some()
                    || query.limit_clause.is_some()
                    || query.fetch.is_some()
                    || !query.locks.is_empty()
                    || query.for_clause.is_some()
                    || query.settings.is_some()
                    || query.format_clause.is_some()
                    || !query.pipe_operators.is_empty()
                {
                    return ControlFlow::Break("unsupported outer INSERT query options".into());
                }
                if let Some(with) = &query.with {
                    let Some(source) = &mut insert.source else {
                        return ControlFlow::Break("unsupported CTE with DEFAULT VALUES".into());
                    };
                    if source.with.is_some() {
                        return ControlFlow::Break("unsupported nested INSERT WITH clauses".into());
                    }
                    source.with = Some(with.clone());
                }
                if let Some(limit) = recursion_limit {
                    let Some(source) = &mut insert.source else {
                        return ControlFlow::Break(
                            "unsupported recursion hint with DEFAULT VALUES".into(),
                        );
                    };
                    crate::query_options::set(source, limit);
                }
                *statement = Statement::Insert(insert.clone());
            }
            ControlFlow::Continue(())
        }
    }
    if let std::ops::ControlFlow::Break(error) = statement.visit(&mut Normalize) {
        anyhow::bail!(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn semicolon_free_batches_do_not_split_literals_or_comments() {
        let statements=parse("SET NOCOUNT ON\nSET ANSI_NULLS ON\nSELECT N'a; SET NOCOUNT OFF' AS text; -- SELECT 2\nSELECT 3").unwrap();
        assert_eq!(statements.len(), 4);
        assert!(matches!(&statements[2], Statement::Query(_)));
    }
}
