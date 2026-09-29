//! Declaration-only binding for the captured `AT TIME ZONE` input families.
//!
//! The caller supplies the logical timestamp type. Zone-name validation and
//! transition acquisition belong to the effectful runtime adapter.

use msduck_core::{
    diagnostic::SqlError,
    types::{Scale, Type},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindError {
    Sql(SqlError),
    Unsupported(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundExpression {
    pub result: Type,
    pub nullable: bool,
}

/// Bind only input families whose result shape or error was captured.
/// Runtime values, including the zone name, cannot change this declaration.
pub fn bind_timestamp(input: Type) -> Result<BoundExpression, BindError> {
    let scale = match input {
        Type::DateTime2(scale) | Type::DateTimeOffset(scale) => scale,
        Type::DateTime => Scale::new(3).expect("valid DATETIME scale"),
        Type::SmallDateTime => Scale::new(0).expect("valid SMALLDATETIME scale"),
        Type::Date => return Err(invalid_input("date")),
        Type::Int => return Err(invalid_input("int")),
        _ => return Err(BindError::Unsupported("uncaptured AT TIME ZONE input type")),
    };
    Ok(BoundExpression {
        result: Type::DateTimeOffset(scale),
        nullable: true,
    })
}

fn invalid_input(name: &str) -> BindError {
    BindError::Sql(SqlError::new(
        8116,
        1,
        format!("Argument data type {name} is invalid for argument 1 of AT TIME ZONE function."),
    ))
}
