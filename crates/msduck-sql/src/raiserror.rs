//! RAISERROR syntax carried in a visitor-compatible statement node.
use sqlparser::{
    ast::*,
    parser::{Parser, ParserError},
    tokenizer::Token,
};

const MARKER: &str = "__msduck_raiserror";
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub log: bool,
    pub nowait: bool,
    pub seterror: bool,
}
impl Options {
    fn bits(self) -> u8 {
        u8::from(self.log) | (u8::from(self.nowait) << 1) | (u8::from(self.seterror) << 2)
    }
}
pub struct Call<'a> {
    pub options: Options,
    pub message: &'a Expr,
    pub severity: &'a Expr,
    pub state: &'a Expr,
    pub substitutions: Vec<&'a Expr>,
}

pub fn starts(parser: &Parser) -> bool {
    matches!(parser.peek_token().token, Token::Word(word) if word.quote_style.is_none() && word.value.eq_ignore_ascii_case("RAISERROR"))
}
pub fn parse(parser: &mut Parser) -> Result<Statement, ParserError> {
    parser.next_token();
    parser.expect_token(&Token::LParen)?;
    let mut values = vec![parser.parse_expr()?];
    while parser.consume_token(&Token::Comma) {
        values.push(parser.parse_expr()?);
    }
    parser.expect_token(&Token::RParen)?;
    if values.len() < 3 {
        return Err(ParserError::ParserError(
            "RAISERROR requires a message, severity and state".into(),
        ));
    }
    let mut options = Options::default();
    if parser.parse_keyword(sqlparser::keywords::Keyword::WITH) {
        loop {
            let token = parser.next_token();
            let Token::Word(word) = token.token else {
                return Err(ParserError::ParserError("Expected RAISERROR option".into()));
            };
            if word.quote_style.is_some() {
                return Err(ParserError::ParserError("Expected RAISERROR option".into()));
            }
            match word.value.to_ascii_uppercase().as_str() {
                "LOG" => options.log = true,
                "NOWAIT" => options.nowait = true,
                "SETERROR" => options.seterror = true,
                _ => {
                    return Err(ParserError::ParserError(format!(
                        "Unknown RAISERROR option {}",
                        word.value
                    )));
                }
            }
            if !parser.consume_token(&Token::Comma) {
                break;
            }
        }
    }
    values.insert(0, crate::expr::number(options.bits()));
    let Expr::Function(mut function) = crate::expr::unary_function(MARKER, crate::expr::number(0))
    else {
        unreachable!()
    };
    if let FunctionArguments::List(args) = &mut function.args {
        args.args = values
            .into_iter()
            .map(|value| FunctionArg::Unnamed(FunctionArgExpr::Expr(value)))
            .collect();
    }
    Ok(Statement::Raise(RaiseStatement {
        value: Some(RaiseStatementValue::Expr(Expr::Function(function))),
    }))
}

pub fn call(statement: &Statement) -> Option<Call<'_>> {
    let Statement::Raise(RaiseStatement {
        value: Some(RaiseStatementValue::Expr(Expr::Function(function))),
    }) = statement
    else {
        return None;
    };
    if function.name.to_string() != MARKER {
        return None;
    }
    let FunctionArguments::List(args) = &function.args else {
        return None;
    };
    let values = args
        .args
        .iter()
        .map(|arg| match arg {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) => Some(value),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let [
        Expr::Value(flags),
        message,
        severity,
        state,
        substitutions @ ..,
    ] = values.as_slice()
    else {
        return None;
    };
    let Value::Number(flags, _) = &flags.value else {
        return None;
    };
    let flags = flags.parse::<u8>().ok().filter(|value| *value <= 7)?;
    Some(Call {
        options: Options {
            log: flags & 1 != 0,
            nowait: flags & 2 != 0,
            seterror: flags & 4 != 0,
        },
        message,
        severity,
        state,
        substitutions: substitutions.to_vec(),
    })
}

