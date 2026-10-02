//! GREATEST/LEAST declarations and selection over already-converted operands.
//! Callers acquire declarations, evaluate/coerce every operand, and supply
//! collation comparison keys. No result declaration depends on operand values.
use msduck_core::{
    character::{CharacterType, Family, Length},
    collation::{Conflict, Label},
    diagnostic::SqlError,
    types::{BinaryType, DecimalType, Type},
};
use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Function {
    Greatest,
    Least,
}
impl Function {
    fn name(self) -> &'static str {
        match self {
            Self::Greatest => "greatest",
            Self::Least => "least",
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DecimalFamily {
    #[default]
    Decimal,
    Numeric,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Origin {
    #[default]
    Expression,
    Column,
    /// Precision contribution is lexical, not a current parameter value.
    IntegerLiteral(u8),
    UntypedNull,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Argument {
    pub data_type: Option<Type>,
    pub decimal_family: DecimalFamily,
    pub nullable: Option<bool>,
    pub collation: Option<Label>,
    pub origin: Origin,
}
impl Argument {
    pub fn typed(data_type: Type, nullable: Option<bool>) -> Self {
        Self {
            data_type: Some(data_type),
            decimal_family: DecimalFamily::Decimal,
            nullable,
            collation: None,
            origin: Origin::Expression,
        }
    }
    pub fn untyped_null() -> Self {
        Self {
            data_type: None,
            decimal_family: DecimalFamily::Decimal,
            nullable: Some(true),
            collation: None,
            origin: Origin::UntypedNull,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unsupported {
    UnknownDeclaration,
    InvalidLiteralDeclaration,
    UncapturedConversion { source: Type, target: Type },
    CollationDefaultChoice,
    UnknownCollation,
    ComparableShape,
    NonFiniteFloat,
    UncapturedVariantFamily,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Sql(SqlError),
    Unsupported(Unsupported),
}
impl From<SqlError> for Error {
    fn from(e: SqlError) -> Self {
        Self::Sql(e)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub data_type: Type,
    pub decimal_family: DecimalFamily,
    pub nullable: Option<bool>,
    pub collation: Option<Label>,
    pub computed: Option<bool>,
}
impl Declaration {
    pub fn ast_type(&self) -> sqlparser::ast::DataType {
        if let Type::Decimal(d) = self.data_type
            && self.decimal_family == DecimalFamily::Numeric
        {
            return sqlparser::ast::DataType::Numeric(
                sqlparser::ast::ExactNumberInfo::PrecisionAndScale(
                    d.precision().into(),
                    d.scale().into(),
                ),
            );
        }
        crate::sql_type::ast(self.data_type)
    }
    /// Case sensitivity is only known here for the three retained collations.
    /// An unfamiliar catalog name is retained, not assigned fabricated flags.
    pub fn flags(&self) -> Option<u16> {
        let nullable = self.nullable?;
        let sensitive = match self.collation.as_ref().and_then(Label::name) {
            None if !matches!(self.data_type, Type::Character(_)) => false,
            Some(n) if n.eq_ignore_ascii_case("SQL_Latin1_General_CP1_CI_AS") => false,
            Some(n)
                if n.eq_ignore_ascii_case("Latin1_General_BIN")
                    || n.eq_ignore_ascii_case("Latin1_General_CS_AS") =>
            {
                true
            }
            _ => return None,
        };
        Some(u16::from(nullable) | (u16::from(sensitive) << 1) | (u16::from(self.computed?) << 5))
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub function: Function,
    pub declaration: Declaration,
    arity: usize,
}

/// Compile-time inputs are declarations and provenance only. NULL syntax is
/// explicitly different from an unknown declaration or a nullable parameter.
pub fn plan(function: Function, arguments: &[Argument]) -> Result<Plan, Error> {
    if !(1..=254).contains(&arguments.len()) {
        return Err(SqlError::syntax(
            189,
            1,
            format!(
                "The {} function requires 1 to 254 arguments.",
                function.name()
            ),
        )
        .into());
    }
    let mut known = Vec::new();
    for (i, arg) in arguments.iter().enumerate() {
        if arg.origin == Origin::UntypedNull {
            if arg.data_type.is_some() || arg.nullable != Some(true) {
                return Err(Error::Unsupported(Unsupported::InvalidLiteralDeclaration));
            }
            continue;
        }
        let kind = arg
            .data_type
            .ok_or(Error::Unsupported(Unsupported::UnknownDeclaration))?;
        if let Origin::IntegerLiteral(digits) = arg.origin
            && (kind != Type::Int || !(1..=10).contains(&digits))
        {
            return Err(Error::Unsupported(Unsupported::InvalidLiteralDeclaration));
        }
        if matches!(kind, Type::Text | Type::Ntext | Type::Image | Type::Xml) {
            return Err(SqlError::new(
                8116,
                4,
                format!(
                    "Argument data type {} is invalid for argument {} of {} function.",
                    name(kind),
                    i + 1,
                    function.name()
                ),
            )
            .into());
        }
        known.push((arg, kind));
    }
    let winner = known
        .iter()
        .max_by_key(|(_, kind)| precedence(*kind))
        .map_or(Type::Int, |(_, kind)| *kind);
    for (_, source) in &known {
        compatible(*source, winner)?;
    }
    let (kind, decimal_family) = match winner {
        Type::Decimal(_) => {
            let mut integral = 0;
            let mut scale = 0;
            for (arg, kind) in &known {
                if let Some((p, s)) = decimal_shape(*kind, arg.origin) {
                    integral = integral.max(p - s);
                    scale = scale.max(s);
                }
            }
            let precision = (integral + scale).min(38);
            let scale = scale.min(38 - integral);
            let family = known
                .iter()
                .find(|(_, t)| matches!(t, Type::Decimal(_)))
                .map_or(DecimalFamily::Decimal, |(a, _)| a.decimal_family);
            (
                Type::Decimal(DecimalType::new(precision, scale).expect("bounded decimal shape")),
                family,
            )
        }
        Type::Character(common) => {
            let unicode = matches!(common.family(), Family::Nchar | Family::Nvarchar);
            let maximum = if unicode { 4000 } else { 8000 };
            let width = known
                .iter()
                .filter_map(|(_, t)| {
                    if let Type::Character(c) = t {
                        Some(match c.length() {
                            Length::Max => maximum,
                            Length::Bounded(n) => n.min(maximum),
                        })
                    } else {
                        None
                    }
                })
                .max()
                .expect("winning character declaration");
            (
                Type::Character(
                    CharacterType::new(common.family(), Length::Bounded(width))
                        .expect("bounded character shape"),
                ),
                DecimalFamily::Decimal,
            )
        }
        Type::Binary(b) => {
            let width = known
                .iter()
                .filter_map(|(_, t)| {
                    if let Type::Binary(b) = t {
                        Some(match b.length() {
                            Length::Max => 8000,
                            Length::Bounded(n) => n,
                        })
                    } else {
                        None
                    }
                })
                .max()
                .unwrap_or(8000);
            (
                Type::Binary(
                    BinaryType::new(b.fixed(), Length::Bounded(width))
                        .expect("bounded binary shape"),
                ),
                DecimalFamily::Decimal,
            )
        }
        Type::Time(_) | Type::DateTime2(_) | Type::DateTimeOffset(_) => {
            let scale = known
                .iter()
                .filter_map(|(_, t)| match t {
                    Type::Time(s) | Type::DateTime2(s) | Type::DateTimeOffset(s) => Some(s.get()),
                    Type::DateTime => Some(3),
                    _ => None,
                })
                .max()
                .unwrap_or(7);
            let scale = msduck_core::types::Scale::new(scale).expect("validated scale");
            (
                match winner {
                    Type::Time(_) => Type::Time(scale),
                    Type::DateTime2(_) => Type::DateTime2(scale),
                    _ => Type::DateTimeOffset(scale),
                },
                DecimalFamily::Decimal,
            )
        }
        _ => (winner, DecimalFamily::Decimal),
    };
    let collation = if matches!(kind, Type::Character(_)) {
        resolve_collation(arguments)?
    } else {
        None
    };
    let converting_text = known.iter().any(|(_, t)| matches!(t, Type::Character(_)))
        && !matches!(kind, Type::Character(_) | Type::Variant);
    let nullable = if converting_text || arguments.iter().any(|a| a.nullable == Some(true)) {
        Some(true)
    } else if arguments.iter().all(|a| a.nullable == Some(false)) {
        Some(false)
    } else {
        None
    };
    let computed = if nullable == Some(false)
        || !arguments.iter().any(|a| a.origin == Origin::Column)
        || matches!(kind, Type::Character(_))
    {
        Some(true)
    } else if kind == Type::Int && known.iter().all(|(_, t)| *t == Type::Int) {
        Some(false)
    } else {
        None
    };
    Ok(Plan {
        function,
        declaration: Declaration {
            data_type: kind,
            decimal_family,
            nullable,
            collation,
            computed,
        },
        arity: arguments.len(),
    })
}
fn decimal_shape(kind: Type, origin: Origin) -> Option<(u8, u8)> {
    Some(match kind {
        Type::Decimal(d) => (d.precision(), d.scale()),
        Type::Money => (19, 4),
        Type::SmallMoney => (10, 4),
        Type::Int => (
            if let Origin::IntegerLiteral(n) = origin {
                n
            } else {
                10
            },
            0,
        ),
        Type::BigInt => (19, 0),
        Type::SmallInt => (5, 0),
        Type::TinyInt => (3, 0),
        Type::Bit => (1, 0),
        _ => return None,
    })
}
fn precedence(kind: Type) -> u8 {
    match kind {
        Type::Variant => 30,
        Type::DateTimeOffset(_) => 29,
        Type::DateTime2(_) => 28,
        Type::DateTime => 27,
        Type::SmallDateTime => 26,
        Type::Date => 25,
        Type::Time(_) => 24,
        Type::Float => 23,
        Type::Real => 22,
        Type::Decimal(_) => 21,
        Type::Money => 20,
        Type::SmallMoney => 19,
        Type::BigInt => 18,
        Type::Int => 17,
        Type::SmallInt => 16,
        Type::TinyInt => 15,
        Type::Bit => 14,
        Type::UniqueIdentifier => 13,
        Type::Character(c) => match c.family() {
            Family::Nvarchar => 12,
            Family::Nchar => 11,
            Family::Varchar => 10,
            Family::Char => 9,
        },
        Type::Binary(b) => {
            if b.fixed() {
                7
            } else {
                8
            }
        }
        _ => 0,
    }
}
fn numeric(t: Type) -> bool {
    matches!(
        t,
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
    )
}
fn temporal(t: Type) -> bool {
    matches!(
        t,
        Type::Date
            | Type::Time(_)
            | Type::DateTime
            | Type::SmallDateTime
            | Type::DateTime2(_)
            | Type::DateTimeOffset(_)
    )
}
fn compatible(source: Type, target: Type) -> Result<(), Error> {
    if source == target
        || (target == Type::Variant && source == Type::Int)
        || (numeric(source) && numeric(target))
        || (matches!(source, Type::Character(_))
            && matches!(
                target,
                Type::Character(_)
                    | Type::Int
                    | Type::Decimal(_)
                    | Type::Float
                    | Type::Date
                    | Type::UniqueIdentifier
            ))
        || (matches!(source, Type::Binary(_))
            && (matches!(target, Type::Binary(_)) || target == Type::Int))
    {
        return Ok(());
    }
    if target == Type::Date && (source == Type::Int || matches!(source, Type::Time(_))) {
        return Err(SqlError::new(
            206,
            2,
            format!(
                "Operand type clash: {} is incompatible with date",
                name(source)
            ),
        )
        .into());
    }
    if matches!(
        (source, target),
        (Type::Date, Type::DateTime | Type::DateTime2(_))
            | (Type::SmallDateTime, Type::DateTime)
            | (Type::DateTime, Type::DateTime2(_))
            | (Type::DateTime2(_), Type::DateTimeOffset(_))
            | (Type::Time(_), Type::DateTime)
            | (Type::Time(_), Type::Time(_))
            | (Type::DateTime2(_), Type::DateTime2(_))
            | (Type::DateTimeOffset(_), Type::DateTimeOffset(_))
            | (Type::Int, Type::DateTime)
    ) {
        return Ok(());
    }
    Err(Error::Unsupported(Unsupported::UncapturedConversion {
        source,
        target,
    }))
}
fn resolve_collation(arguments: &[Argument]) -> Result<Option<Label>, Error> {
    let mut result: Option<Label> = None;
    for label in arguments
        .iter()
        .filter(|a| matches!(a.data_type, Some(Type::Character(_))))
        .filter_map(|a| a.collation.as_ref())
    {
        result = Some(match result {
            None => label.clone(),
            Some(old) => match old.combine(label) {
                Ok(label) => label,
                Err(Conflict::Explicit { left, right }) => {
                    return Err(collation_error(&left, &right));
                }
                _ => return Err(Error::Unsupported(Unsupported::CollationDefaultChoice)),
            },
        });
    }
    if let Some(Label::NoCollation { left, right }) = &result {
        return Err(collation_error(left, right));
    }
    Ok(result)
}
fn collation_error(left: &str, right: &str) -> Error {
    SqlError::new(468, 9, format!("Cannot resolve the collation conflict between \"{right}\" and \"{left}\" in the GREATEST/LEAST operation.")).into()
}
fn name(t: Type) -> &'static str {
    match t {
        Type::Text => "text",
        Type::Ntext => "ntext",
        Type::Image => "image",
        Type::Xml => "xml",
        Type::Int => "int",
        Type::BigInt => "bigint",
        Type::SmallInt => "smallint",
        Type::TinyInt => "tinyint",
        Type::Bit => "bit",
        Type::Date => "date",
        Type::Time(_) => "time",
        Type::Decimal(_) => "numeric",
        Type::Character(c) => match c.family() {
            Family::Char => "char",
            Family::Varchar => "varchar",
            Family::Nchar => "nchar",
            Family::Nvarchar => "nvarchar",
        },
        _ => "unsupported",
    }
}

/// Coercion failures remain explicit inputs: this function does not infer
/// declaration metadata from a value or catch/rewrite arbitrary backend text.
pub fn conversion_failure(
    source: Type,
    target: Type,
    value: &str,
) -> Result<SqlError, Unsupported> {
    match (source, target) {
        (Type::Character(c), Type::Int) if c.family() == Family::Varchar => Ok(SqlError::new(
            245,
            1,
            format!(
                "Conversion failed when converting the varchar value '{value}' to data type int."
            ),
        )),
        (Type::Character(c), Type::Decimal(_)) if c.family() == Family::Varchar => Ok(
            SqlError::new(8114, 5, "Error converting data type varchar to numeric."),
        ),
        _ => Err(Unsupported::UncapturedConversion { source, target }),
    }
}

/// Values must already have the plan's common declaration. Character sort keys
/// are supplied explicitly by the collation adapter, not guessed from UTF-16.
#[derive(Clone, Debug, PartialEq)]
pub enum Comparable {
    Null,
    Exact {
        coefficient: i128,
        scale: u8,
    },
    Float(f64),
    Character {
        units: Vec<u16>,
        sort_key: Vec<u32>,
        collation: String,
    },
    Binary(Vec<u8>),
    Guid([u8; 16]),
    /// Common exact tick unit; offset payload remains with the selected index.
    Temporal(i128),
    Variant {
        base_type: Type,
        value: Box<Comparable>,
    },
}
impl Plan {
    /// Validate all supplied outcomes before selection, including errors in
    /// later arguments and oversized values that would not win. Return the
    /// first tied index, or None only when all operands are SQL NULL.
    pub fn select(&self, values: &[Result<Comparable, SqlError>]) -> Result<Option<usize>, Error> {
        if values.len() != self.arity {
            return Err(Error::Unsupported(Unsupported::ComparableShape));
        }
        for value in values {
            let value = value.as_ref().map_err(|e| Error::Sql(e.clone()))?;
            self.validate(value, self.declaration.data_type)?;
        }
        let mut winner: Option<usize> = None;
        for (index, value) in values.iter().enumerate() {
            let value = value.as_ref().expect("all outcomes checked");
            if matches!(value, Comparable::Null) {
                continue;
            }
            if let Some(old) = winner {
                let order = compare(value, values[old].as_ref().expect("all outcomes checked"))?;
                if (self.function == Function::Greatest && order == Ordering::Greater)
                    || (self.function == Function::Least && order == Ordering::Less)
                {
                    winner = Some(index);
                }
            } else {
                winner = Some(index);
            }
        }
        Ok(winner)
    }
    fn validate(&self, value: &Comparable, kind: Type) -> Result<(), Error> {
        if matches!(value, Comparable::Null) {
            return Ok(());
        }
        let shape = match (kind, value) {
            (kind, Comparable::Exact { coefficient, scale }) if numeric(kind) => match kind {
                Type::Decimal(d) => {
                    *scale == d.scale()
                        && coefficient.unsigned_abs() < 10u128.pow(d.precision().into())
                }
                Type::Money => *scale == 4 && i64::try_from(*coefficient).is_ok(),
                Type::SmallMoney => *scale == 4 && i32::try_from(*coefficient).is_ok(),
                Type::Int => *scale == 0 && i32::try_from(*coefficient).is_ok(),
                Type::BigInt => *scale == 0 && i64::try_from(*coefficient).is_ok(),
                Type::SmallInt => *scale == 0 && i16::try_from(*coefficient).is_ok(),
                Type::TinyInt => *scale == 0 && u8::try_from(*coefficient).is_ok(),
                Type::Bit => *scale == 0 && matches!(*coefficient, 0 | 1),
                _ => false,
            },
            (Type::Real | Type::Float, Comparable::Float(n)) => {
                if !n.is_finite() {
                    return Err(Error::Unsupported(Unsupported::NonFiniteFloat));
                }
                kind != Type::Real || f64::from(*n as f32) == *n
            }
            (
                Type::Character(c),
                Comparable::Character {
                    units, collation, ..
                },
            ) => {
                let Length::Bounded(max) = c.length() else {
                    return Err(Error::Unsupported(Unsupported::ComparableShape));
                };
                if units.len() > usize::from(max) {
                    return Err(truncation().into());
                }
                if !self
                    .declaration
                    .collation
                    .as_ref()
                    .and_then(Label::name)
                    .is_some_and(|n| n.eq_ignore_ascii_case(collation))
                {
                    return Err(Error::Unsupported(Unsupported::UnknownCollation));
                }
                !matches!(c.family(), Family::Char | Family::Nchar)
                    || units.len() == usize::from(max)
            }
            (Type::Binary(b), Comparable::Binary(bytes)) => {
                let Length::Bounded(max) = b.length() else {
                    return Err(Error::Unsupported(Unsupported::ComparableShape));
                };
                if bytes.len() > usize::from(max) {
                    return Err(truncation().into());
                }
                !b.fixed() || bytes.len() == usize::from(max)
            }
            (Type::UniqueIdentifier, Comparable::Guid(_)) => true,
            (t, Comparable::Temporal(_)) if temporal(t) => true,
            (Type::Variant, Comparable::Variant { base_type, value }) => {
                if !matches!(base_type, Type::Int | Type::Decimal(_) | Type::Character(_))
                    || matches!(
                        value.as_ref(),
                        Comparable::Null | Comparable::Variant { .. }
                    )
                {
                    return Err(Error::Unsupported(Unsupported::UncapturedVariantFamily));
                }
                if let (Type::Character(c), Comparable::Character { units, .. }) =
                    (base_type, value.as_ref())
                {
                    matches!(c.length(), Length::Bounded(max) if units.len() <= usize::from(max))
                } else {
                    self.validate(value, *base_type)?;
                    true
                }
            }
            _ => false,
        };
        if shape {
            Ok(())
        } else {
            Err(Error::Unsupported(Unsupported::ComparableShape))
        }
    }
}
fn truncation() -> SqlError {
    SqlError::new(8152, 10, "String or binary data would be truncated.")
}
fn compare(left: &Comparable, right: &Comparable) -> Result<Ordering, Error> {
    use Comparable::*;
    Ok(match (left, right) {
        (
            Exact {
                coefficient: a,
                scale: sa,
            },
            Exact {
                coefficient: b,
                scale: sb,
            },
        ) => compare_exact(*a, *sa, *b, *sb),
        (Float(a), Float(b)) => a
            .partial_cmp(b)
            .ok_or(Error::Unsupported(Unsupported::NonFiniteFloat))?,
        (
            Character {
                sort_key: a,
                collation: ca,
                ..
            },
            Character {
                sort_key: b,
                collation: cb,
                ..
            },
        ) => {
            if !ca.eq_ignore_ascii_case(cb) {
                return Err(Error::Unsupported(Unsupported::UnknownCollation));
            }
            a.cmp(b)
        }
        (Binary(a), Binary(b)) => a.cmp(b),
        (Guid(a), Guid(b)) => msduck_core::types::uniqueidentifier::compare(a, b),
        (Temporal(a), Temporal(b)) => a.cmp(b),
        (
            Variant {
                base_type: ta,
                value: a,
            },
            Variant {
                base_type: tb,
                value: b,
            },
        ) => {
            let rank = |t| {
                if matches!(t, Type::Character(_)) {
                    0
                } else {
                    1
                }
            };
            let ranks = rank(*ta).cmp(&rank(*tb));
            if ranks != Ordering::Equal {
                ranks
            } else {
                compare(a, b)?
            }
        }
        _ => return Err(Error::Unsupported(Unsupported::ComparableShape)),
    })
}
fn compare_exact(a: i128, sa: u8, b: i128, sb: u8) -> Ordering {
    let signs = a.signum().cmp(&b.signum());
    if signs != Ordering::Equal {
        return signs;
    }
    if a == 0 && b == 0 {
        return Ordering::Equal;
    }
    let scale = sa.max(sb);
    let mut left = a.unsigned_abs().to_string();
    let mut right = b.unsigned_abs().to_string();
    left.extend(std::iter::repeat_n('0', usize::from(scale - sa)));
    right.extend(std::iter::repeat_n('0', usize::from(scale - sb)));
    let order = left.len().cmp(&right.len()).then_with(|| left.cmp(&right));
    if a < 0 { order.reverse() } else { order }
}
