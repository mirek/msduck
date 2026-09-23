//! Date-part keyword and argument syntax, independent of temporal evaluation.
use sqlparser::ast::*;

pub fn args(f: &Function) -> Result<Option<(usize, &Expr)>, String> {
    named_args(f, "DATEPART")
}
pub fn datename_args(f: &Function) -> Result<Option<(usize, &Expr)>, String> {
    named_args(f, "DATENAME")
}
pub fn named_args<'a>(f: &'a Function, name: &str) -> Result<Option<(usize, &'a Expr)>, String> {
    let Some([part, value]) = crate::expression_metadata::conditional::binary_args(f, name)? else {
        return Ok(None);
    };
    let Expr::Identifier(part) = part else {
        return Err(format!("{name} requires a datepart keyword"));
    };
    let part = match part.value.to_ascii_lowercase().as_str() {
        "year" | "yy" | "yyyy" => 0,
        "quarter" | "qq" | "q" => 1,
        "month" | "mm" | "m" => 2,
        "dayofyear" | "dy" | "y" => 3,
        "day" | "dd" | "d" => 4,
        "week" | "wk" | "ww" => 5,
        "weekday" | "dw" | "w" => 6,
        "hour" | "hh" => 7,
        "minute" | "mi" | "n" => 8,
        "second" | "ss" | "s" => 9,
        "millisecond" | "ms" => 10,
        "microsecond" | "mcs" => 11,
        "nanosecond" | "ns" => 12,
        "tzoffset" | "tz" => 13,
        "iso_week" | "isowk" | "isoww" => 14,
        _ => return Err(format!("invalid {name} datepart")),
    };
    Ok(Some((part, value)))
}
