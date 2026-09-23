use sqlparser::ast::*;

pub use msduck_sql::expression_metadata::conditional::isnull_args as args;

pub use msduck_sql::expression_metadata::conditional::binary_args;

pub use msduck_sql::expression_metadata::conditional::literal_null;

pub fn lower(expr: &mut Expr) -> Result<(), String> {
    let Expr::Function(function) = expr else {
        return Ok(());
    };
    let Some([first, replacement]) = args(function)? else {
        return Ok(());
    };
    let text_type = crate::result_types::isnull_type(first, replacement);
    if literal_null(first) {
        *expr = if literal_null(replacement) {
            Expr::Cast {
                kind: CastKind::Cast,
                expr: Box::new(replacement.clone()),
                data_type: DataType::Int(None),
                format: None,
            }
        } else {
            replacement.clone()
        };
        // A replacement can itself be ISNULL; pre-visit does not revisit this node.
        return lower(expr);
    }
    if let Some(crate::tds::Type::Char(width)) = &text_type {
        let converted = crate::engine::binary_function(
            "__msduck_cast_char",
            replacement.clone(),
            Expr::Value(Value::Number(width.to_string(), false).into()),
        );
        if let FunctionArguments::List(args) = &mut function.args {
            args.args[1] = FunctionArg::Unnamed(FunctionArgExpr::Expr(converted));
        }
    }
    function.name = ObjectName::from(vec![Ident::new("__msduck_isnull")]);
    if let Some(kind) = text_type {
        let (name, width) = match kind {
            crate::tds::Type::Nchar(width) => ("__msduck_nchar_width", width),
            crate::tds::Type::Nvarchar(width) => ("__msduck_nvarchar_width", width),
            crate::tds::Type::Char(width) => ("__msduck_char_width", width),
            crate::tds::Type::Varchar(u16::MAX) => return Ok(()),
            crate::tds::Type::Varchar(width) => ("__msduck_varchar_width", width),
            _ => return Ok(()),
        };
        *expr = crate::engine::binary_function(
            name,
            expr.clone(),
            Expr::Value(Value::Number(width.to_string(), false).into()),
        );
    }
    Ok(())
}