/// Bind the constant/variable argument grammar without backend evaluation.
/// This preserves integer widths and avoids turning RAISERROR into a query that
/// can invalidate the backend transaction.
pub fn argument(
    expression: &Expr,
    parameters: &std::collections::HashMap<String, crate::parameter::Parameter>,
) -> anyhow::Result<msduck_core::value::Value> {
    use msduck_core::{diagnostic::SqlError, value::Value as Bound};
    match expression {
        Expr::Identifier(name) if name.value.starts_with('@') => {
            let parameter = parameters.get(&name.value.to_lowercase()).ok_or_else(|| {
                SqlError::syntax(
                    137,
                    2,
                    format!("Must declare the scalar variable \"{}\".", name.value),
                )
            })?;
            anyhow::ensure!(
                matches!(
                    parameter.data_type,
                    msduck_core::types::Type::TinyInt
                        | msduck_core::types::Type::SmallInt
                        | msduck_core::types::Type::Int
                        | msduck_core::types::Type::BigInt
                        | msduck_core::types::Type::Character(_)
                        | msduck_core::types::Type::Binary(_)
                ),
                "unsupported RAISERROR argument type"
            );
            Ok(parameter.value.clone())
        }
        Expr::Value(value) => match &value.value {
            Value::Null => Ok(Bound::Null),
            Value::SingleQuotedString(text) | Value::NationalStringLiteral(text) => {
                Ok(Bound::Text(text.clone()))
            }
            Value::Number(number, _) => numeric_argument(number),
            Value::HexStringLiteral(hex) => {
                let hex = if hex.len() % 2 == 1 {
                    format!("0{hex}")
                } else {
                    hex.clone()
                };
                let bytes = hex
                    .as_bytes()
                    .chunks_exact(2)
                    .map(|pair| {
                        let pair = std::str::from_utf8(pair)?;
                        Ok(u8::from_str_radix(pair, 16)?)
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;
                Ok(Bound::Blob(bytes))
            }
            _ => anyhow::bail!("unsupported RAISERROR argument literal"),
        },
        Expr::UnaryOp {
            op: UnaryOperator::Minus | UnaryOperator::Plus,
            expr,
        } => {
            if let Expr::Value(value) = expr.as_ref()
                && let Value::Number(number, _) = &value.value
            {
                let signed = if matches!(
                    expression,
                    Expr::UnaryOp {
                        op: UnaryOperator::Minus,
                        ..
                    }
                ) {
                    format!("-{number}")
                } else {
                    number.clone()
                };
                return numeric_argument(&signed);
            }
            anyhow::bail!("unsupported RAISERROR argument form")
        }
        _ => anyhow::bail!("unsupported RAISERROR argument form"),
    }
}

fn numeric_argument(number: &str) -> anyhow::Result<msduck_core::value::Value> {
    use msduck_core::value::{Decimal, Value};
    if let Ok(value) = number.parse::<i32>() {
        return Ok(Value::Int(value));
    }
    if number.contains(['e', 'E']) {
        return Ok(Value::Double(number.parse()?));
    }
    let unsigned = number.strip_prefix('-').unwrap_or(number);
    let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    let precision = (whole.trim_start_matches('0').len() + fraction.len()).max(1);
    anyhow::ensure!(
        precision <= 38,
        "unsupported RAISERROR numeric literal precision"
    );
    Ok(Value::Decimal(Decimal::new(
        precision as u8,
        fraction.len() as u8,
        number.replace('.', "").parse()?,
    )?))
}

/// Validate the supported RAISERROR grammar and declared substitution types
/// before any batch statement runs, including inside unreachable branches.
pub fn validate(
    statement: &Statement,
    parameters: &std::collections::HashMap<String, crate::parameter::Parameter>,
) -> anyhow::Result<()> {
    use msduck_core::{diagnostic::SqlError, types::Type};
    struct Check<'a>(&'a std::collections::HashMap<String, crate::parameter::Parameter>);
    impl Visitor for Check<'_> {
        type Break = SqlError;
        fn pre_visit_statement(
            &mut self,
            statement: &Statement,
        ) -> std::ops::ControlFlow<SqlError> {
            let Some(call) = call(statement) else {
                return std::ops::ControlFlow::Continue(());
            };
            if call.substitutions.len() > 20 {
                return std::ops::ControlFlow::Break(SqlError::new(
                    2747,
                    1,
                    "Too many substitution parameters for RAISERROR. Cannot exceed 20 substitution parameters.",
                ));
            }
            for (index, expr) in [call.message, call.severity, call.state]
                .into_iter()
                .chain(call.substitutions)
                .enumerate()
            {
                let token = match expr {
                    Expr::Identifier(id) if id.value.starts_with('@') => None,
                    Expr::Value(value) => match &value.value {
                        Value::Null if index < 3 => {
                            return std::ops::ControlFlow::Break(SqlError::syntax(
                                156,
                                1,
                                "Incorrect syntax near the keyword 'NULL'.",
                            ));
                        }
                        Value::Null | Value::Number(_, _) => None,
                        Value::SingleQuotedString(text) | Value::NationalStringLiteral(text)
                            if index == 1 || index == 2 =>
                        {
                            Some(text.clone())
                        }
                        Value::SingleQuotedString(_)
                        | Value::NationalStringLiteral(_)
                        | Value::HexStringLiteral(_) => None,
                        _ => Some(value.to_string()),
                    },
                    Expr::UnaryOp {
                        op: UnaryOperator::Minus,
                        expr,
                    } if matches!(expr.as_ref(), Expr::Value(v) if matches!(v.value, Value::Number(_, _))) => {
                        None
                    }
                    Expr::UnaryOp {
                        op: UnaryOperator::Minus,
                        expr,
                    } => Some(expr.to_string()),
                    Expr::UnaryOp { op, .. } => Some(op.to_string()),
                    Expr::BinaryOp { op, .. } => Some(op.to_string()),
                    Expr::Nested(_) => Some("(".into()),
                    _ => Some(expr.to_string()),
                };
                if let Some(token) = token {
                    return std::ops::ControlFlow::Break(SqlError::syntax(
                        102,
                        1,
                        format!("Incorrect syntax near '{token}'."),
                    ));
                }
                if index >= 3
                    && let Expr::Identifier(id) = expr
                    && let Some(parameter) = self.0.get(&id.value.to_lowercase())
                    && !matches!(
                        parameter.data_type,
                        Type::TinyInt
                            | Type::SmallInt
                            | Type::Int
                            | Type::BigInt
                            | Type::Character(_)
                            | Type::Binary(_)
                    )
                {
                    let kind = crate::sql_type::ast(parameter.data_type)
                        .to_string()
                        .to_lowercase()
                        .replace(", ", ",");
                    return std::ops::ControlFlow::Break(SqlError::new(
                        2748,
                        1,
                        format!(
                            "Cannot specify {kind} data type (parameter {}) as a substitution parameter.",
                            index + 1
                        ),
                    ));
                }
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    match Visit::visit(statement, &mut Check(parameters)) {
        std::ops::ControlFlow::Continue(()) => Ok(()),
        std::ops::ControlFlow::Break(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arguments_and_options_survive_parsing_and_expression_visitors() {
        let mut statements = Parser::parse_sql(
            &crate::dialect::ServerDialect,
            "RAISERROR(N'%s:%d',16,7,@text,-42) WITH NOWAIT, SETERROR; SELECT 1",
        )
        .unwrap();
        assert_eq!(statements.len(), 2);
        let raised = call(&statements[0]).unwrap();
        assert_eq!(
            raised.options,
            Options {
                log: false,
                nowait: true,
                seterror: true
            }
        );
        assert_eq!(raised.message.to_string(), "N'%s:%d'");
        assert_eq!(raised.severity.to_string(), "16");
        assert_eq!(raised.state.to_string(), "7");
        assert_eq!(
            raised
                .substitutions
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>(),
            ["@text", "-42"]
        );
        struct Replace;
        impl VisitorMut for Replace {
            type Break = ();
            fn post_visit_expr(&mut self, expr: &mut Expr) -> std::ops::ControlFlow<()> {
                if matches!(expr,Expr::Identifier(id) if id.value=="@text") {
                    *expr = Expr::Value(Value::SingleQuotedString("duck".into()).into());
                }
                std::ops::ControlFlow::Continue(())
            }
        }
        let _ = VisitMut::visit(&mut statements[0], &mut Replace);
        assert_eq!(
            call(&statements[0]).unwrap().substitutions[0].to_string(),
            "'duck'"
        );
        for sql in [
            "RAISERROR('x',16)",
            "RAISERROR('x',16,1) WITH INVALID",
            "RAISERROR('x',16,1) WITH",
        ] {
            assert!(
                Parser::parse_sql(&crate::dialect::ServerDialect, sql).is_err(),
                "{sql}"
            );
        }
    }
}
