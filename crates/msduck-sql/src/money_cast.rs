//! Currency conversion plans; execution and native storage belong to adapters.
use msduck_core::money::MoneyType;
use sqlparser::ast::*;

pub fn money_type(kind: &DataType) -> Option<MoneyType> {
    let DataType::Custom(name, args) = kind else {
        return None;
    };
    if !args.is_empty() {
        return None;
    }
    match name.to_string().to_ascii_lowercase().as_str() {
        "money" => Some(MoneyType::Money),
        "smallmoney" => Some(MoneyType::SmallMoney),
        _ => None,
    }
}

/// Apply a logical currency declaration before backend lowering. Matching casts,
/// including TRY casts, retain their existing error boundary.
pub fn coerce(value: &mut Expr, kind: MoneyType) {
    if matches!(value, Expr::Cast { data_type, .. } | Expr::Convert { data_type: Some(data_type), .. }
        if money_type(data_type) == Some(kind))
    {
        return;
    }
    *value = Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(value.clone()),
        data_type: crate::expression_metadata::currency::declaration(kind),
        format: None,
    };
}

/// Round/convert exactly once before range validation. TRY must cover both
/// backend conversion failures and the narrower currency range failure.
pub fn convert(value: Expr, kind: MoneyType, trying: bool) -> Expr {
    let function = match (kind, trying) {
        (MoneyType::Money, false) => "__msduck_money_convert",
        (MoneyType::Money, true) => "__msduck_try_money_convert",
        (MoneyType::SmallMoney, false) => "__msduck_smallmoney_convert",
        (MoneyType::SmallMoney, true) => "__msduck_try_smallmoney_convert",
    };
    let checked = crate::expr::unary_function(function, value);
    // DuckDB can fold a NULL scalar call to an untyped NULL. Keep the declared
    // result type outside the callback for empty and failed TRY conversions.
    Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(checked),
        data_type: DataType::Decimal(ExactNumberInfo::PrecisionAndScale(
            match kind {
                MoneyType::Money => 19,
                MoneyType::SmallMoney => 10,
            },
            4,
        )),
        format: None,
    }
}

pub fn lower(expr: &mut Expr) {
    let (value, kind, trying) = match expr {
        Expr::Cast {
            expr: value,
            data_type,
            kind,
            format: None,
        } => (
            value,
            money_type(data_type),
            matches!(kind, CastKind::TryCast | CastKind::SafeCast),
        ),
        Expr::Convert {
            expr: value,
            data_type: Some(kind),
            is_try,
            charset: None,
            styles,
            ..
        } if styles.is_empty() => (value, money_type(kind), *is_try),
        _ => return,
    };
    if let Some(kind) = kind {
        *expr = convert(*value.clone(), kind, trying);
    }
}

pub fn column(column: &mut ColumnDef) {
    let Some(kind) = money_type(&column.data_type) else {
        return;
    };
    for option in &mut column.options {
        if let ColumnOption::Default(value) = &mut option.option {
            *value = convert(value.clone(), kind, false);
        }
    }
}
