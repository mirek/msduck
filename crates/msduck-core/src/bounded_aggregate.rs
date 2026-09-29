//! Bounded aggregate states shared by backend adapters.
//!
//! Integer/currency sums are exact. Floating statistical transitions deliberately
//! retain binary64 arithmetic order observed in pinned SQL Server captures.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct State {
    pub sum: i64,
    pub count: u64,
    pub failed: bool,
}

pub fn add<const BIG: bool>(state: State, sum: i64, count: u64) -> Option<State> {
    if state.failed {
        return None;
    }
    let sum = state.sum.checked_add(sum)?;
    if !BIG && i32::try_from(sum).is_err() {
        return None;
    }
    Some(State {
        sum,
        count: state.count.checked_add(count)?,
        failed: false,
    })
}

impl State {
    /// Truncate an exact average toward zero, preserving empty-set NULL.
    pub fn value(self, average: bool) -> Result<Option<i64>, Overflow> {
        if self.failed {
            return Err(Overflow);
        }
        Ok((self.count != 0).then(|| {
            if average {
                (i128::from(self.sum) / i128::from(self.count)) as i64
            } else {
                self.sum
            }
        }))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Overflow;

/// The four SQL Server statistical result families. All return FLOAT(53).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Statistic {
    Stdev,
    Stdevp,
    Var,
    Varp,
}

/// One sequential binary64 statistical transition state.
///
/// The caller converts each non-NULL input once and handles typed DISTINCT
/// before calling `push`. This state intentionally does not define a parallel
/// combine rule: binary64 accumulation is order-sensitive.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FloatStatsState {
    pub count: u64,
    pub sum: f64,
    pub squares: f64,
    pub failed: bool,
}

impl FloatStatsState {
    /// Add one already converted, non-NULL input exactly once.
    pub fn push(&mut self, value: f64) {
        if self.failed {
            return;
        }
        let Some(count) = self.count.checked_add(1) else {
            self.failed = true;
            return;
        };
        let sum = self.sum + value;
        let squares = self.squares + value * value;
        if !value.is_finite() || !sum.is_finite() || !squares.is_finite() {
            self.failed = true;
            return;
        }
        self.count = count;
        self.sum = sum;
        self.squares = squares;
    }

    /// Return the SQL Server sample/population shape or a numerical overflow.
    pub fn value(self, statistic: Statistic) -> Result<Option<f64>, Overflow> {
        if self.failed {
            return Err(Overflow);
        }
        if self.count == 0
            || (self.count == 1 && matches!(statistic, Statistic::Stdev | Statistic::Var))
        {
            return Ok(None);
        }
        let count = self.count as f64;
        let raw = self.squares - self.sum * self.sum / count;
        if !raw.is_finite() {
            return Err(Overflow);
        }
        // A negative cancellation residue and -0 both yield positive zero.
        let numerator = if raw <= 0.0 { 0.0 } else { raw };
        let denominator = if matches!(statistic, Statistic::Stdev | Statistic::Var) {
            count - 1.0
        } else {
            count
        };
        let variance = numerator / denominator;
        let result = if matches!(statistic, Statistic::Stdev | Statistic::Stdevp) {
            variance.sqrt()
        } else {
            variance
        };
        result
            .is_finite()
            .then_some(result)
            .ok_or(Overflow)
            .map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_averages_empty_groups_and_sticky_failure() {
        assert_eq!(State::default().value(true), Ok(None));
        for (sum, count, expected) in [
            (3, 2, 1),
            (-3, 2, -1),
            (i64::MIN, 1, i64::MIN),
            (i64::MAX, u64::MAX, 0),
        ] {
            let state = State {
                sum,
                count,
                failed: false,
            };
            assert_eq!(state.value(true), Ok(Some(expected)));
            assert_eq!(state.value(false), Ok(Some(sum)));
        }
        let failed = State {
            failed: true,
            ..State::default()
        };
        assert_eq!(failed.value(false), Err(Overflow));
        assert!(add::<true>(failed, 0, 0).is_none());
    }
    #[test]
    fn bounded_states_detect_intermediate_and_combined_overflow() {
        assert!(
            add::<false>(
                State {
                    sum: i32::MAX.into(),
                    count: 1,
                    failed: false
                },
                1,
                1
            )
            .is_none()
        );
        assert!(
            add::<true>(
                State {
                    sum: i64::MAX,
                    count: 1,
                    failed: false
                },
                1,
                1
            )
            .is_none()
        );
        assert!(
            add::<true>(
                State {
                    sum: i64::MIN,
                    count: 1,
                    failed: false
                },
                -1,
                1
            )
            .is_none()
        );
        assert!(
            add::<true>(
                State {
                    sum: 0,
                    count: u64::MAX,
                    failed: false
                },
                0,
                1
            )
            .is_none()
        );
    }
}
