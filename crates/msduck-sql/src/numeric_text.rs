//! Declaration-preserving implicit numeric text; no source coercion or metadata inference.
use msduck_core::{
    money::{self, MoneyType},
    types::Type,
    value::Decimal,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    Integer(i64),
    Decimal(Decimal),
    /// Exact coefficient in units of 0.0001.
    Money(i64),
    RealBits(u32),
    FloatBits(u64),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    UnsupportedSource,
    UnsupportedStyle,
    InvalidPayload,
    NonFinite,
}

/// Format an already typed value. NULL does not bypass declaration/style validation.
/// The source kind remains the caller's declaration; this returns no character metadata.
pub fn text(
    source: Type,
    value: Option<Value>,
    style: Option<i32>,
) -> Result<Option<String>, Error> {
    if style.is_some_and(|s| s != 0) {
        return Err(Error::UnsupportedStyle);
    }
    if !matches!(
        source,
        Type::Bit
            | Type::TinyInt
            | Type::SmallInt
            | Type::Int
            | Type::BigInt
            | Type::Decimal(_)
            | Type::Money
            | Type::SmallMoney
            | Type::Real
            | Type::Float
    ) {
        return Err(Error::UnsupportedSource);
    }
    let Some(value) = value else {
        return Ok(None);
    };
    let result = match (source, value) {
        (kind, Value::Integer(v))
            if matches!(
                kind,
                Type::Bit | Type::TinyInt | Type::SmallInt | Type::Int | Type::BigInt
            ) =>
        {
            let valid = match kind {
                Type::Bit => (0..=1).contains(&v),
                Type::TinyInt => u8::try_from(v).is_ok(),
                Type::SmallInt => i16::try_from(v).is_ok(),
                Type::Int => i32::try_from(v).is_ok(),
                Type::BigInt => true,
                _ => unreachable!(),
            };
            if !valid {
                return Err(Error::InvalidPayload);
            }
            v.to_string()
        }
        (Type::Decimal(kind), Value::Decimal(v))
            if kind.precision() == v.precision() && kind.scale() == v.scale() =>
        {
            v.to_string()
        }
        (kind @ (Type::Money | Type::SmallMoney), Value::Money(v)) => {
            let money_kind = if kind == Type::Money {
                MoneyType::Money
            } else {
                MoneyType::SmallMoney
            };
            money_kind
                .check_scaled(i128::from(v))
                .map_err(|_| Error::InvalidPayload)?;
            money::format(v, 0)
        }
        (Type::Real, Value::RealBits(bits)) => float(f64::from(f32::from_bits(bits)))?,
        (Type::Float, Value::FloatBits(bits)) => float(f64::from_bits(bits))?,
        _ => return Err(Error::InvalidPayload),
    };
    Ok(Some(result))
}

fn float(value: f64) -> Result<String, Error> {
    if !value.is_finite() {
        return Err(Error::NonFinite);
    }
    if value == 0.0 {
        return Ok(if value.is_sign_negative() { "-0" } else { "0" }.into());
    }
    // A guarded 17-significant-digit decimal conversion, then six-digit
    // decimal rounding, matches both retained IEEE grids. Scaling in binary
    // can push adjacent inputs across a half-digit boundary.
    let raw = format!("{:.16e}", value.abs());
    let (mantissa, raw_exponent) = raw.split_once('e').expect("scientific format");
    let mut exponent: i32 = raw_exponent.parse().expect("scientific exponent");
    let guarded = mantissa.replace('.', "");
    let mut coefficient = guarded[..6].parse::<u32>().expect("six decimal digits");
    if guarded.as_bytes()[6] >= b'5' {
        coefficient += 1;
    }
    if coefficient >= 1_000_000 {
        coefficient /= 10;
        exponent += 1;
    }
    let digits = format!("{coefficient:06}");
    let mantissa = format!("{}.{}", &digits[..1], &digits[1..]);
    let mut result = String::with_capacity(32);
    if value.is_sign_negative() {
        result.push('-');
    }
    if !(-4..6).contains(&exponent) {
        result.push_str(mantissa.trim_end_matches('0').trim_end_matches('.'));
        result.push('e');
        result.push(if exponent < 0 { '-' } else { '+' });
        result.push_str(&format!("{:03}", exponent.unsigned_abs()));
    } else {
        let digits = &digits;
        let point = exponent + 1;
        if point <= 0 {
            result.push_str("0.");
            result.push_str(&"0".repeat((-point) as usize));
            result.push_str(digits);
        } else {
            let point = point as usize;
            result.push_str(&digits[..point]);
            if point < digits.len() {
                result.push('.');
                result.push_str(&digits[point..]);
            }
        }
        if result.contains('.') {
            result.truncate(result.trim_end_matches('0').trim_end_matches('.').len());
        }
    }
    Ok(result)
}
