//! Exact DECIMAL aggregate arithmetic, independent of native vectors and SQL ASTs.
use crate::{bounded_aggregate::Overflow, types::DecimalType};

const LIMIT: i128 = 10i128.pow(38);

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct State {
    sum: i128,
    count: u64,
    failed: bool,
}

impl State {
    /// Add one non-NULL coefficient at the aggregate's input scale.
    pub fn push(&mut self, coefficient: i128) {
        self.combine(Self {
            sum: coefficient,
            count: 1,
            failed: coefficient <= -LIMIT || coefficient >= LIMIT,
        });
    }

    /// Merge partial states without losing an earlier overflow.
    pub fn combine(&mut self, other: Self) {
        let next = self
            .sum
            .checked_add(other.sum)
            .zip(self.count.checked_add(other.count));
        if !self.failed
            && !other.failed
            && let Some((sum, count)) = next
            && sum > -LIMIT
            && sum < LIMIT
        {
            self.sum = sum;
            self.count = count;
        } else {
            self.failed = true;
        }
    }

    /// Return a coefficient at max(input scale, 6), truncating toward zero.
    pub fn average(self, input: DecimalType) -> Result<Option<i128>, Overflow> {
        if self.failed {
            return Err(Overflow);
        }
        if self.count == 0 {
            return Ok(None);
        }
        let count = i128::from(self.count);
        let factor = 10i128.pow(u32::from(6u8.saturating_sub(input.scale())));
        // Divide first: multiplying the entire sum can overflow i128 even
        // when the average fits DECIMAL(38, result_scale). The remainder's
        // magnitude is below a u64 count, so its scaled product always fits.
        let whole = (self.sum / count).checked_mul(factor).ok_or(Overflow)?;
        let fraction = (self.sum % count) * factor / count;
        let value = whole.checked_add(fraction).ok_or(Overflow)?;
        if value <= -LIMIT || value >= LIMIT {
            return Err(Overflow);
        }
        Ok(Some(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn average(scale: u8, values: &[i128]) -> Result<Option<i128>, Overflow> {
        let mut state = State::default();
        for value in values {
            state.push(*value);
        }
        state.average(DecimalType::new(38, scale).unwrap())
    }

    #[test]
    fn reference_truncation_scale_and_empty_inputs() {
        for scale in [0, 2, 6, 7, 38] {
            let unit = if scale == 38 {
                1
            } else {
                10i128.pow(scale.into())
            };
            let factor = 10i128.pow(u32::from(6u8.saturating_sub(scale)));
            for sign in [-1, 1] {
                assert_eq!(
                    average(scale, &[sign * unit, 0, 0]),
                    Ok(Some(sign * (unit * factor / 3)))
                );
                assert_eq!(
                    average(scale, &[sign * unit, sign * unit, 0]),
                    Ok(Some(sign * (2 * unit * factor / 3)))
                );
            }
            assert_eq!(average(scale, &[]), Ok(None));
        }
    }

    #[test]
    fn reference_sum_overflow_and_result_scale_overflow_are_distinct() {
        let maximum_whole = 10i128.pow(32) - 1;
        // The scaled sum cannot fit i128, but division before scaling is exact.
        assert_eq!(
            average(0, &[maximum_whole; 10]),
            Ok(Some(maximum_whole * 1_000_000))
        );
        assert_eq!(average(0, &[10i128.pow(33) - 1]), Err(Overflow));
        assert_eq!(average(6, &[6 * 10i128.pow(37); 2]), Err(Overflow));
        assert_eq!(average(6, &[LIMIT - 1, 1, -1]), Err(Overflow));
        assert_eq!(average(6, &[-LIMIT + 1, -1, 1]), Err(Overflow));
    }

    #[test]
    fn partial_states_preserve_overflow_and_empty_groups() {
        let mut state = State::default();
        state.push(LIMIT - 1);
        let mut other = State::default();
        other.push(1);
        state.combine(other);
        state.push(-1);
        assert_eq!(
            state.average(DecimalType::new(38, 6).unwrap()),
            Err(Overflow)
        );
        let mut empty = State::default();
        empty.combine(state);
        assert_eq!(
            empty.average(DecimalType::new(38, 6).unwrap()),
            Err(Overflow)
        );
    }
}
