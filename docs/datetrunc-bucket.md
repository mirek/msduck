# DATETRUNC and DATE_BUCKET reference contract

The retained fixture in reference/datetrunc-bucket.json contains 922 SQL
Server programs: 901 batches, 19 sp_executesql RPC calls and two prepared
statements (sp_prepare/sp_execute, nine executions in all). Each program was
captured in two fresh databases in each of two independent containers. Both
containers used the pinned SQL Server 2025 image digest
86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a.
The four raw captures matched exactly. They retain result rows, TDS column
descriptors, error number/state/class/line/text, information messages, DONE
tokens and RPC return status. scripts/capture-datetrunc-bucket.mjs
regenerates an artifact and checks it against the retained fixture. It
refuses to overwrite an existing fixture. Before starting any container, it
also rejects an output path that resolves to the fixture through a symlink
or hard link.

Every scalar probe projects the typed result and
`CONVERT(VARCHAR(50), result, 121)`. The text column keeps offsets and all
seven fractional digits. The tedious value is a JavaScript Date and cannot
hold them. The script pins `TZ=UTC` because tedious takes a bound
DATETIMEOFFSET parameter's offset from the host time zone.

msduck does not implement either function. This document only records
SQL Server behavior.

## Coverage

- DATETRUNC with all 15 dateparts (year, quarter, month, dayofyear, day, week,
  iso_week, hour, minute, second, millisecond, microsecond, weekday,
  nanosecond, tzoffset). Each part runs against date, time(0–7),
  datetime2(0–7), datetimeoffset(0–7), datetime and smalldatetime.
- DATE_BUCKET with width 2 and 13 dateparts. The 9 supported parts are
  year, quarter, month, week, day, hour, minute, second and millisecond.
  The rejected parts are dayofyear, iso_week, microsecond and nanosecond.
  Each part runs against the same 27 input types.
- Datepart abbreviations, brackets and case. Unknown, string and variable
  dateparts.
- String, integer and decimal inputs. The DATEFORMAT dmy effect on strings.
- Typed and untyped NULL in every argument position. Widths 0, negative,
  large, bigint/smallint/tinyint/decimal/float/string, variables and
  expressions. Origins before, equal to and after the date. Month-end and
  leap-day origins. Origins with mismatched types and scales, and string
  origins. Datetimeoffset origins with other offsets. Time origins. Range
  edges.
- DATEFIRST 1–7, SET LANGUAGE British and session restoration.
- Table columns, GROUP BY, SELECT INTO with the sys.columns declaration, and
  SQL_VARIANT_PROPERTY.
- RPC calls with typed parameters (datetime2, date, time, datetimeoffset,
  datetime, smalldatetime, nvarchar, int, bigint), including NULL and replay.
- Prepared executions with values that change, NULL, invalid widths and
  DATEFIRST applied inside the prepared batch.

## Result type

| Input | Observed result |
| --- | --- |
| date, time(s), datetime2(s), datetimeoffset(s) | Same type and scale as the input. Descriptors: Date, Time/scale s, DateTime2/scale s, DateTimeOffset/scale s. |
| datetime, smalldatetime | DateTimeN with length 8 and 4 respectively. |
| Character string (DATETRUNC only) | datetime2(7). SQL_VARIANT_PROPERTY reports BaseType datetime2, Scale 7. |
| Untyped NULL (DATETRUNC) | datetime2(7) NULL. |
| DATE_BUCKET, datetime2(3) date with a datetime2(7) origin | datetime2(7). |

Scalar result descriptors carry flags 33 (nullable and computed). The GROUP BY
projection carries flags 1. SELECT INTO created datetimeoffset(2),
datetime2(4) and smalldatetime nullable columns from the corresponding
source columns.

## DATETRUNC rules

- The sample `2024-05-15 13:47:39.1234567` is a Wednesday. With the default
  DATEFIRST 7, the results are: year 2024-01-01, quarter 2024-04-01, month
  2024-05-01, dayofyear and day 2024-05-15, week 2024-05-12 (Sunday) and
  iso_week 2024-05-13 (Monday). Smaller parts zero the lower fields and keep
  the input scale in the text, for example `13:47:39.1230000` for millisecond
  on datetime2(7).
- Truncation applies to the already-typed value. datetime2(6) microsecond
  gives `.123457` because the CAST had already rounded the value, while
  datetime2(7) gives `.1234560`. smalldatetime `13:47:29` stores 13:47, so
  second returns `13:47:00`.
- datetimeoffset truncates the local wall-clock value and keeps its offset:
  `2024-05-15 00:00:00 +05:30` and `2024-05-15 00:00:00 -08:00`. iso_week of
  `2024-05-13T02:00+14:00` returns `2024-05-13 00:00:00 +14:00`.
