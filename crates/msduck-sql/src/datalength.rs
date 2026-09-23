//! SQL storage-length expression lowering.
use crate::parameter::Parameter;
use sqlparser::ast::*;
use std::collections::HashMap;

pub use crate::expression_metadata::storage::fixed_width;
pub use crate::expression_metadata::storage::kind;
// Restore SQL declarations rather than inferring widths from DuckDB's DECIMAL
// backing type (MONEY and SMALLMONEY share decimal storage with other types).
pub use crate::expression_metadata::storage::catalog_scalar;

fn cast(expr: Expr, data_type: DataType) -> Expr {
    Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(expr),
        data_type,
        format: None,
    }
}
pub fn lower(
    expr: &mut Expr,
    parameters: &HashMap<String, Parameter>,
    column: &impl Fn(&Expr) -> Option<DataType>,
) -> Result<(), String> {
    let Expr::Function(function) = expr else {
        return Ok(());
    };
    let is_len = function.name.to_string().eq_ignore_ascii_case("LEN");
    let Some(value) =
        crate::function_args::unary(function, if is_len { "LEN" } else { "DATALENGTH" })?
    else {
        return Ok(());
    };
    let Some(declared) = kind(value, parameters, column) else {
        // Existing LEN lowering handles expressions whose type is not yet known.
        return if is_len {
            Ok(())
        } else {
            Err("unsupported DATALENGTH source type".into())
        };
    };
    let large = matches!(
        declared,
        DataType::Varchar(Some(CharacterLength::Max))
            | DataType::CharacterVarying(Some(CharacterLength::Max))
            | DataType::CharVarying(Some(CharacterLength::Max))
            | DataType::Nvarchar(Some(CharacterLength::Max))
            | DataType::Varbinary(Some(BinaryLength::Max))
    );
    let native = match &declared {
        DataType::Varchar(_)
        | DataType::Char(_)
        | DataType::Character(_)
        | DataType::CharacterVarying(_)
        | DataType::CharVarying(_) => Some("__msduck_datalength_ansi"),
        DataType::Nvarchar(_) => Some("__msduck_datalength_unicode"),
        DataType::Custom(n, _) if n.to_string().eq_ignore_ascii_case("nchar") => {
            Some("__msduck_datalength_unicode")
        }
        DataType::Binary(_) | DataType::Varbinary(_) => Some("__msduck_datalength_binary"),
        _ => None,
    };
    let unicode = native == Some("__msduck_datalength_unicode");
    let result = if unicode {
        crate::expr::unary_function(
            if is_len {
                "__msduck_carrier_len"
            } else {
                "__msduck_carrier_datalength"
            },
            crate::expr::unary_function("__msduck_pack_unicode", value.clone()),
        )
    } else if is_len {
        crate::expr::unary_function("__msduck_len", value.clone())
    } else if let Some(name) = native {
        // Keep binary values as BLOBs, and use an unbounded text cast for strings.
        let value = if name.ends_with("binary") {
            value.clone()
        } else {
            cast(value.clone(), DataType::Text)
        };
        crate::expr::unary_function(name, value)
    } else {
        let width = fixed_width(&declared).ok_or("unsupported DATALENGTH source type")?;
        Expr::Case {
            case_token: helpers::attached_token::AttachedToken::empty(),
            end_token: helpers::attached_token::AttachedToken::empty(),
            operand: None,
            conditions: vec![CaseWhen {
                condition: Expr::IsNull(Box::new(value.clone())),
                result: Expr::Value(Value::Null.into()),
            }],
            else_result: Some(Box::new(Expr::Value(
                Value::Number(width.to_string(), false).into(),
            ))),
        }
    };
    *expr = cast(
        result,
        if large {
            DataType::BigInt(None)
        } else {
            DataType::Int(None)
        },
    );
    Ok(())
}
