//! The clock read for current-time functions (SYSDATETIMEOFFSET, GETDATE, ...).
use msduck_sql::session_function::Clock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Read the system clock and the local UTC offset once, for one statement.
pub fn now() -> Clock {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    Clock {
        utc_ticks: i64::try_from(elapsed.as_nanos() / 100).unwrap_or(i64::MAX),
        offset_minutes: local_offset_minutes(elapsed.as_secs() as i64),
    }
}

/// The local time zone's offset from UTC at `seconds` since the epoch.
#[cfg(unix)]
fn local_offset_minutes(seconds: i64) -> i16 {
    let time = seconds as libc::time_t;
    // SAFETY: localtime_r writes only to the provided tm.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&time, &mut tm) }.is_null() {
        return 0;
    }
    (tm.tm_gmtoff / 60) as i16
}

/// Without a portable local time zone lookup, local time is UTC.
#[cfg(not(unix))]
fn local_offset_minutes(_seconds: i64) -> i16 {
    0
}
