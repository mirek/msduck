//! Statement syntax for SQL Server features that live in their own modules.
//!
//! Each module under `ext/` exposes `parse`, which claims a statement (or
//! declines without consuming tokens), and `owns`, which exempts statements it
//! validates itself from the generic batch checks. Statements that have no
//! sqlparser equivalent travel through the batch as a [`carrier`] and are
//! decoded at execution with [`custom`]. See `docs/extension-hooks.md`.
use sqlparser::{
    ast::*,
    parser::{Parser, ParserError},
    tokenizer::Token,
};

pub mod applock;
pub mod backup;
pub mod bulk;
pub mod catalog;
pub mod computed;
pub mod constraints;
pub mod conversion;
pub mod functions;
pub mod identifiers;
pub mod json_string;
pub mod keys;
pub mod merge;
pub mod outer_dml;
pub mod procedures;
pub mod rowversion_identity;
pub mod temp_tables;
pub mod transactions;
pub mod triggers;

type Parse = fn(&mut Parser) -> Option<Result<Statement, ParserError>>;
type Owns = fn(&Statement) -> bool;

/// Every feature, in dispatch order.
const FEATURES: &[(Parse, Owns)] = &[
    (identifiers::parse, identifiers::owns),
    (transactions::parse, transactions::owns),
    (applock::parse, applock::owns),
    (backup::parse, backup::owns),
    (procedures::parse, procedures::owns),
    (functions::parse, functions::owns),
    (triggers::parse, triggers::owns),
    (temp_tables::parse, temp_tables::owns),
    (keys::parse, keys::owns),
    (constraints::parse, constraints::owns),
    (rowversion_identity::parse, rowversion_identity::owns),
    (computed::parse, computed::owns),
    (merge::parse, merge::owns),
    (outer_dml::parse, outer_dml::owns),
    (catalog::parse, catalog::owns),
    (json_string::parse, json_string::owns),
    (conversion::parse, conversion::owns),
    (bulk::parse, bulk::owns),
];

/// The first feature parse that claims the next statement.
pub fn parse(parser: &mut Parser) -> Option<Result<Statement, ParserError>> {
    FEATURES.iter().find_map(|(parse, _)| parse(parser))
}

/// Whether a feature validates `statement` itself.
pub fn owns(statement: &Statement) -> bool {
    custom(statement).is_some() || FEATURES.iter().any(|(_, owns)| owns(statement))
}

const MARKER: &str = "__msduck_ext";

/// A statement without an sqlparser equivalent: a feature `kind`, an opaque
/// `payload` the feature encodes as it likes, and expressions that must stay
/// in the tree so variable checks and binding see them.
#[derive(Clone, Debug, PartialEq)]
pub struct Custom<'a> {
    pub kind: &'a str,
    pub payload: &'a str,
    pub args: Vec<&'a Expr>,
}

fn text(value: &str) -> Expr {
    Expr::Value(Value::SingleQuotedString(value.into()).into())
}

/// Wrap a custom statement so it survives batch parsing and visitors.
pub fn carrier(kind: &str, payload: &str, args: Vec<Expr>) -> Statement {
    let args = [text(kind), text(payload)]
        .into_iter()
        .chain(args)
        .map(|e| FunctionArg::Unnamed(FunctionArgExpr::Expr(e)))
        .collect();
    Statement::Raise(RaiseStatement {
        value: Some(RaiseStatementValue::Expr(Expr::Function(Function {
            name: ObjectName::from(vec![Ident::new(MARKER)]),
            uses_odbc_syntax: false,
            parameters: FunctionArguments::None,
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args,
                clauses: vec![],
            }),
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
        }))),
    })
}

/// Decode a [`carrier`].
pub fn custom(statement: &Statement) -> Option<Custom<'_>> {
    let Statement::Raise(RaiseStatement {
        value: Some(RaiseStatementValue::Expr(Expr::Function(function))),
    }) = statement
    else {
        return None;
    };
    if function.name.to_string() != MARKER {
        return None;
    }
    let FunctionArguments::List(list) = &function.args else {
        return None;
    };
    let mut args = list.args.iter().map(|arg| match arg {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Some(e),
        _ => None,
    });
    fn string(e: Option<Option<&Expr>>) -> Option<&str> {
        match e?? {
            Expr::Value(ValueWithSpan {
                value: Value::SingleQuotedString(s),
                ..
            }) => Some(s.as_str()),
            _ => None,
        }
    }
    let kind = string(args.next())?;
    let payload = string(args.next())?;
    Some(Custom {
        kind,
        payload,
        args: args.collect::<Option<_>>()?,
    })
}

/// The first `count` words of a batch, upper-cased, skipping comments and
/// whitespace; punctuation ends the list. Batch hooks use it to recognize
/// statements such as `CREATE OR ALTER PROCEDURE` before parsing.
pub fn leading_words(sql: &str, count: usize) -> Vec<String> {
    let Ok(tokens) = crate::dialect::tokenize(sql) else {
        return Vec::new();
    };
    tokens
        .into_iter()
        .filter(|t| !matches!(t.token, Token::Whitespace(_)))
        .map_while(|t| match t.token {
            Token::Word(word) if word.quote_style.is_none() => Some(word.value.to_uppercase()),
            _ => None,
        })
        .take(count)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carrier_round_trips_kind_payload_and_arguments() {
        let arg = Expr::Identifier(Ident::new("@x"));
        let statement = carrier("waitfor", "delay", vec![arg.clone()]);
        let decoded = custom(&statement).unwrap();
        assert_eq!(decoded.kind, "waitfor");
        assert_eq!(decoded.payload, "delay");
        assert_eq!(decoded.args, vec![&arg]);
        assert!(owns(&statement));
        assert!(custom(&Statement::Raise(RaiseStatement { value: None })).is_none());
    }

    #[test]
    fn leading_words_skip_comments_and_stop_at_punctuation() {
        assert_eq!(
            leading_words("-- c\n /* d */ create  or alter PROC dbo.p", 5),
            ["CREATE", "OR", "ALTER", "PROC", "DBO"]
        );
        assert_eq!(leading_words("SELECT 1", 3), ["SELECT"]);
        assert!(leading_words("", 2).is_empty());
    }
}
