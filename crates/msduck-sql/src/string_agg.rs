//! STRING_AGG call, declaration and accumulation rules captured in
//! reference/string-agg.json. Binding of source declarations and runtime
//! execution belong to the root adapter.
use msduck_core::{
    character::{CharacterType, Family, Length},
    diagnostic::SqlError,
    types::Type,
};
use sqlparser::ast::*;

/// Maximum bytes of a bounded STRING_AGG result.
pub const BOUNDED_LIMIT: usize = 8000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Sql(SqlError),
    /// A form the captures do not cover. Callers must reject it explicitly
    /// instead of guessing SQL Server's behavior.
    Unsupported(&'static str),
}
impl From<SqlError> for Error {
    fn from(error: SqlError) -> Self {
        Self::Sql(error)
    }
}

/// Separator argument forms accepted by SQL Server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Separator<'a> {
    Literal {
        text: &'a str,
        unicode: bool,
    },
    /// `CAST(NULL AS <bounded character type>)`; concatenates without a separator.
    TypedNull(CharacterType),
    /// A variable or parameter whose declaration the caller binds.
    Variable(&'a str),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Call<'a> {
    pub value: &'a Expr,
    pub separator: Separator<'a>,
    /// WITHIN GROUP keys; empty when unordered. Unordered concatenation has
    /// no ordering guarantee.
    pub order_by: &'a [OrderByExpr],
}

fn distinct() -> SqlError {
    SqlError::syntax(102, 1, "Incorrect syntax near ','.")
}
fn separator_form() -> SqlError {
    SqlError::new(
        8733,
        1,
        "Separator parameter for STRING_AGG must be a string literal or variable.",
    )
}
fn large_separator() -> SqlError {
    SqlError::new(
        8734,
        1,
        "Separator parameter for STRING_AGG cannot be large object type such as VARCHAR(MAX) or NVARCHAR(MAX).",
    )
}
fn invalid_argument(type_name: &str, position: u8) -> SqlError {
    SqlError::new(
        8116,
        1,
        format!(
            "Argument data type {type_name} is invalid for argument {position} of string_agg function."
        ),
    )
}

/// Recognize and validate a STRING_AGG call. Returns `None` for other functions.
pub fn call(function: &Function) -> Result<Option<Call<'_>>, Error> {
    if !function.name.to_string().eq_ignore_ascii_case("STRING_AGG") {
        return Ok(None);
    }
    let FunctionArguments::List(args) = &function.args else {
        return Err(Error::Unsupported("STRING_AGG requires an argument list"));
    };
    if matches!(args.duplicate_treatment, Some(DuplicateTreatment::Distinct)) {
        return Err(distinct().into());
    }
    if function.over.is_some() {
        return Err(SqlError::syntax(
            4113,
            4,
            "The function 'STRING_AGG' is not a valid windowing function, and cannot be used with the OVER clause.",
        )
        .into());
    }
    if args.duplicate_treatment.is_some()
        || !args.clauses.is_empty()
        || !matches!(function.parameters, FunctionArguments::None)
        || function.filter.is_some()
        || function.null_treatment.is_some()
    {
        return Err(Error::Unsupported("Unsupported STRING_AGG modifiers"));
    }
    let [
        FunctionArg::Unnamed(FunctionArgExpr::Expr(value)),
        FunctionArg::Unnamed(FunctionArgExpr::Expr(separator)),
    ] = args.args.as_slice()
    else {
        return Err(Error::Unsupported("STRING_AGG argument count"));
    };
    Ok(Some(Call {
        value,
        separator: separator_arg(separator)?,
        order_by: &function.within_group,
    }))
}

fn separator_arg(expr: &Expr) -> Result<Separator<'_>, Error> {
    match expr {
        Expr::Nested(inner) => separator_arg(inner),
        Expr::Value(v) => match &v.value {
            Value::SingleQuotedString(text) => Ok(Separator::Literal {
                text,
                unicode: false,
            }),
            Value::NationalStringLiteral(text) => Ok(Separator::Literal {
                text,
                unicode: true,
            }),
            Value::Placeholder(name) if name.starts_with('@') => Ok(Separator::Variable(name)),
            // Integer literals are accepted as literals and then fail typing.
            Value::Number(text, _) if text.parse::<i32>().is_ok() => {
                Err(invalid_argument("int", 2).into())
            }
            _ => Err(Error::Unsupported("STRING_AGG separator literal type")),
        },
        Expr::Identifier(ident) if ident.value.starts_with('@') => {
            Ok(Separator::Variable(&ident.value))
        }
        Expr::Cast {
            expr, data_type, ..
        } => {
            let target = crate::sql_type::declaration(data_type)
                .map_err(|_| Error::Unsupported("STRING_AGG separator CAST target"))?;
            match target {
                Type::Character(kind) if kind.length() == Length::Max => {
                    Err(separator_form().into())
                }
                Type::Character(kind) if matches!(&**expr, Expr::Value(v) if matches!(v.value, Value::Null)) => {
                    Ok(Separator::TypedNull(kind))
                }
                _ => Err(Error::Unsupported("STRING_AGG separator expression")),
            }
        }
        _ => Err(Error::Unsupported("STRING_AGG separator expression")),
    }
}

