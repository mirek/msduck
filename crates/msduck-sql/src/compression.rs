//! Deterministic SQL type rules for COMPRESS and DECOMPRESS.
//!
//! This module is intentionally independent of DuckDB and does not inspect
//! argument values. The caller supplies bound declarations from its catalog.

use msduck_core::{
    character::{Family, Length},
    diagnostic::SqlError,
    types::{BinaryType, Type},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Function {
    Compress,
    Decompress,
}

impl Function {
    const fn display(self) -> &'static str {
        match self {
            Self::Compress => "Compress",
            Self::Decompress => "Decompress",
        }
    }
}

/// `UntypedNull` is the bare SQL NULL literal, unlike a typed NULL parameter.
/// JSON and TIMESTAMP need distinct identities not represented in core `Type`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Argument {
    UntypedNull,
    Known(Type),
    NumericLiteral,
    Timestamp,
    Json,
    Uncaptured,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BindError {
    Sql(SqlError),
    Unsupported(&'static str),
}

/// Compile-time properties captured for both functions. Result typing is
/// independent of the input declaration and value, including typed NULLs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BoundFunction {
    pub result: Type,
    pub nullable: bool,
    pub computed_column_deterministic: bool,
    pub computed_column_precise: bool,
}

fn character_name(family: Family, length: Length) -> &'static str {
    match (family, length) {
        (Family::Char, _) => "char",
        (Family::Nchar, _) => "nchar",
        (Family::Varchar, Length::Max) => "varchar(max)",
        (Family::Nvarchar, Length::Max) => "nvarchar(max)",
        (Family::Varchar, _) => "varchar",
        (Family::Nvarchar, _) => "nvarchar",
    }
}

/// Bind one captured argument declaration. Unknown families remain unsupported
/// until their SQL Server error text and behavior have been captured.
pub fn bind(function: Function, args: &[Argument]) -> Result<BoundFunction, BindError> {
    let [argument] = args else {
        return Err(BindError::Sql(SqlError::syntax(
            174,
            1,
            format!(
                "The {} function requires 1 argument(s).",
                function.display()
            ),
        )));
    };
    let rejected = match (function, argument) {
        (_, Argument::UntypedNull | Argument::Known(Type::Binary(_)))
        | (Function::Compress, Argument::Known(Type::Character(_))) => None,
        (Function::Decompress, Argument::Known(Type::Character(kind)))
            if matches!(kind.family(), Family::Varchar | Family::Nvarchar) =>
        {
            Some(character_name(kind.family(), kind.length()))
        }
        (_, Argument::Known(Type::Int)) => Some("int"),
        (Function::Compress, Argument::Known(Type::Bit)) => Some("bit"),
        (Function::Compress, Argument::Known(Type::Float)) => Some("float"),
        (Function::Compress, Argument::Known(Type::DateTime)) => Some("datetime"),
        (Function::Compress, Argument::Known(Type::Date)) => Some("date"),
        (Function::Compress, Argument::Known(Type::UniqueIdentifier)) => Some("uniqueidentifier"),
        (Function::Compress, Argument::Known(Type::Xml)) => Some("xml"),
        (Function::Compress, Argument::Known(Type::Text)) => Some("text"),
        (Function::Compress, Argument::Known(Type::Ntext)) => Some("ntext"),
        (Function::Compress, Argument::Known(Type::Image)) => Some("image"),
        (Function::Compress, Argument::Known(Type::Variant)) => Some("sql_variant"),
        (Function::Compress, Argument::NumericLiteral) => Some("numeric"),
        (Function::Compress, Argument::Timestamp) => Some("timestamp"),
        (Function::Compress, Argument::Json) => Some("json"),
        _ => {
            return Err(BindError::Unsupported(
                "uncaptured compression argument type",
            ));
        }
    };
    if let Some(name) = rejected {
        return Err(BindError::Sql(SqlError::new(
            8116,
            1,
            format!(
                "Argument data type {name} is invalid for argument 1 of {} function.",
                function.display()
            ),
        )));
    }
    Ok(BoundFunction {
        result: Type::Binary(BinaryType::new(false, Length::Max).expect("valid VARBINARY(MAX)")),
        nullable: true,
        computed_column_deterministic: false,
        computed_column_precise: true,
    })
}
