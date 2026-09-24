use sqlparser::ast::*;

pub use crate::expression_metadata::conditional::choose_args as args;

pub fn lower(expr: &mut Expr) -> Result<(), String> {
    let Expr::Function(function) = expr else {
        return Ok(());
    };
    let Some(args) = args(function)? else {
        return Ok(());
    };
    *expr = Expr::Case {
        case_token: helpers::attached_token::AttachedToken::empty(),
        end_token: helpers::attached_token::AttachedToken::empty(),
        operand: Some(Box::new(Expr::Cast {
            kind: CastKind::Cast,
            expr: Box::new(args[0].clone()),
            data_type: DataType::Int(None),
            format: None,
        })),
        conditions: args[1..]
            .iter()
            .enumerate()
            .map(|(i, value)| CaseWhen {
                condition: Expr::Value(Value::Number((i + 1).to_string(), false).into()),
                result: (*value).clone(),
            })
            .collect(),
        else_result: None,
    };
    Ok(())
}
