//! Exact bounded sums shared by integer and scaled currency aggregate adapters.
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
