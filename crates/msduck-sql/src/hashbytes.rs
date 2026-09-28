//! Deterministic SQL declaration binding for HASHBYTES.
//!
//! The digest algorithm and byte encoding are separate core/adapter concerns.
//! This binder depends only on argument declarations, never runtime values.

use msduck_core::{
    character::{Family, Length},
    diagnostic::SqlError,
    types::{BinaryType, Type},
};

/// An untyped NULL literal differs from a typed NULL parameter or CAST.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Argument {
    UntypedNull,
    Known(Type),
    NumericLiteral,
    Uncaptured,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BindError {
    Sql(SqlError),
    Unsupported(&'static str),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BoundFunction {
    pub result: Type,
    pub nullable: bool,
}

enum Checked {
    Accepted,
    Rejected(&'static str),
    Unknown,
}

fn check(argument: Argument, position: usize) -> Checked {
    match (position, argument) {
        (1, Argument::Known(Type::Character(kind)))
            if matches!(kind.family(), Family::Varchar | Family::Nvarchar) =>
        {
            Checked::Accepted
        }
        (2, Argument::Known(Type::Character(_))) => Checked::Accepted,
        (2, Argument::Known(Type::Binary(kind))) if !kind.fixed() => Checked::Accepted,
        (_, Argument::UntypedNull) => Checked::Rejected("NULL"),
        (_, Argument::Known(Type::Int)) => Checked::Rejected("int"),
        (2, Argument::NumericLiteral) => Checked::Rejected("numeric"),
        (2, Argument::Known(Type::DateTime)) => Checked::Rejected("datetime"),
        (2, Argument::Known(Type::UniqueIdentifier)) => Checked::Rejected("uniqueidentifier"),
        (2, Argument::Known(Type::Xml)) => Checked::Rejected("xml"),
        (2, Argument::Known(Type::Text)) => Checked::Rejected("text"),
        _ => Checked::Unknown,
    }
}

/// Preserve the captured compile-time error order: the first argument is
/// checked before the second. Two invalid arguments were not captured and are
/// left unsupported rather than assigning an invented precedence.
pub fn bind(args: &[Argument]) -> Result<BoundFunction, BindError> {
    if args.len() != 2 {
        let captured_arity = !args.is_empty()
            && args.iter().all(|argument| {
                matches!(argument, Argument::Known(Type::Character(kind)) if kind.family() == Family::Varchar)
            });
        return Err(if captured_arity {
            BindError::Sql(SqlError::syntax(
                174,
                1,
                "The hashbytes function requires 2 argument(s).",
            ))
        } else {
            BindError::Unsupported("uncaptured HASHBYTES arity and argument combination")
        });
    }
    match (check(args[0], 1), check(args[1], 2)) {
        (Checked::Accepted, Checked::Accepted) => Ok(BoundFunction {
            result: Type::Binary(
                BinaryType::new(false, Length::Bounded(8000)).expect("valid VARBINARY(8000)"),
            ),
            nullable: true,
        }),
        (Checked::Rejected(name), Checked::Accepted) => Err(invalid(name, 1)),
        (Checked::Accepted, Checked::Rejected(name)) => Err(invalid(name, 2)),
        _ => Err(BindError::Unsupported(
            "uncaptured HASHBYTES argument type combination",
        )),
    }
}

fn invalid(name: &str, position: usize) -> BindError {
    BindError::Sql(SqlError::new(
        8116,
        1,
        format!(
            "Argument data type {name} is invalid for argument {position} of hashbytes function."
        ),
    ))
}