- week follows @@DATEFIRST. With DATEFIRST 1–7, week of 2024-05-15 was
  05-13, 05-14, 05-15, 05-09, 05-10, 05-11 and 05-12. SET LANGUAGE British
  set DATEFIRST 1 (05-13). iso_week always returned 05-13. The prepared
  statement observed DATEFIRST values that were set inside its batch.
- Strings are converted to datetime2(7). Examples:
  - `'13:47:39.1234567'` with minute returns `1900-01-01 13:47:00.0000000`.
  - `'2024-05-15T13:47:39.1234567+05:30'` with day drops the offset and
    returns `2024-05-15 00:00:00.0000000`.
  - Under DATEFORMAT dmy, `'05/04/2024 13:47'` returns 2024-04-05.
  - Unparseable text raises error 241 after the result descriptor.
  - int and numeric inputs raise error 8116 before metadata.
- Abbreviations are accepted: yy, yyyy, qq, q, mm, m, dy, y, dd, d, wk, ww,
  isowk, isoww, hh, mi, n, ss, s, ms and mcs. `[day]` and `DAY` are also
  accepted. dw, w, ns and tz map to weekday, nanosecond and tzoffset and are
  rejected with 9810.
- Datepart errors:
  - An unknown keyword raises 155 state 1 class 15
    (`'fortnight' is not a recognized datetrunc option.`).
  - A string literal or variable datepart raises 1023
    (`Invalid parameter 1 specified for datetrunc.`).
  - Both come before metadata.
- Unsupported datepart/type pairs raise error 9810 class 16 *after* the result
  descriptor, with no row and a DONE with no row count. The state depends on
  the input type and the reason:
  - State 10: a date/time category mismatch. This covers time parts on date,
    date parts on time, weekday and tzoffset on time, and tzoffset on date.
  - State 11: weekday on date/datetime2/datetimeoffset, nanosecond on
    time/datetime2/datetimeoffset, tzoffset on datetime2/datetimeoffset, and
    fractional parts finer than the scale. Millisecond is rejected for scale
    0–2 and microsecond for scale 0–5.
  - State 9 (datetime): microsecond, nanosecond, weekday, tzoffset.
    millisecond is accepted.
  - State 8 (smalldatetime): millisecond, microsecond, nanosecond, weekday,
    tzoffset.
- Range: week of date `0001-01-01` raises 9837 state 3. week of datetime
  `1753-01-01` raises 9837 state 4. week of smalldatetime `1900-01-01`
  raises 9837 state 2. All three come after metadata. iso_week of
  0001-01-01 (a Monday) succeeds.
  year of `9999-12-31 23:59:59.9999999` returns `9999-01-01`.

## DATE_BUCKET rules

- The default origin is 1900-01-01 00:00 of the input type. For
  datetimeoffset, that origin is UTC midnight. With width 2 on the sample,
  the results were:
  - year 2024-01-01, quarter 2024-01-01, month 2024-05-01, week 2024-05-06
    and day 2024-05-14;
  - hour 12:00, minute 13:46, second 13:47:38 and millisecond .122 in the
    input scale;
  - on datetimeoffset(0) `+05:30`: day `2024-05-14 05:30:00 +05:30` and hour
    `13:30 +05:30`. The input offset is preserved in the result.
- week buckets count from the origin (1900-01-01 is a Monday) and ignore
  DATEFIRST and LANGUAGE. Every DATEFIRST value and British returned
  2024-05-13 for width 1.
- Buckets take the floor, including before the origin. For example:
  - day width 7 of 1899-12-30 returns 1899-12-25;
  - hour width 5 of `1899-12-31 01:00` returns `1899-12-30 23:00`;
  - month width 5 of 2024-01-15 with origin 2024-05-31 returns 2023-12-31;
  - day width 3 with origin 2024-06-01 returns 2024-05-14.
- Month and year arithmetic from end-of-month origins clamps the day. Origin
  2024-01-31 with width 1 month gives 2024-02-29 for both 2024-02-29 and
  2024-03-30. Origin 2024-02-29 with width 1 year gives 2025-02-28 for
  2025-03-01. Quarter width 1 with origin 2024-02-10 gives 2024-05-10.
  An origin with fractional seconds (`00:30:00.5`) keeps the fraction in hour
  buckets (`13:30:00.5000000`).
- datetime millisecond buckets land on the datetime tick grid: width 2 of
  `.123` returns `.123`. smalldatetime second and millisecond buckets return
  whole minutes.
- Widths:
  - 1, 3, 7, 10 and 100 day widths returned 05-15, 05-13, 05-13, 05-10 and
    04-20.
  - 2147483647 days and 5000 years returned the origin 1900-01-01.
  - bigint, smallint, tinyint, decimal 2.5, float 2 and `1+1` are accepted
    and behaved as width 2.
  - Width 0 or a negative width raises 9834 state 1 after metadata. This
    also holds for variables, RPC parameters and prepared executions, where
    the return status is -6.
  - A typed NULL width returns NULL. An untyped NULL width raises 8116.
  - A varchar width raises 8116.
