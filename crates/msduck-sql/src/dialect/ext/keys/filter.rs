//! Filtered index predicates, lowered to DuckDB expressions over stored
//! columns.
//!
//! SQL Server accepts only simple filters: conjunctions of
//! `column comparison constant`, `column IS [NOT] NULL` and
//! `column IN (constant, ...)`. Comparisons follow the column's storage:
//! Unicode carriers compare BIN2 equality keys (ordering comparisons are not
//! supported), temporal structs compare UTC ticks and ANSI text ignores
//! trailing spaces for equality. Only built-in DuckDB functions appear, so
//! the index binds while DuckDB replays its WAL.
use super::value::{Column, Storage};
use msduck_core::{datetime2::DateTime2, datetimeoffset::DateTimeOffset};
use sqlparser::ast::*;

pub const UNSUPPORTED: &str = "unsupported filtered index predicate";

/// SQL Server's error for a filter column that does not exist.
pub fn invalid_column(name: &str) -> (i32, u8, u8, String) {
    (207, 1, 16, format!("Invalid column name '{name}'."))
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Unsupported,
    InvalidColumn(String),
}

enum Constant {
    Number(String),
    Text(String),
}

fn constant(expr: &Expr) -> Option<Constant> {
    match expr {
        Expr::Value(value) => match &value.value {
            Value::Number(n, _) => Some(Constant::Number(n.clone())),
            Value::SingleQuotedString(s) | Value::NationalStringLiteral(s) => {
                Some(Constant::Text(s.clone()))
            }
            _ => None,
        },
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } => match constant(expr)? {
            Constant::Number(n) => Some(Constant::Number(format!("-{n}"))),
            Constant::Text(_) => None,
        },
        Expr::Nested(expr) => constant(expr),
        _ => None,
    }
}

fn column<'a>(expr: &Expr, columns: &'a [Column]) -> Option<Result<&'a Column, Error>> {
    let ident = match expr {
        Expr::Identifier(ident) => ident,
        Expr::CompoundIdentifier(parts) if parts.len() == 1 => &parts[0],
        Expr::Nested(expr) => return column(expr, columns),
        _ => return None,
    };
    Some(
        columns
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(&ident.value))
            .ok_or_else(|| Error::InvalidColumn(ident.value.clone())),
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// A VARCHAR constant without its text in the expression.
fn varchar(text: &str) -> String {
    format!("decode(from_hex('{}'))", hex(text.as_bytes()))
}

fn operator(op: &BinaryOperator) -> Option<&'static str> {
    Some(match op {
        BinaryOperator::Eq => "=",
        BinaryOperator::NotEq => "<>",
        BinaryOperator::Lt => "<",
        BinaryOperator::LtEq => "<=",
        BinaryOperator::Gt => ">",
        BinaryOperator::GtEq => ">=",
        _ => return None,
    })
}

