//! Exact decimal arithmetic over explicitly declared inputs and outputs.
use crate::{types::DecimalType, value::Decimal};
use num_bigint::BigUint;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    DivideByZero,
    Overflow,
}

/// Divide coefficients without binary floating point, truncating toward zero
/// at the caller's declared result scale. SQL type inference chooses that
/// declaration; this routine does not inspect ASTs or session settings.
pub fn divide(left: Decimal, right: Decimal, result: DecimalType) -> Result<Decimal, Error> {
    if right.coefficient() == 0 {
        return Err(Error::DivideByZero);
    }
    let mut numerator = BigUint::from(left.coefficient().unsigned_abs());
    let mut denominator = BigUint::from(right.coefficient().unsigned_abs());
    let exponent = i16::from(result.scale()) + i16::from(right.scale()) - i16::from(left.scale());
    // Validated declarations bound the exponent to -38..=76 and the largest
    // intermediate to 114 decimal digits. No unbounded input controls work.
    let factor = BigUint::from(10u8).pow(u32::from(exponent.unsigned_abs()));
    if exponent >= 0 {
        numerator *= factor;
    } else {
        denominator *= factor;
    }
    let quotient = numerator / denominator;
    let magnitude = i128::try_from(quotient).map_err(|_| Error::Overflow)?;
    let coefficient = if (left.coefficient() < 0) != (right.coefficient() < 0) {
        -magnitude
    } else {
        magnitude
    };
    Decimal::new(result.precision(), result.scale(), coefficient).map_err(|_| Error::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn d(p: u8, s: u8, coefficient: i128) -> Decimal {
        Decimal::new(p, s, coefficient).unwrap()
    }
    #[test]
    fn reference_quotients_truncate_at_declared_scale_in_both_directions() {
        for (p, s, rp, rs) in [
            (5, 2, 13, 8),
            (38, 6, 38, 6),
            (38, 0, 38, 6),
            (38, 20, 38, 6),
        ] {
            let unit = 10i128.pow(s.into());
            for sign in [-1, 1] {
                let result = divide(
                    d(p, s, sign * 2 * unit),
                    d(p, s, 3 * unit),
                    DecimalType::new(rp, rs).unwrap(),
                )
                .unwrap();
                assert_eq!(result.coefficient(), sign * (2 * 10i128.pow(rs.into()) / 3));
                let negative_divisor = divide(
                    d(p, s, sign * 2 * unit),
                    d(p, s, -3 * unit),
                    DecimalType::new(rp, rs).unwrap(),
                )
                .unwrap();
                assert_eq!(negative_divisor.coefficient(), -result.coefficient());
            }
        }
        let fraction = divide(
            d(38, 38, 10i128.pow(37)),
            d(38, 38, 3 * 10i128.pow(37)),
            DecimalType::new(38, 6).unwrap(),
        )
        .unwrap();
        assert_eq!(fraction.to_string(), "0.333333");
    }

    #[test]
    fn large_intermediates_and_result_overflow_are_distinct() {
        let maximum = 10i128.pow(38) - 1;
        let result = DecimalType::new(38, 6).unwrap();
        assert_eq!(
            divide(d(38, 0, maximum), d(38, 0, maximum), result)
                .unwrap()
                .to_string(),
            "1.000000"
        );
        assert_eq!(
            divide(d(38, 0, maximum), d(1, 0, 1), result),
            Err(Error::Overflow)
        );
        // Both ends of the scale-exponent range, including a tiny quotient.
        assert_eq!(
            divide(
                d(38, 38, 1),
                d(38, 0, maximum),
                DecimalType::new(38, 0).unwrap()
            )
            .unwrap()
            .coefficient(),
            0
        );
        assert_eq!(
            divide(d(1, 0, 1), d(38, 38, 1), DecimalType::new(38, 38).unwrap()),
            Err(Error::Overflow)
        );
        for coefficient in [0, 1, -1] {
            assert_eq!(
                divide(d(5, 2, coefficient), d(5, 2, 0), result),
                Err(Error::DivideByZero)
            );
        }
    }
}
