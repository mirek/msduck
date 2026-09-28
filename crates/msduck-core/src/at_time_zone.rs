//! Pure transition resolution for SQL Server's AT TIME ZONE operation.
//!
//! The caller owns the versioned zone rules. Ticks are 100 ns units since
//! 0001-01-01, matching `DateTime2`; this module never consults the host clock.
use anyhow::{Result, ensure};

const MINUTE_TICKS: i64 = 600_000_000;
const MAX_TICKS_EXCLUSIVE: i64 = 3_155_378_976_000_000_000;
const MAX_TRANSITIONS: usize = 100_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transition {
    pub utc_ticks: i64,
    pub offset_before_minutes: i16,
    pub offset_after_minutes: i16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resolution {
    pub utc_ticks: i64,
    pub local_ticks: i64,
    pub offset_minutes: i16,
}

/// A validated, caller-owned snapshot of one zone's UTC transitions.
pub struct Rules<'a> {
    initial_offset_minutes: i16,
    transitions: &'a [Transition],
}

fn valid_ticks(ticks: i64) -> bool {
    (0..MAX_TICKS_EXCLUSIVE).contains(&ticks)
}

fn valid_offset(minutes: i16) -> bool {
    (-840..=840).contains(&minutes)
}

fn shifted(ticks: i64, minutes: i16) -> Result<i64> {
    ticks
        .checked_add(i64::from(minutes) * MINUTE_TICKS)
        .ok_or_else(|| anyhow::anyhow!("time-zone tick overflow"))
}

impl<'a> Rules<'a> {
    pub fn new(initial_offset_minutes: i16, transitions: &'a [Transition]) -> Result<Self> {
        ensure!(
            valid_offset(initial_offset_minutes),
            "invalid initial time-zone offset"
        );
        ensure!(
            transitions.len() <= MAX_TRANSITIONS,
            "too many time-zone transitions"
        );
        let mut previous_utc = None;
        let mut previous_offset = initial_offset_minutes;
        for transition in transitions {
            ensure!(
                valid_ticks(transition.utc_ticks),
                "time-zone transition outside DATETIME2 range"
            );
            ensure!(
                previous_utc.is_none_or(|utc| utc < transition.utc_ticks),
                "time-zone transitions must increase"
            );
            ensure!(
                transition.offset_before_minutes == previous_offset,
                "discontinuous time-zone offsets"
            );
            ensure!(
                valid_offset(transition.offset_after_minutes),
                "invalid time-zone offset"
            );
            ensure!(
                transition.offset_before_minutes != transition.offset_after_minutes,
                "no-op time-zone transition"
            );
            previous_utc = Some(transition.utc_ticks);
            previous_offset = transition.offset_after_minutes;
        }
        Ok(Self {
            initial_offset_minutes,
            transitions,
        })
    }

    /// An offset-bearing input already identifies an instant. Retain that UTC
    /// instant and select the destination zone's offset at that instant.
    pub fn resolve_utc(&self, utc_ticks: i64) -> Result<Resolution> {
        ensure!(
            valid_ticks(utc_ticks),
            "UTC instant outside DATETIME2 range"
        );
        let index = self
            .transitions
            .partition_point(|t| t.utc_ticks <= utc_ticks);
        let offset_minutes = if index == 0 {
            self.initial_offset_minutes
        } else {
            self.transitions[index - 1].offset_after_minutes
        };
        let local_ticks = shifted(utc_ticks, offset_minutes)?;
        ensure!(
            valid_ticks(local_ticks),
            "local time outside DATETIME2 range"
        );
        Ok(Resolution {
            utc_ticks,
            local_ticks,
            offset_minutes,
        })
    }

    /// An offset-free input is a local wall time in this zone. In an overlap,
    /// select the first occurrence (the pre-change offset). In a forward gap,
    /// move the local wall time forward by the size of the gap.
    pub fn resolve_local(&self, local_ticks: i64) -> Result<Resolution> {
        ensure!(
            valid_ticks(local_ticks),
            "local time outside DATETIME2 range"
        );
        let mut segment_start = 0;
        let mut offset_minutes = self.initial_offset_minutes;
        let mut first = None;
        for transition in self.transitions {
            let utc_ticks = shifted(local_ticks, -offset_minutes)?;
            if (segment_start..transition.utc_ticks).contains(&utc_ticks) && valid_ticks(utc_ticks)
            {
                first = Some(Resolution {
                    utc_ticks,
                    local_ticks,
                    offset_minutes,
                });
                break;
            }
            segment_start = transition.utc_ticks;
            offset_minutes = transition.offset_after_minutes;
        }
        if first.is_none() {
            let utc_ticks = shifted(local_ticks, -offset_minutes)?;
            if (segment_start..MAX_TICKS_EXCLUSIVE).contains(&utc_ticks) {
                first = Some(Resolution {
                    utc_ticks,
                    local_ticks,
                    offset_minutes,
                });
            }
        }
        if let Some(resolution) = first {
            return Ok(resolution);
        }

        // No UTC interval contains this local wall time. A positive jump makes
        // precisely this range nonexistent; the old offset identifies the UTC
        // instant while the new offset supplies its adjusted local display.
        for transition in self.transitions {
            if transition.offset_after_minutes <= transition.offset_before_minutes {
                continue;
            }
            let gap_start = shifted(transition.utc_ticks, transition.offset_before_minutes)?;
            let gap_end = shifted(transition.utc_ticks, transition.offset_after_minutes)?;
            if (gap_start..gap_end).contains(&local_ticks) {
                let utc_ticks = shifted(local_ticks, -transition.offset_before_minutes)?;
                let adjusted_local = shifted(utc_ticks, transition.offset_after_minutes)?;
                ensure!(
                    valid_ticks(utc_ticks) && valid_ticks(adjusted_local),
                    "resolved time outside DATETIME2 range"
                );
                return Ok(Resolution {
                    utc_ticks,
                    local_ticks: adjusted_local,
                    offset_minutes: transition.offset_after_minutes,
                });
            }
        }
        anyhow::bail!("local time has no valid zone mapping")
    }
}
