//! Pure SQL Server DATETRUNC and DATE_BUCKET rules over explicit calendar days
//! and 100-nanosecond ticks. Backend conversion and SQL AST binding live elsewhere.

const DAY: i128 = 864_000_000_000;
const HOUR: i128 = 36_000_000_000;
const MINUTE: i128 = 600_000_000;
const SECOND: i128 = 10_000_000;
const MILLISECOND: i128 = 10_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TemporalType {
    Date,
    Time(u8),
    DateTime2(u8),
    DateTimeOffset(u8),
    DateTime,
    SmallDateTime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceType {
    Temporal(TemporalType),
    Character,
    Integer,
    Numeric,
    UntypedNull,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WidthInput {
    Integer(Option<i64>),
    Character,
    UntypedNull,
}

fn invalid_argument(ty: &str, position: u8) -> RuleError {
    sql(
        8116,
        1,
        16,
        Phase::Binding,
        format!(
            "Argument data type {ty} is invalid for argument {position} of Date_Bucket function."
        ),
    )
}

pub fn bind_width(input: WidthInput) -> Result<Option<i64>, RuleError> {
    match input {
        WidthInput::Integer(value) => Ok(value),
        WidthInput::Character => Err(invalid_argument("varchar", 2)),
        WidthInput::UntypedNull => Err(invalid_argument("NULL", 2)),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Part {
    Year,
    Quarter,
    Month,
    DayOfYear,
    Day,
    Week,
    IsoWeek,
    Hour,
    Minute,
    Second,
    Millisecond,
    Microsecond,
    Weekday,
    Nanosecond,
    TzOffset,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Binding,
    Execution,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuleError {
    Sql {
        number: i32,
        state: u8,
        class: u8,
        phase: Phase,
        message: String,
    },
    Unsupported(&'static str),
}

fn sql(number: i32, state: u8, class: u8, phase: Phase, message: String) -> RuleError {
    RuleError::Sql {
        number,
        state,
        class,
        phase,
        message,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Value {
    /// Days after 0001-01-01 in the proleptic Gregorian calendar.
    pub day: i32,
    /// Local wall-clock ticks since midnight, in units of 100 ns.
    pub tick: i64,
    /// Minutes east of UTC. Zero for non-offset types.
    pub offset_minutes: i16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Bound {
    pub part: Part,
    pub result_type: TemporalType,
}

pub fn part(keyword: &str) -> Result<Part, RuleError> {
    part_for(keyword, "datetrunc")
}

fn part_for(keyword: &str, function: &str) -> Result<Part, RuleError> {
    let trimmed = keyword.trim();
    let word = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed);
    let word = word.to_ascii_lowercase();
    let part = match word.as_str() {
        "year" | "yy" | "yyyy" => Part::Year,
        "quarter" | "qq" | "q" => Part::Quarter,
        "month" | "mm" | "m" => Part::Month,
        "dayofyear" | "dy" | "y" => Part::DayOfYear,
        "day" | "dd" | "d" => Part::Day,
        "week" | "wk" | "ww" => Part::Week,
        "iso_week" | "isowk" | "isoww" => Part::IsoWeek,
        "hour" | "hh" => Part::Hour,
        "minute" | "mi" | "n" => Part::Minute,
        "second" | "ss" | "s" => Part::Second,
        "millisecond" | "ms" => Part::Millisecond,
        "microsecond" | "mcs" => Part::Microsecond,
        "weekday" | "dw" | "w" => Part::Weekday,
        "nanosecond" | "ns" => Part::Nanosecond,
        "tzoffset" | "tz" => Part::TzOffset,
        _ => {
            return Err(sql(
                155,
                1,
                15,
                Phase::Binding,
                format!("'{word}' is not a recognized {function} option."),
            ));
        }
    };
    Ok(part)
}

pub fn non_keyword_part() -> RuleError {
    non_keyword_part_for("datetrunc")
}

pub fn non_keyword_part_for(function: &str) -> RuleError {
    sql(
        1023,
        1,
        15,
        Phase::Binding,
        format!("Invalid parameter 1 specified for {function}."),
    )
}

fn result_type(source: SourceType) -> TemporalType {
    match source {
        SourceType::Temporal(ty) => ty,
        SourceType::Character | SourceType::UntypedNull => TemporalType::DateTime2(7),
        SourceType::Integer | SourceType::Numeric => TemporalType::DateTime2(7),
    }
}

pub fn bind_trunc(keyword: &str, source: SourceType) -> Result<Bound, RuleError> {
    let part = part(keyword)?;
    if matches!(source, SourceType::Integer | SourceType::Numeric) {
        let ty = if source == SourceType::Integer {
            "int"
        } else {
            "numeric"
        };
        return Err(sql(
            8116,
            1,
            16,
            Phase::Binding,
            format!("Argument data type {ty} is invalid for argument 2 of datetrunc function."),
        ));
    }
    let ty = result_type(source);
    if let Some(state) = trunc_rejection(part, ty) {
        return Err(sql(
            9810,
            state,
            16,
            Phase::Execution,
            format!(
                "The datepart {} is not supported by date function datetrunc for data type {}.",
                part_name(part),
                type_name(ty)
            ),
        ));
    }
    Ok(Bound {
        part,
        result_type: ty,
    })
}

fn trunc_rejection(part: Part, ty: TemporalType) -> Option<u8> {
    use Part::*;
    match ty {
        TemporalType::Date => match part {
            Year | Quarter | Month | DayOfYear | Day | Week | IsoWeek => None,
            Weekday => Some(11),
            _ => Some(10),
        },
        TemporalType::Time(s) => match part {
            Year | Quarter | Month | DayOfYear | Day | Week | IsoWeek | Weekday | TzOffset => {
                Some(10)
            }
            Nanosecond => Some(11),
            Millisecond if s < 3 => Some(11),
            Microsecond if s < 6 => Some(11),
            _ => None,
        },
        TemporalType::DateTime2(s) | TemporalType::DateTimeOffset(s) => match part {
            Weekday | Nanosecond | TzOffset => Some(11),
            Millisecond if s < 3 => Some(11),
            Microsecond if s < 6 => Some(11),
            _ => None,
        },
        TemporalType::DateTime => match part {
            Microsecond | Nanosecond | Weekday | TzOffset => Some(9),
            _ => None,
        },
        TemporalType::SmallDateTime => match part {
            Millisecond | Microsecond | Nanosecond | Weekday | TzOffset => Some(8),
            _ => None,
        },
    }
}

pub fn bind_bucket(
    keyword: &str,
    source: TemporalType,
    origin: Option<TemporalType>,
) -> Result<Bound, RuleError> {
    bind_bucket_source(
        keyword,
        SourceType::Temporal(source),
        origin.map(SourceType::Temporal),
    )
}

pub fn bind_bucket_source(
    keyword: &str,
    source: SourceType,
    origin: Option<SourceType>,
) -> Result<Bound, RuleError> {
    let part = part_for(keyword, "Date_Bucket")?;
    let source = match source {
        SourceType::Temporal(ty) => ty,
        SourceType::Character => return Err(invalid_argument("varchar", 3)),
        SourceType::Integer => return Err(invalid_argument("int", 3)),
        SourceType::Numeric => return Err(invalid_argument("numeric", 3)),
        SourceType::UntypedNull => return Err(invalid_argument("NULL", 3)),
    };
    let origin = match origin {
        Some(SourceType::Temporal(ty)) => Some(ty),
        Some(SourceType::Character) => return Err(invalid_argument("varchar", 4)),
        Some(SourceType::Integer) => return Err(invalid_argument("int", 4)),
        Some(SourceType::Numeric) => return Err(invalid_argument("numeric", 4)),
        Some(SourceType::UntypedNull) | None => None,
    };
    use Part::*;
    let is_date_part = matches!(
        part,
        Year | Quarter | Month | DayOfYear | Day | Week | IsoWeek
    );
    let is_time_part = matches!(
        part,
        Hour | Minute | Second | Millisecond | Microsecond | Nanosecond
    );
    let category_mismatch = (matches!(source, TemporalType::Date) && is_time_part)
        || (matches!(source, TemporalType::Time(_)) && is_date_part);
    if category_mismatch {
        return Err(sql(
            9810,
            1,
            16,
            Phase::Binding,
            format!(
                "The datepart {} is not supported by date function Date_Bucket for data type {}.",
                part_name(part),
                type_name(source)
            ),
        ));
    }
    let result_type = match (source, origin) {
        (TemporalType::DateTime2(a), Some(TemporalType::DateTime2(b))) => {
            TemporalType::DateTime2(a.max(b))
        }
        (TemporalType::Time(a), Some(TemporalType::Time(b))) => TemporalType::Time(a.max(b)),
        (TemporalType::DateTimeOffset(a), Some(TemporalType::DateTimeOffset(b))) if a == b => {
            source
        }
        (TemporalType::DateTimeOffset(_), Some(TemporalType::DateTimeOffset(_))) => {
            return Err(RuleError::Unsupported(
                "uncaptured datetimeoffset scale combination",
            ));
        }
        (TemporalType::Date, Some(TemporalType::DateTime2(_))) => {
            return Err(invalid_argument("date", 3));
        }
        (TemporalType::DateTime2(_), Some(TemporalType::DateTime)) => {
            return Err(invalid_argument("datetime", 4));
        }
        (_, Some(other)) if source != other => {
            return Err(RuleError::Unsupported("uncaptured origin type mismatch"));
        }
        _ => source,
    };
    if !matches!(
        part,
        Year | Quarter | Month | Week | Day | Hour | Minute | Second | Millisecond
    ) {
        return Err(sql(
            9810,
            1,
            16,
            Phase::Execution,
            format!(
                "The datepart {} is not supported by date function Date_Bucket for data type {}.",
                part_name(part),
                type_name(source)
            ),
        ));
    }
    Ok(Bound { part, result_type })
}

pub fn part_name(part: Part) -> &'static str {
    match part {
        Part::Year => "year",
        Part::Quarter => "quarter",
        Part::Month => "month",
        Part::DayOfYear => "dayofyear",
        Part::Day => "day",
        Part::Week => "week",
        Part::IsoWeek => "iso_week",
        Part::Hour => "hour",
        Part::Minute => "minute",
        Part::Second => "second",
        Part::Millisecond => "millisecond",
        Part::Microsecond => "microsecond",
        Part::Weekday => "weekday",
        Part::Nanosecond => "nanosecond",
        Part::TzOffset => "tzoffset",
    }
}

pub fn type_name(ty: TemporalType) -> &'static str {
    match ty {
        TemporalType::Date => "date",
        TemporalType::Time(_) => "time",
        TemporalType::DateTime2(_) => "datetime2",
        TemporalType::DateTimeOffset(_) => "datetimeoffset",
        TemporalType::DateTime => "datetime",
        TemporalType::SmallDateTime => "smalldatetime",
    }
}

fn leap(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn month_length(year: i32, month: i32) -> i32 {
    match month {
        2 if leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

pub fn day_from_ymd(year: i32, month: i32, day: i32) -> Option<i32> {
    if !(1..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=month_length(year, month)).contains(&day)
    {
        return None;
    }
    let previous = year - 1;
    let mut n = 365 * previous + previous / 4 - previous / 100 + previous / 400;
    for m in 1..month {
        n += month_length(year, m);
    }
    Some(n + day - 1)
}

pub fn ymd_from_day(day: i32) -> Option<(i32, i32, i32)> {
    if day < 0 || day > day_from_ymd(9999, 12, 31)? {
        return None;
    }
    let (mut lo, mut hi) = (1, 10_000);
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if day_from_ymd(mid, 1, 1).is_some_and(|start| start <= day) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let mut remainder = day - day_from_ymd(lo, 1, 1)?;
    let mut month = 1;
    while remainder >= month_length(lo, month) {
        remainder -= month_length(lo, month);
        month += 1;
    }
    Some((lo, month, remainder + 1))
}

impl Value {
    pub fn new(year: i32, month: i32, day: i32, tick: i64, offset_minutes: i16) -> Option<Self> {
        if !(0..DAY as i64).contains(&tick) || !(-840..=840).contains(&offset_minutes) {
            return None;
        }
        Some(Self {
            day: day_from_ymd(year, month, day)?,
            tick,
            offset_minutes,
        })
    }

    fn absolute(self) -> i128 {
        i128::from(self.day) * DAY + i128::from(self.tick)
    }

    fn from_absolute(absolute: i128, offset_minutes: i16) -> Result<Self, RuleError> {
        let day =
            i32::try_from(absolute.div_euclid(DAY)).map_err(|_| range_error(9835, 1, "date"))?;
        let tick = i64::try_from(absolute.rem_euclid(DAY)).expect("one day fits i64");
        if ymd_from_day(day).is_none() {
            return Err(range_error(9835, 1, "date"));
        }
        Ok(Self {
            day,
            tick,
            offset_minutes,
        })
    }
}

fn range_error(number: i32, state: u8, ty: &str) -> RuleError {
    let message = if number == 9835 {
        format!("Calculating date bucket for '{ty}' column caused an overflow.")
    } else {
        format!(
            "An invalid {ty} value was encountered: The date value is less than the minimum date value allowed for the data type."
        )
    };
    sql(number, state, 16, Phase::Execution, message)
}

fn minimum(ty: TemporalType) -> i32 {
    match ty {
        TemporalType::DateTime => day_from_ymd(1753, 1, 1).unwrap(),
        TemporalType::SmallDateTime => day_from_ymd(1900, 1, 1).unwrap(),
        _ => 0,
    }
}

fn maximum(ty: TemporalType) -> i32 {
    match ty {
        TemporalType::SmallDateTime => day_from_ymd(2079, 6, 6).unwrap(),
        _ => day_from_ymd(9999, 12, 31).unwrap(),
    }
}

fn range_check(value: Value, ty: TemporalType, function: &'static str) -> Result<Value, RuleError> {
    if matches!(ty, TemporalType::Time(_)) {
        return Ok(value);
    }
    if value.day < minimum(ty) || value.day > maximum(ty) {
        if function == "datetrunc" {
            if value.day > maximum(ty) {
                return Err(RuleError::Unsupported(
                    "upper-bound datetrunc error not captured",
                ));
            }
            let state = match ty {
                TemporalType::SmallDateTime => 2,
                TemporalType::DateTime => 4,
                _ => 3,
            };
            return Err(range_error(9837, state, type_name(ty)));
        }
        return Err(range_error(9835, 1, type_name(ty)));
    }
    Ok(value)
}

/// Caller supplies an already-converted value, including SQL Server's source
/// rounding. `datefirst` is the session's explicit 1=Monday through 7=Sunday.
pub fn truncate(
    bound: Bound,
    value: Option<Value>,
    datefirst: u8,
) -> Result<Option<Value>, RuleError> {
    let Some(mut value) = value else {
        return Ok(None);
    };
    if !(1..=7).contains(&datefirst) {
        return Err(RuleError::Unsupported("DATEFIRST outside 1..7"));
    }
    if value.day < minimum(bound.result_type) || value.day > maximum(bound.result_type) {
        return Err(RuleError::Unsupported("input outside declared type range"));
    }
    use Part::*;
    match bound.part {
        Year | Quarter | Month => {
            let (year, month, _) =
                ymd_from_day(value.day).ok_or(RuleError::Unsupported("calendar input"))?;
            let month = match bound.part {
                Year => 1,
                Quarter => ((month - 1) / 3) * 3 + 1,
                _ => month,
            };
            value.day = day_from_ymd(year, month, 1).expect("valid truncation");
            value.tick = 0;
        }
        Week | IsoWeek => {
            let first = if bound.part == IsoWeek {
                1
            } else {
                i32::from(datefirst)
            };
            let weekday = value.day.rem_euclid(7) + 1;
            value.day -= (weekday - first).rem_euclid(7);
            value.tick = 0;
        }
        DayOfYear | Day => value.tick = 0,
        Hour | Minute | Second | Millisecond | Microsecond => {
            let quantum = match bound.part {
                Hour => HOUR,
                Minute => MINUTE,
                Second => SECOND,
                Millisecond => MILLISECOND,
                _ => 10,
            };
            value.tick = i64::try_from(i128::from(value.tick).div_euclid(quantum) * quantum)
                .expect("day tick");
        }
        Weekday | Nanosecond | TzOffset => {
            return Err(RuleError::Unsupported("unsupported datepart was not bound"));
        }
    }
    Ok(Some(range_check(value, bound.result_type, "datetrunc")?))
}

fn default_origin(ty: TemporalType) -> Value {
    Value {
        day: day_from_ymd(1900, 1, 1).unwrap(),
        tick: 0,
        offset_minutes: 0,
    }
    .with_time_origin(ty)
}

impl Value {
    fn with_time_origin(mut self, ty: TemporalType) -> Self {
        if matches!(ty, TemporalType::Time(_)) {
            self.day = 0;
        }
        self
    }
}

fn shift_months(origin: Value, months: i128) -> Result<Value, RuleError> {
    let (year, month, day) =
        ymd_from_day(origin.day).ok_or(RuleError::Unsupported("origin calendar"))?;
    let target = i128::from(year - 1) * 12 + i128::from(month - 1) + months;
    let year =
        i32::try_from(target.div_euclid(12) + 1).map_err(|_| range_error(9835, 1, "date"))?;
    let month = i32::try_from(target.rem_euclid(12) + 1).expect("month range");
    let day = day.min(month_length(year, month));
    let day = day_from_ymd(year, month, day).ok_or_else(|| range_error(9835, 1, "date"))?;
    Ok(Value { day, ..origin })
}

fn quantum(part: Part) -> Option<i128> {
    match part {
        Part::Week => Some(DAY * 7),
        Part::Day => Some(DAY),
        Part::Hour => Some(HOUR),
        Part::Minute => Some(MINUTE),
        Part::Second => Some(SECOND),
        Part::Millisecond => Some(MILLISECOND),
        _ => None,
    }
}

fn datetime_grid(value: Value) -> Result<Value, RuleError> {
    let absolute = value.absolute();
    let ticks = (absolute * 300 + SECOND / 2).div_euclid(SECOND);
    let rounded = (ticks * SECOND + 150).div_euclid(300);
    Value::from_absolute(rounded, value.offset_minutes)
}

/// A typed NULL width yields NULL. A NULL origin selects the default origin.
/// Binding rejects unsupported source/origin combinations before this call.
pub fn bucket(
    bound: Bound,
    width: Option<i64>,
    value: Option<Value>,
    origin: Option<Value>,
) -> Result<Option<Value>, RuleError> {
    let Some(width) = width else {
        return Ok(None);
    };
    let Some(value) = value else {
        return Ok(None);
    };
    if width <= 0 {
        return Err(sql(
            9834,
            1,
            16,
            Phase::Execution,
            "Invalid bucket width value passed to date_bucket function. Only positive values are allowed.".to_owned(),
        ));
    }
    let ty = bound.result_type;
    let origin = origin.unwrap_or_else(|| default_origin(ty));
    let width = i128::from(width);
    let out = if matches!(bound.part, Part::Year | Part::Quarter | Part::Month) {
        if matches!(ty, TemporalType::Time(_)) {
            return Err(RuleError::Unsupported("date part on time"));
        }
        let local_origin = if matches!(ty, TemporalType::DateTimeOffset(_)) {
            // Month arithmetic uses the input's wall-clock calendar after both
            // instants are placed under the input offset.
            Value::from_absolute(
                origin.absolute() - i128::from(origin.offset_minutes) * MINUTE
                    + i128::from(value.offset_minutes) * MINUTE,
                value.offset_minutes,
            )?
        } else {
            origin
        };
        let (year, month, _) =
            ymd_from_day(value.day).ok_or(RuleError::Unsupported("calendar input"))?;
        let (origin_year, origin_month, _) =
            ymd_from_day(local_origin.day).ok_or(RuleError::Unsupported("origin calendar"))?;
        let months_per_part = match bound.part {
            Part::Year => 12,
            Part::Quarter => 3,
            _ => 1,
        };
        let step = width * months_per_part;
        let difference = i128::from(year - origin_year) * 12 + i128::from(month - origin_month);
        let mut bucket_index = difference.div_euclid(step);
        let mut candidate = shift_months(local_origin, bucket_index * step)?;
        if candidate.absolute() > value.absolute() {
            bucket_index -= 1;
            candidate = shift_months(local_origin, bucket_index * step)?;
        }
        candidate
    } else {
        let unit = quantum(bound.part).ok_or(RuleError::Unsupported("unbound bucket datepart"))?;
        let size = unit * width;
        let input_absolute = value.absolute() - i128::from(value.offset_minutes) * MINUTE;
        let origin_absolute = origin.absolute() - i128::from(origin.offset_minutes) * MINUTE;
        let result_absolute =
            origin_absolute + (input_absolute - origin_absolute).div_euclid(size) * size;
        Value::from_absolute(
            result_absolute + i128::from(value.offset_minutes) * MINUTE,
            value.offset_minutes,
        )?
    };
    let out = if ty == TemporalType::DateTime {
        datetime_grid(out)?
    } else {
        out
    };
    Ok(Some(range_check(out, ty, "date_bucket")?))
}

/// Style-121 display used by the retained reference fixture.
pub fn text(value: Value, ty: TemporalType) -> Result<String, RuleError> {
    let (year, month, day) =
        ymd_from_day(value.day).ok_or(RuleError::Unsupported("calendar value"))?;
    let mut remainder = i128::from(value.tick);
    let hour = remainder / HOUR;
    remainder %= HOUR;
    let minute = remainder / MINUTE;
    remainder %= MINUTE;
    let second = remainder / SECOND;
    remainder %= SECOND;
    let date = format!("{year:04}-{month:02}-{day:02}");
    let clock = format!("{hour:02}:{minute:02}:{second:02}");
    let fractional = match ty {
        TemporalType::Time(s) | TemporalType::DateTime2(s) | TemporalType::DateTimeOffset(s)
            if s > 0 =>
        {
            let digits = format!("{remainder:07}");
            format!(".{}", &digits[..usize::from(s)])
        }
        TemporalType::DateTime => format!(".{:03}", (remainder + MILLISECOND / 2) / MILLISECOND),
        TemporalType::SmallDateTime => ".000".to_owned(),
        _ => String::new(),
    };
    Ok(match ty {
        TemporalType::Date => date,
        TemporalType::Time(_) => format!("{clock}{fractional}"),
        TemporalType::DateTimeOffset(_) => {
            let offset = i32::from(value.offset_minutes);
            let sign = if offset < 0 { '-' } else { '+' };
            let offset = offset.abs();
            format!(
                "{date} {clock}{fractional} {sign}{:02}:{:02}",
                offset / 60,
                offset % 60
            )
        }
        _ => format!("{date} {clock}{fractional}"),
    })
}
