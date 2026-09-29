//! Percentile ordering declarations over explicit inputs; no value evaluation.
use msduck_core::{
    character::{Family, Length},
    collation::Label,
    diagnostic::SqlError,
    result::Properties,
    types::Type,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Continuous,
    Discrete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub data_type: Type,
    pub properties: Properties,
    /// Caller-resolved source collation, unchanged for DISC character results.
    pub collation: Option<Label>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindingError {
    Sql(SqlError),
    /// No captured eligibility rule; the adapter must not guess a result type.
    Unsupported(Type),
}

/// A missing source declaration remains unknown, including for CONT: the
/// adapter still has to determine whether the ordering expression is eligible.
/// Row values, emptiness and parameter bindings never participate in this rule.
pub fn declaration(
    kind: Kind,
    source: Option<Type>,
    collation: Option<&Label>,
) -> Result<Option<Declaration>, BindingError> {
    let Some(source) = source else {
        return Ok(None);
    };
    if matches!(source, Type::Text | Type::Ntext | Type::Image) {
        return Err(BindingError::Unsupported(source));
    }
    let numeric = matches!(
        source,
        Type::Bit
            | Type::TinyInt
            | Type::SmallInt
            | Type::Int
            | Type::BigInt
            | Type::Real
            | Type::Float
            | Type::Decimal(_)
            | Type::Money
            | Type::SmallMoney
    );
    let data_type = match kind {
        Kind::Continuous if numeric => Type::Float,
        Kind::Continuous => {
            let name = source_name(source);
            return Err(BindingError::Sql(SqlError::new(
                402,
                1,
                format!(
                    "The data types numeric and {name} are incompatible in the percentile_cont operator."
                ),
            )));
        }
        Kind::Discrete if source == Type::Xml => {
            return Err(BindingError::Sql(SqlError::new(
                305,
                1,
                "The XML data type cannot be compared or sorted, except when using the IS NULL operator.",
            )));
        }
        Kind::Discrete => source,
    };
    Ok(Some(Declaration {
        data_type,
        properties: Properties::expression(true),
        collation: if matches!(data_type, Type::Character(_)) {
            collation.cloned()
        } else {
            None
        },
    }))
}

fn source_name(source: Type) -> &'static str {
    match source {
        Type::Character(character) => match (character.family(), character.length()) {
            (Family::Varchar, Length::Max) => "varchar(max)",
            (Family::Nvarchar, Length::Max) => "nvarchar(max)",
            (Family::Char, _) => "char",
            (Family::Varchar, _) => "varchar",
            (Family::Nchar, _) => "nchar",
            (Family::Nvarchar, _) => "nvarchar",
        },
        Type::Binary(binary) if binary.fixed() => "binary",
        Type::Binary(_) => "varbinary",
        Type::Date => "date",
        Type::Time(_) => "time",
        Type::SmallDateTime => "smalldatetime",
        Type::DateTime => "datetime",
        Type::DateTime2(_) => "datetime2",
        Type::DateTimeOffset(_) => "datetimeoffset",
        Type::UniqueIdentifier => "uniqueidentifier",
        Type::Xml => "xml",
        Type::Variant => "sql_variant",
        // Reached only for rejected nonnumeric, nonlegacy ordering declarations.
        _ => unreachable!("eligible or explicitly unsupported type"),
    }
}