/// Output declaration for a source declaration. `Ok(None)` means the result
/// family has not been captured and must not be inferred.
pub fn output(source: Type) -> Result<Option<CharacterType>, SqlError> {
    let (family, length) = match source {
        Type::Character(kind) => match (kind.family(), kind.length()) {
            (Family::Varchar, Length::Bounded(_)) => (Family::Varchar, Length::Bounded(8000)),
            (Family::Nvarchar, Length::Bounded(_)) => (Family::Nvarchar, Length::Bounded(4000)),
            (family @ (Family::Varchar | Family::Nvarchar), Length::Max) => (family, Length::Max),
            _ => return Ok(None),
        },
        // Formatted as text by SQL Server; see the retained rows for exact text.
        Type::Int | Type::Decimal(_) | Type::DateTime2(_) => {
            (Family::Nvarchar, Length::Bounded(4000))
        }
        Type::Binary(kind) if !kind.fixed() => return Err(invalid_argument("varbinary", 1)),
        _ => return Ok(None),
    };
    Ok(Some(
        CharacterType::new(family, length).expect("captured STRING_AGG output length"),
    ))
}

/// Check a separator declaration against the output declaration.
/// `variable` distinguishes bound variables, whose MAX declarations are rejected.
pub fn check_separator(
    output: CharacterType,
    separator: Type,
    variable: bool,
) -> Result<Option<()>, SqlError> {
    match separator {
        Type::Character(kind) => {
            if kind.length() == Length::Max {
                return if variable {
                    Err(large_separator())
                } else {
                    Ok(None)
                };
            }
            match (output.family(), kind.family()) {
                (Family::Varchar, Family::Nvarchar) => Err(invalid_argument("nvarchar", 2)),
                (Family::Varchar | Family::Nvarchar, Family::Varchar | Family::Nvarchar) => {
                    Ok(Some(()))
                }
                _ => Ok(None),
            }
        }
        Type::Int => Err(invalid_argument("int", 2)),
        _ => Ok(None),
    }
}

/// Declaration of a separator literal as SQL Server types it.
pub fn literal_type(text: &str, unicode: bool) -> Option<Type> {
    let units = text.encode_utf16().count().max(1);
    let (family, units) = if unicode {
        (Family::Nvarchar, u16::try_from(units).ok()?)
    } else {
        (Family::Varchar, u16::try_from(units).ok()?)
    };
    CharacterType::new(family, Length::Bounded(units))
        .ok()
        .map(Type::Character)
}

/// Group accumulator. NULL values are skipped without a separator; a NULL
/// separator concatenates directly; an empty or all-NULL group yields NULL.
///
/// VARCHAR byte counts assume the captured single-byte (CP1252) collations,
/// where each UTF-16 unit converts to one byte. Values must already be
/// converted to the output family.
#[derive(Clone, Debug)]
pub struct Accumulator {
    output: CharacterType,
    value: Option<String>,
    bytes: usize,
}
impl Accumulator {
    pub fn new(output: CharacterType) -> Self {
        Self {
            output,
            value: None,
            bytes: 0,
        }
    }
    fn bytes(&self, text: &str) -> usize {
        let units = text.encode_utf16().count();
        match self.output.family() {
            Family::Nvarchar | Family::Nchar => units * 2,
            Family::Varchar | Family::Char => units,
        }
    }
    pub fn push(&mut self, value: Option<&str>, separator: Option<&str>) -> Result<(), SqlError> {
        let Some(value) = value else {
            return Ok(());
        };
        let separator = if self.value.is_some() {
            separator.unwrap_or("")
        } else {
            ""
        };
        let bytes = self.bytes + self.bytes(separator) + self.bytes(value);
        if self.output.length() != Length::Max && bytes > BOUNDED_LIMIT {
            // Captured states differ by output family.
            let state = u8::from(self.output.family() == Family::Nvarchar);
            return Err(SqlError::new(
                9829,
                state,
                "STRING_AGG aggregation result exceeded the limit of 8000 bytes. Use LOB types to avoid result truncation.",
            ));
        }
        let text = self.value.get_or_insert_with(String::new);
        text.push_str(separator);
        text.push_str(value);
        self.bytes = bytes;
        Ok(())
    }
    pub fn finish(self) -> Option<String> {
        self.value
    }
}

/// Ordered STRING_AGG calls in one scope must share their WITHIN GROUP list.
/// Mixing ordered and unordered calls has not been captured.
pub fn check_orderings(orderings: &[&[OrderByExpr]]) -> Result<(), Error> {
    let ordered: Vec<_> = orderings.iter().filter(|o| !o.is_empty()).collect();
    if ordered.windows(2).any(|w| w[0] != w[1]) {
        return Err(SqlError::new(
            8711,
            1,
            "Multiple ordered aggregate functions in the same scope have mutually incompatible orderings.",
        )
        .into());
    }
    if !ordered.is_empty() && ordered.len() != orderings.len() {
        return Err(Error::Unsupported(
            "Mixing ordered and unordered STRING_AGG in one scope",
        ));
    }
    Ok(())
}
