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
        let mut statement = parser.parse_statement().map_err(|error| {
            match crate::dialect::ext::procedures::diagnostic(&error) {
                Some(diagnostic) => diagnostic.into(),
                None => crate::drop_index_syntax::parse_error(error),
            }
        })?;
        explicit_defaults(&mut statement);
        crate::variant_cast::mark(&mut statement);
        crate::window_frame::validate_syntax(&statement).map_err(anyhow::Error::msg)?;
        crate::top::restore_fetch(&mut statement, &fetch_expressions);
        crate::dialect::canonicalize_select_assignments(&mut statement)
            .map_err(anyhow::Error::msg)?;
        canonicalize_insert(&mut statement)?;
        // Check supported target shapes now, retaining ON scopes for binding.
        // Extension features validate the statements they own.
        if !crate::dialect::ext::owns(&statement) {
            let mut checked = statement.clone();
            if crate::output::joined_update(&checked).is_none() {
                crate::update::canonicalize(&mut checked)?;
            }
            crate::delete::canonicalize(&mut checked)?;
        }
        statements.push(statement);
    }
}

/// The names and types of an sp_executesql parameter declaration list.
pub fn parameter_declarations(source: &str) -> Result<Vec<(String, SqlType)>> {
    Ok(declared_parameters(source)?
        .into_iter()
        .map(|parameter| (parameter.name, parameter.data_type))
        .collect())
}

/// One sp_executesql parameter declaration: `@name type [OUT | OUTPUT]`.
#[derive(Clone, Debug, PartialEq)]
pub struct DeclaredParameter {
    /// Lower-cased name, including `@`.
    pub name: String,
    pub data_type: SqlType,
    pub output: bool,
}

/// Parse an sp_executesql parameter declaration list such as
/// `@a int, @b nvarchar(10) OUTPUT`. `OUT`/`OUTPUT` follow a type; the list
/// is otherwise a DECLARE list without initializers.
pub fn declared_parameters(source: &str) -> Result<Vec<DeclaredParameter>> {
    use sqlparser::tokenizer::Token;
    if source.trim().is_empty() {
        return Ok(Vec::new());
    }
    // Remove each OUT/OUTPUT keyword that ends a declaration, recording which
    // declaration it belonged to, then parse the rest as DECLARE.
    let tokens = crate::dialect::tokenize(source)?;
    let mut outputs = vec![false];
    let mut depth = 0usize;
    let mut kept = String::new();
    let mut last = 0;
    for token in &tokens {
        match &token.token {
            Token::LParen => depth += 1,
            Token::RParen => depth = depth.saturating_sub(1),
            Token::Comma if depth == 0 => outputs.push(false),
            Token::Word(word)
                if depth == 0
                    && word.quote_style.is_none()
                    && (word.value.eq_ignore_ascii_case("OUTPUT")
                        || word.value.eq_ignore_ascii_case("OUT")) =>
            {
                let output = outputs.last_mut().expect("one declaration");
                ensure!(!*output, "invalid parameter declarations");
                *output = true;
                let start = crate::dialect::ext::procedures::offset(source, token.span.start);
                let end = crate::dialect::ext::procedures::offset(source, token.span.end);
                kept.push_str(&source[last..start]);
                last = end;
            }
            _ => {}
        }
    }
    kept.push_str(&source[last..]);
    let statements = parse(&format!("DECLARE {kept}"))?;
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
            result.push(DeclaredParameter {
                name: name.value.to_lowercase(),
                data_type: kind,
                output: false,
            });
        }
    }
    ensure!(
        result.len() == outputs.len(),
        "invalid parameter declarations"
    );
    for (parameter, output) in result.iter_mut().zip(outputs) {
        parameter.output = output;
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
    fn declarations_accept_output_parameters() {
        let declared =
            declared_parameters("@a int, @B nvarchar(10) OUTPUT, @c decimal(5, 2) OUT").unwrap();
        let shape: Vec<_> = declared
            .iter()
            .map(|p| (p.name.as_str(), p.output))
            .collect();
        assert_eq!(shape, [("@a", false), ("@b", true), ("@c", true)]);
        assert_eq!(
            parameter_declarations("@x int OUTPUT").unwrap(),
            [("@x".to_string(), SqlType::Int)]
        );
        for invalid in ["@x int OUTPUT OUTPUT", "@x OUTPUT", "@x int = 1"] {
            assert!(declared_parameters(invalid).is_err(), "{invalid}");
        }
    }
    #[test]
    fn semicolon_free_batches_do_not_split_literals_or_comments() {
        let statements=parse("SET NOCOUNT ON\nSET ANSI_NULLS ON\nSELECT N'a; SET NOCOUNT OFF' AS text; -- SELECT 2\nSELECT 3").unwrap();
        assert_eq!(statements.len(), 4);
        assert!(matches!(&statements[2], Statement::Query(_)));
    }
}
