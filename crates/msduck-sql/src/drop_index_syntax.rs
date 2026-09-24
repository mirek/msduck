//! Preserve a DROP request in a visitor-compatible node until root execution.
use crate::drop_index::{self, Error, OptionClause, Request, Target};
use sqlparser::{
    ast::*,
    keywords::Keyword,
    parser::{Parser, ParserError},
};
const MARKER: &str = "__msduck_drop_index";

pub fn starts(parser: &Parser<'_>) -> bool {
    parser.peek_keyword(Keyword::DROP)
        && matches!(parser.peek_nth_token(1).token, sqlparser::tokenizer::Token::Word(w) if w.keyword == Keyword::INDEX)
}

pub fn parse(parser: &mut Parser<'_>) -> Result<Statement, ParserError> {
    let request = drop_index::parse_cursor(parser).map_err(|error| {
        ParserError::ParserError(match error {
            Error::Sql(d) => d.message,
            Error::Unsupported(message) => message,
        })
    })?;
    let mut args = vec![Expr::Value(Value::Boolean(request.if_exists).into())];
    for target in request.targets {
        let parts = target
            .table
            .0
            .into_iter()
            .map(|p| match p {
                ObjectNamePart::Identifier(id) => id,
                _ => unreachable!("DROP parser accepts only identifiers"),
            })
            .collect();
        args.push(Expr::Tuple(vec![
            Expr::CompoundIdentifier(parts),
            Expr::Identifier(target.index),
            crate::expr::number(match target.option {
                OptionClause::None => 0,
                OptionClause::OnlineOff => 1,
                OptionClause::MaxdopOne => 2,
            }),
        ]));
    }
    let Expr::Function(mut function) = crate::expr::unary_function(MARKER, crate::expr::number(0))
    else {
        unreachable!()
    };
    if let FunctionArguments::List(list) = &mut function.args {
        list.args = args
            .into_iter()
            .map(|e| FunctionArg::Unnamed(FunctionArgExpr::Expr(e)))
            .collect();
    }
    Ok(Statement::Raise(RaiseStatement {
        value: Some(RaiseStatementValue::Expr(Expr::Function(function))),
    }))
}

pub fn request(statement: &Statement) -> Option<Request> {
    let Statement::Raise(RaiseStatement {
        value: Some(RaiseStatementValue::Expr(Expr::Function(f))),
    }) = statement
    else {
        return None;
    };
    if f.name.to_string() != MARKER {
        return None;
    }
    let FunctionArguments::List(args) = &f.args else {
        return None;
    };
    let expressions = args
        .args
        .iter()
        .map(|a| match a {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Some(e),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let (Expr::Value(value), targets) = expressions.split_first()? else {
        return None;
    };
    let Value::Boolean(if_exists) = value.value else {
        return None;
    };
    let targets = targets
        .iter()
        .map(|e| {
            let Expr::Tuple(parts) = e else { return None };
            let [
                Expr::CompoundIdentifier(table),
                Expr::Identifier(index),
                Expr::Value(option),
            ] = parts.as_slice()
            else {
                return None;
            };
            let Value::Number(option, false) = &option.value else {
                return None;
            };
            let option = match option.as_str() {
                "0" => OptionClause::None,
                "1" => OptionClause::OnlineOff,
                "2" => OptionClause::MaxdopOne,
                _ => return None,
            };
            if !(1..=2).contains(&table.len()) {
                return None;
            }
            Some(Target {
                table: ObjectName::from(table.clone()),
                index: index.clone(),
                option,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    if targets.is_empty() {
        return None;
    }
    Some(Request { if_exists, targets })
}

/// Preserve captured syntax errors through sqlparser's string-only error type.
/// Matching only the two exact messages avoids reclassifying unrelated errors.
pub fn parse_error(error: ParserError) -> anyhow::Error {
    if let ParserError::ParserError(message) = &error {
        let number = match message.as_str() {
            "Must specify the table name and index name for the DROP INDEX statement." => Some(159),
            "Incorrect syntax near the keyword 'ON'." => Some(156),
            _ => None,
        };
        if let Some(number) = number {
            return msduck_core::diagnostic::SqlError::syntax(number, 1, message).into();
        }
    }
    error.into()
}