- Origins:
  - A typed or untyped NULL origin did *not* yield NULL. The result matched
    the default-origin bucket, and the same held for a NULL RPC origin.
  - A different origin type raises 8116 before metadata. With a date
    input and a datetime2 origin, the error names argument 3 (the date). With
    a datetime2 input and a datetime origin, it names argument 4.
  - A varchar origin raises 8116 for argument 4.
  - A datetimeoffset origin with a different offset is compared by instant,
    and the result keeps the input offset. Hour width 1 of
    `13:47:39 +05:30` with origin `00:30 +00:00` gives `13:00:00 +05:30`.
- DATE_BUCKET rejects character and numeric dates with 8116 for argument 3.
  It does not convert strings, unlike DATETRUNC. An untyped NULL date raises
  8116. A typed NULL date returns NULL of that type.
- Supported parts:
  - Category mismatches raise 9810 state 1 *before* metadata. These are
    hour through nanosecond on date, and year through iso_week (including
    day and dayofyear) on time.
  - dayofyear, iso_week, microsecond and nanosecond raise 9810 state 1
    *after* metadata on every type where the category matches. The message
    spells the function `Date_Bucket`.
  - Abbreviations yy, qq, mm, wk, dd, d, hh, mi, n, ss, s and ms are
    accepted. dy, isowk and mcs are rejected with 9810. Unknown and string
    dateparts raise 155 and 1023.
- Overflow below the type minimum raises 9835 state 1 after metadata, for
  example `Calculating date bucket for 'date' column caused an overflow.`
  The captured cases are day width 10 of date 0001-01-01 and day width 3 of
  datetime 1753-01-01. Day width 7 of 0001-01-01 and week buckets of the
  datetime and smalldatetime minimums are aligned and succeed. Year width 3
  of 9999-12-31 returns 9997-01-01.

## Completion tokens

- A successful batch ends with DONE (row count 1).
- A batch that fails after metadata ends with DONE without a row count. The
  batch also sends the column descriptor, and tedious surfaces it.
- A statement preceded by DECLARE has an extra leading DONE with more=true.
- A SET DATEFIRST or SET LANGUAGE batch sends DONE tokens for the SET
  statements.
- RPC calls end with DONEINPROC followed by DONEPROC. An 8116 compile error
  under RPC returns only DONEPROC.

## Not captured

- Languages other than us_english and British.
- DATEFORMAT orders other than dmy.
- Persisted computed columns, indexes and check constraints over either
  function.
- sql_variant and alias-type inputs.
- Effects of ANSI_WARNINGS, ARITHABORT and XACT_ABORT on errors 9834, 9835
  and 9837.
- Statement continuation after those errors within a larger batch.
- DATE_BUCKET with datetimeoffset origins of a different scale.
- Width types beyond those listed.
- Behavior under parallel plans.
- TVP and bulk-copy parameter paths.

## Proposed successors

**Deterministic core (`msduck-core`, with `msduck-sql` for binding).**
- Add a pure datepart catalog covering keywords, aliases, brackets and case,
  with separate supported sets for DATETRUNC and DATE_BUCKET.
- Add the result-type rule: result type follows the input, character strings
  become datetime2(7), and mixed datetime2 scales widen to the larger scale.
- Implement exact tick-level truncation and floor bucketing over explicit
  inputs: DATEFIRST, an optional origin, and the type's range and precision
  grid. This includes datetimeoffset wall-clock truncation against UTC
  bucketing, month-end clamping and datetime tick rounding.
- Return typed diagnostics carrying the exact numbers and states above:
  155, 1023, 8116 with argument ordinals, 9810 with states 1/8/9/10/11,
  9834, 9835 and 9837.
- Classify each diagnostic as either pre-metadata (binding) or post-metadata
  (execution).
- Scope: the new deterministic modules and their unit tests seeded from this
  fixture. No DuckDB, clocks or session access.

**Root integration (`msduck` crate).**
- Lower both functions through the AST so each operand is evaluated once
  per row.
- Supply the connection's DATEFIRST and DATEFORMAT at execution time. Bind
  RPC and prepared parameter types, including NULL widths and origins.
- Emit the captured descriptors: same-type scale, DateTimeN lengths 8/4 and
  flags. Order errors before or after COLMETADATA as captured, with the DONE,
  DONEINPROC/DONEPROC and return-status sequences.
- Propagate declarations through SELECT INTO and views.
- Scope: root adapters, a tedious client test, and an audit comparison
  against this fixture.
- Keep parser, engine, catalog, metadata and client-test changes to that
  successor. This reference task did not edit them.
