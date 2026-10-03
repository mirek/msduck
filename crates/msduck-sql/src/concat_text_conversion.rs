//! Captured function-specific conversion contracts; not general CAST rules.
use msduck_core::{
    character::{Family, Length},
    encoding::decode_cp1252,
    types::Type,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Function {
    ConcatWs,
    Translate,
}

/// Explicit target domain chosen by the function adapter, not the current value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Domain {
    Cp1252,
    Utf16Le,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsupported {
    Declaration,
    Domain,
    Style,
    Format,
    InvalidBinaryValue,
    InputLimit,
    FunctionSource { number: u32 },
}

/// The source remains noncharacter when it was declared noncharacter.
/// Length is in target code units (ANSI bytes or UTF-16 units), not wire bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Contract {
    pub source: Type,
    pub function: Function,
    pub domain: Domain,
    pub allocation: Length,
}

fn captured_width(source: Type) -> Result<Length, Unsupported> {
    use Type::*;
    let width = match source {
        Bit => 1,
        TinyInt => 4,
        SmallInt => 6,
        Int => 12,
        BigInt => 24,
        Real | Float => 23,
        Money | SmallMoney | Date | DateTime | SmallDateTime | UniqueIdentifier => 40,
        Decimal(d)
            if matches!(
                (d.precision(), d.scale()),
                (1, 0) | (8, 2) | (18, 0) | (18, 4) | (38, 0) | (38, 18) | (38, 38)
            ) =>
        {
            41
        }
        Time(s) | DateTime2(s) | DateTimeOffset(s) if matches!(s.get(), 0 | 2 | 3 | 7) => 40,
        Binary(b) => {
            return match (b.fixed(), b.length()) {
                (_, Length::Bounded(2 | 10))
                | (false, Length::Bounded(8000))
                | (false, Length::Max) => Ok(b.length()),
                _ => Err(Unsupported::Declaration),
            };
        }
        Character(c) => {
            return match c.length() {
                Length::Bounded(10) | Length::Max => Ok(c.length()),
                _ => Err(Unsupported::Declaration),
            };
        }
        Text | Ntext => return Ok(Length::Max),
        Xml | Variant | Image => return Err(Unsupported::Declaration),
        _ => return Err(Unsupported::Declaration),
    };
    Ok(Length::Bounded(width))
}

/// Only default implicit conversions in the retained default collation domain.
/// `style: Some(_)` never silently becomes the default style.
pub fn contract(
    function: Function,
    source: Type,
    domain: Domain,
    style: Option<u32>,
) -> Result<Contract, Unsupported> {
    if style.is_some() {
        return Err(Unsupported::Style);
    }
    if domain == Domain::Unknown {
        return Err(Unsupported::Domain);
    }
    let rejection = match (function, source) {
        (Function::ConcatWs, Type::Xml | Type::Variant) => Some(257),
        (Function::ConcatWs, Type::Image) => Some(206),
        (
            Function::Translate,
            Type::Xml | Type::Variant | Type::Image | Type::Text | Type::Ntext,
        ) => Some(8116),
        _ => None,
    };
    if let Some(number) = rejection {
        return Err(Unsupported::FunctionSource { number });
    }
    let native_unicode = matches!(source, Type::Ntext)
        || matches!(source, Type::Character(c) if matches!(c.family(), Family::Nchar | Family::Nvarchar));
    if domain == Domain::Cp1252 && native_unicode {
        return Err(Unsupported::Domain);
    }
    let source_width = captured_width(source)?;
    let allocation = match function {
        Function::Translate => match source_width {
            Length::Max => Length::Max,
            _ => Length::Bounded(if domain == Domain::Utf16Le {
                4000
            } else {
                8000
            }),
        },
        Function::ConcatWs => match source_width {
            Length::Max => Length::Max,
            Length::Bounded(width) => Length::Bounded(
                if domain == Domain::Utf16Le && matches!(source, Type::Binary(_)) {
                    width.div_ceil(2)
                } else {
                    width
                },
            ),
        },
    };
    Ok(Contract {
        source,
        function,
        domain,
        allocation,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Text {
    Ansi(String),
    Unicode(Vec<u16>),
}

pub const INPUT_LIMIT: usize = 16 * 1024 * 1024;

/// Convert an already-stored binary value. Fixed BINARY padding belongs to its
/// storage/CAST adapter; a short fixed value is rejected rather than repaired.
/// NULL is preserved; skipping it for CONCAT_WS belongs to the function core.
pub fn binary_text(plan: Contract, value: Option<&[u8]>) -> Result<Option<Text>, Unsupported> {
    let Type::Binary(source) = plan.source else {
        return Err(Unsupported::Format);
    };
    // Public contracts can be copied/constructed: revalidate all declarations.
    let verified = contract(plan.function, plan.source, plan.domain, None)?;
    if plan != verified {
        return Err(Unsupported::Declaration);
    }
    let Some(bytes) = value else { return Ok(None) };
    if bytes.len() > INPUT_LIMIT {
        return Err(Unsupported::InputLimit);
    }
    if let Length::Bounded(width) = source.length()
        && (bytes.len() > usize::from(width)
            || (source.fixed() && bytes.len() != usize::from(width)))
    {
        return Err(Unsupported::InvalidBinaryValue);
    }
    Ok(Some(match plan.domain {
        Domain::Cp1252 => Text::Ansi(decode_cp1252(bytes)),
        Domain::Utf16Le => Text::Unicode(
            bytes
                .chunks(2)
                .map(|chunk| u16::from_le_bytes([chunk[0], *chunk.get(1).unwrap_or(&0)]))
                .collect(),
        ),
        Domain::Unknown => return Err(Unsupported::Domain),
    }))
}
