//! Deterministic plans for integer SQL_VARIANT materialization.
use sqlparser::ast::*;

pub fn lower(expr: &mut Expr) {
    let argument = match expr {
        Expr::Cast {
            expr,
            data_type,
            format: None,
            ..
        } if is_variant(data_type) => Some(expr.as_ref().clone()),
        Expr::Convert {
            expr,
            data_type: Some(data_type),
            charset: None,
            styles,
            ..
        } if is_variant(data_type) && styles.is_empty() => Some(expr.as_ref().clone()),
        _ => None,
    };
    if let Some(argument) = argument {
        *expr = crate::expr::unary_function("__msduck_pack_integer_variant", argument);
    }
}
// Annotation can revisit an expression through a containing conditional or order.
// Canonical keys must stay structurally equal for GROUP BY/GROUPING binding.
pub fn canonicalize(expr: &mut Expr) {
    fn packed(expr: &Expr) -> bool {
        matches!(expr, Expr::Function(f) if f.name.to_string().eq_ignore_ascii_case("__msduck_pack_integer_variant"))
    }
    if packed(expr)
        && let Expr::Function(f) = expr
        && let FunctionArguments::List(args) = &f.args
        && let [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] = args.args.as_slice()
        && packed(value)
    {
        *expr = value.clone();
    }
}
pub fn is_variant(kind: &DataType) -> bool {
    matches!(kind,DataType::Custom(name,args) if args.is_empty()&&name.to_string().eq_ignore_ascii_case("sql_variant"))
}
pub fn storage_type() -> DataType {
    DataType::Struct(
        vec![
            StructField {
                field_name: Some(Ident::new("__msduck_variant_type")),
                field_type: DataType::UTinyInt,
                options: None,
            },
            StructField {
                field_name: Some(Ident::new("__msduck_variant_integer")),
                field_type: DataType::BigInt(None),
                options: None,
            },
        ],
        StructBracketKind::Parentheses,
    )
}
pub fn is_storage(kind: &DataType) -> bool {
    kind == &storage_type()
}
pub fn storage_kind(name: &str) -> Option<DataType> {
    let kind = sqlparser::parser::Parser::new(&sqlparser::dialect::DuckDbDialect {})
        .try_with_sql(name)
        .ok()?
        .parse_data_type()
        .ok()?;
    is_storage(&kind).then_some(kind)
}
pub fn convert(value: Expr) -> Expr {
    if matches!(&value, Expr::Function(f) if f.name.to_string().eq_ignore_ascii_case("__msduck_pack_integer_variant"))
    {
        value
    } else {
        crate::expr::unary_function("__msduck_pack_integer_variant", value)
    }
}
pub fn column(column: &mut ColumnDef) {
    if is_variant(&column.data_type) {
        for option in &mut column.options {
            if let ColumnOption::Default(value) = &mut option.option {
                *value = convert(value.clone());
            }
        }
    }
}
