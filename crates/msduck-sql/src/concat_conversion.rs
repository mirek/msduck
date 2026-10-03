//! Declaration-only function planning and bounded conversion of acquired values.
//! No source expression is evaluated here; storage construction remains an adapter.
use crate::{
    concat_text_conversion as conversion, concat_ws as function, numeric_text,
    temporal_guid_text as temporal,
};
use msduck_core::{
    character::{Family, Length},
    collation::Label,
    types::Type,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub source: Option<Type>,
    pub collation: Option<Label>,
    /// Function implicit conversion only: explicit styles are separate operations.
    pub style: Option<i32>,
}
impl Declaration {
    pub fn null_literal() -> Self {
        Self {
            source: None,
            collation: None,
            style: None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Stored<'a> {
    /// Original character/legacy text decoded under its explicit source encoding.
    Character(&'a [u16]),
    Binary(&'a [u8]),
    Numeric(numeric_text::Value),
    Temporal(temporal::Stored),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Function(function::Error),
    Conversion(conversion::Unsupported),
    Numeric(numeric_text::Error),
    Temporal(temporal::Error),
    InvalidPayload,
    AggregateInputLimit,
}
#[derive(Clone, Debug)]
enum Converter {
    Null,
    Character,
    Binary(conversion::Contract),
    Numeric(Type),
    Temporal(temporal::Contract),
}
#[derive(Clone, Debug)]
pub struct Plan {
    function: function::Plan,
    converters: Vec<Converter>,
}
impl Plan {
    pub fn result(&self) -> &function::Plan {
        &self.function
    }
}
fn unicode(source: Option<Type>) -> bool {
    source == Some(Type::Ntext)
        || matches!(source, Some(Type::Character(c)) if matches!(c.family(), Family::Nchar | Family::Nvarchar))
}
fn is_temporal(source: Type) -> bool {
    matches!(
        source,
        Type::Date
            | Type::Time(_)
            | Type::DateTime2(_)
            | Type::DateTimeOffset(_)
            | Type::DateTime
            | Type::SmallDateTime
            | Type::UniqueIdentifier
    )
}
fn conversion_function(function: function::Function) -> conversion::Function {
    match function {
        function::Function::ConcatWs => conversion::Function::ConcatWs,
        function::Function::Translate => conversion::Function::Translate,
    }
}
/// The complete plan depends only on declarations, explicit session inputs and
/// catalog properties. Unknown allocations do not depend on current values.
pub fn plan(
    operation: function::Function,
    declarations: &[Declaration],
    default_collation: &str,
    catalog: &[function::Collation],
    language: temporal::Language,
    context: Option<function::DiagnosticContext>,
) -> Result<Plan, Error> {
    function::validate_arity(operation, declarations.len()).map_err(Error::Function)?;
    let domain = if declarations.iter().any(|d| unicode(d.source)) {
        conversion::Domain::Utf16Le
    } else {
        conversion::Domain::Cp1252
    };
    let arguments: Vec<_> = declarations
        .iter()
        .map(|d| {
            let width = match d.source {
                None => Some(Length::Bounded(0)),
                Some(Type::Character(c)) => Some(c.length()),
                Some(source) if is_temporal(source) => Some(match operation {
                    function::Function::ConcatWs => Length::Bounded(40),
                    function::Function::Translate => {
                        Length::Bounded(if domain == conversion::Domain::Utf16Le {
                            4000
                        } else {
                            8000
                        })
                    }
                }),
                Some(source) => {
                    conversion::contract(conversion_function(operation), source, domain, None)
                        .ok()
                        .map(|c| c.allocation)
                }
            };
            function::Argument {
                kind: d.source,
                converted_width: width,
                collation: d.collation.clone(),
            }
        })
        .collect();
    // Preserve the function's captured arity/type/collation diagnostic precedence.
    let function =
        function::plan_with_context(operation, &arguments, default_collation, catalog, context)
            .map_err(Error::Function)?;
    if matches!(
        function.declaration.family(),
        Family::Char | Family::Varchar
    ) && function.encoding != function::Encoding::Cp1252
    {
        return Err(Error::Function(function::Error::UnknownEncoding));
    }
    let mut converters = Vec::with_capacity(declarations.len());
    for declaration in declarations {
        if declaration.collation.is_some()
            && !matches!(
                declaration.source,
                Some(Type::Character(_) | Type::Text | Type::Ntext)
            )
        {
            return Err(Error::Function(function::Error::InvalidDeclaration));
        }
        if declaration.style.is_some() {
            return Err(Error::Conversion(conversion::Unsupported::Style));
        }
        converters.push(match declaration.source {
            None => Converter::Null,
            Some(Type::Character(_) | Type::Text | Type::Ntext) => Converter::Character,
            Some(source @ Type::Binary(_)) => Converter::Binary(
                conversion::contract(conversion_function(operation), source, domain, None)
                    .map_err(Error::Conversion)?,
            ),
            Some(source) if is_temporal(source) => Converter::Temporal(
                temporal::contract(
                    source,
                    match operation {
                        function::Function::ConcatWs => temporal::Profile::ConcatWs,
                        function::Function::Translate => temporal::Profile::Translate,
                    },
                    language,
                    if domain == conversion::Domain::Utf16Le {
                        temporal::Domain::Utf16
                    } else {
                        temporal::Domain::Cp1252
                    },
                )
                .map_err(Error::Temporal)?,
            ),
            Some(source) => {
                conversion::contract(conversion_function(operation), source, domain, None)
                    .map_err(Error::Conversion)?;
                numeric_text::text(source, None, None).map_err(Error::Numeric)?;
                Converter::Numeric(source)
            }
        });
    }
    Ok(Plan {
        function,
        converters,
    })
}

/// Bound aggregate temporary conversion storage before allocating any text.
/// The caller still owns the original payloads; this limit is an implementation
/// barrier, not a SQL Server capacity or fabricated SQL diagnostic.
fn converted_values(
    plan: &Plan,
    values: &[Option<Stored<'_>>],
) -> Result<Vec<Option<Vec<u16>>>, Error> {
    if values.len() != plan.converters.len() {
        return Err(Error::InvalidPayload);
    }
    let mut total = 0usize;
    for value in values.iter().flatten() {
        let units = match value {
            Stored::Character(v) => v.len(),
            Stored::Binary(v) => v.len(),
            Stored::Numeric(_) => 41,
            Stored::Temporal(_) => 40,
        };
        total = total.checked_add(units).ok_or(Error::AggregateInputLimit)?;
        if total > function::MAX_INPUT_UNITS {
            return Err(Error::AggregateInputLimit);
        }
    }
    plan.converters
        .iter()
        .zip(values)
        .map(|(converter, value)| {
            let converted = match (converter, value) {
                (Converter::Null, None) => None,
                (Converter::Character, None) => return Ok(None),
                (Converter::Character, Some(Stored::Character(v))) => return Ok(Some(v.to_vec())),
                (Converter::Binary(c), None) => {
                    conversion::binary_text(*c, None).map_err(Error::Conversion)?
                }
                (Converter::Binary(c), Some(Stored::Binary(v))) => {
                    conversion::binary_text(*c, Some(v)).map_err(Error::Conversion)?
                }
                (Converter::Numeric(source), value) => {
                    let value = match value {
                        None => None,
                        Some(Stored::Numeric(v)) => Some(*v),
                        _ => return Err(Error::InvalidPayload),
                    };
                    return numeric_text::text(*source, value, None)
                        .map_err(Error::Numeric)
                        .map(|v| v.map(|s| s.encode_utf16().collect()));
                }
                (Converter::Temporal(c), value) => {
                    let value = match value {
                        None => None,
                        Some(Stored::Temporal(v)) => Some(*v),
                        _ => return Err(Error::InvalidPayload),
                    };
                    return temporal::format(*c, value)
                        .map_err(Error::Temporal)
                        .map(|v| {
                            v.map(|text| match text {
                                temporal::Text::Ansi(s) => s.encode_utf16().collect(),
                                temporal::Text::Unicode(v) => v,
                            })
                        });
                }
                _ => return Err(Error::InvalidPayload),
            };
            Ok(converted.map(|text| match text {
                conversion::Text::Ansi(s) => s.encode_utf16().collect(),
                conversion::Text::Unicode(v) => v,
            }))
        })
        .collect()
}
pub fn evaluate(
    plan: &Plan,
    values: &[Option<Stored<'_>>],
    matches: &impl Fn(&[u16], &[u16]) -> Option<bool>,
) -> Result<Option<Vec<u16>>, Error> {
    function::evaluate(&plan.function, &converted_values(plan, values)?, matches)
        .map_err(Error::Function)
}
pub fn evaluate_with_keys<K: Ord>(
    plan: &Plan,
    values: &[Option<Stored<'_>>],
    key: &impl Fn(&[u16]) -> Option<K>,
) -> Result<Option<Vec<u16>>, Error> {
    function::evaluate_with_keys(&plan.function, &converted_values(plan, values)?, key)
        .map_err(Error::Function)
}