fn flip(op: &'static str) -> &'static str {
    match op {
        "<" => ">",
        "<=" => ">=",
        ">" => "<",
        ">=" => "<=",
        other => other,
    }
}

fn comparison(column: &Column, op: &str, value: Constant) -> Result<String, Error> {
    let q = column.quoted();
    let text = match &value {
        Constant::Number(n) | Constant::Text(n) => n.clone(),
    };
    Ok(match column.kind().map_err(|_| Error::Unsupported)? {
        // Equality only: the index must bind with built-in functions, and
        // they cannot order UTF-16 code units.
        Storage::Unicode if matches!(op, "=" | "<>") => format!(
            "({} {op} '{}')",
            super::value::unicode_key(&q),
            super::value::unicode_key_of(&text)
        ),
        Storage::Unicode => return Err(Error::Unsupported),
        // Text constants are hexadecimal, so no user text appears in the
        // index expression or in DuckDB's duplicate-key message.
        Storage::Ansi if matches!(op, "=" | "<>") => {
            format!(
                "(hex(rtrim({q}, ' ')) {op} '{}')",
                hex(text.trim_end_matches(' ').as_bytes())
            )
        }
        Storage::Ansi => format!("({q} {op} {})", varchar(&text)),
        Storage::Ticks { field, offset } => {
            let ticks = if offset {
                DateTimeOffset::parse_iso(text.trim())
                    .and_then(|v| v.round(column.scale))
                    .map(|v| v.utc().ticks())
            } else {
                DateTime2::parse_iso(text.trim())
                    .and_then(|v| v.round(column.scale))
                    .map(|v| v.ticks())
            }
            .map_err(|_| Error::Unsupported)?;
            format!("(struct_extract({q}, '{field}') {op} {ticks})")
        }
        Storage::Boolean => {
            let value = match text.trim() {
                "0" => "false",
                "1" => "true",
                _ => return Err(Error::Unsupported),
            };
            format!("({q} {op} {value})")
        }
        Storage::Numeric(_) => {
            let number = text.trim();
            if number.parse::<f64>().is_err() {
                return Err(Error::Unsupported);
            }
            format!("({q} {op} {number})")
        }
        Storage::Binary => match value {
            Constant::Number(n) if n.starts_with("0x") || n.starts_with("0X") => {
                format!("({q} {op} from_hex('{}'))", &n[2..])
            }
            _ => return Err(Error::Unsupported),
        },
        Storage::Text => format!("({q} {op} CAST({} AS {}))", varchar(&text), column.storage),
    })
}

/// Lower a filter predicate; the result is true exactly for rows the index
/// covers.
pub fn lower(predicate: &Expr, columns: &[Column]) -> Result<String, Error> {
    match predicate {
        Expr::Nested(expr) => lower(expr, columns),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::And,
            right,
        } => Ok(format!(
            "({} AND {})",
            lower(left, columns)?,
            lower(right, columns)?
        )),
        Expr::IsNull(expr) | Expr::IsNotNull(expr) => {
            let column = column(expr, columns).ok_or(Error::Unsupported)??;
            Ok(format!(
                "({} IS {}NULL)",
                column.quoted(),
                if matches!(predicate, Expr::IsNotNull(_)) {
                    "NOT "
                } else {
                    ""
                }
            ))
        }
        Expr::BinaryOp { left, op, right } => {
            let op = operator(op).ok_or(Error::Unsupported)?;
            if let Some(found) = column(left, columns) {
                comparison(found?, op, constant(right).ok_or(Error::Unsupported)?)
            } else if let Some(found) = column(right, columns) {
                comparison(found?, flip(op), constant(left).ok_or(Error::Unsupported)?)
            } else {
                Err(Error::Unsupported)
            }
        }
        Expr::InList {
            expr,
            list,
            negated: false,
        } => {
            let column = column(expr, columns).ok_or(Error::Unsupported)??;
            let alternatives = list
                .iter()
                .map(|item| comparison(column, "=", constant(item).ok_or(Error::Unsupported)?))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(format!("({})", alternatives.join(" OR ")))
        }
        _ => Err(Error::Unsupported),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn columns() -> Vec<Column> {
        let column = |name: &str, system_type_id, storage: &str| Column {
            name: name.into(),
            system_type_id,
            max_length: 20,
            scale: 3,
            nullable: true,
            storage: storage.into(),
        };
        vec![
            column("email", 231, "STRUCT(__msduck_utf16le BLOB)"),
            column("n", 56, "INTEGER"),
            column("code", 167, "VARCHAR"),
            column("at", 42, "STRUCT(__msduck_datetime2_3 BIGINT)"),
            column("flag", 104, "BOOLEAN"),
        ]
    }

    fn predicate(sql: &str) -> Expr {
        let statements = crate::batch::parse(&format!("SELECT 1 WHERE {sql}")).unwrap();
        let Statement::Query(query) = &statements[0] else {
            panic!()
        };
        let SetExpr::Select(select) = query.body.as_ref() else {
            panic!()
        };
        select.selection.clone().unwrap()
    }

    #[test]
    fn simple_filters_lower_over_storage() {
        let columns = columns();
        assert_eq!(
            lower(&predicate("email IS NOT NULL"), &columns).unwrap(),
            "(\"email\" IS NOT NULL)"
        );
        assert_eq!(
            lower(&predicate("n > 0 AND [flag] = 1"), &columns).unwrap(),
            "((\"n\" > 0) AND (\"flag\" = true))"
        );
        assert_eq!(lower(&predicate("0 < n"), &columns).unwrap(), "(\"n\" > 0)");
        assert_eq!(
            lower(&predicate("code IN ('a', 'b')"), &columns).unwrap(),
            "((hex(rtrim(\"code\", ' ')) = '61') OR (hex(rtrim(\"code\", ' ')) = '62'))"
        );
        assert_eq!(
            lower(&predicate("email = N'a '"), &columns).unwrap(),
            "(regexp_replace(hex(struct_extract(\"email\", '__msduck_utf16le')), '(2000)+$', '') = '6100')"
        );
        assert_eq!(
            lower(&predicate("email > N'a'"), &columns),
            Err(Error::Unsupported)
        );
        assert!(
            lower(&predicate("at >= '2024-01-02'"), &columns)
                .unwrap()
                .starts_with("(struct_extract(\"at\", '__msduck_datetime2_3') >= ")
        );
        assert_eq!(
            lower(&predicate("n > 0 OR n < -5"), &columns),
            Err(Error::Unsupported)
        );
        assert_eq!(
            lower(&predicate("abs(n) > 0"), &columns),
            Err(Error::Unsupported)
        );
        assert_eq!(
            lower(&predicate("missing IS NULL"), &columns),
            Err(Error::InvalidColumn("missing".into()))
        );
    }
}
