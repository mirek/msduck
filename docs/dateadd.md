# DATEADD: DATE, TIME, DATETIME2 and DATETIMEOFFSET

The current implementation accepts typed DATE/TIME/DATETIME2/DATETIMEOFFSET inputs and INT offsets. Year,
quarter and month additions preserve the original day when possible and clamp
to the target month's last day otherwise. Week adds seven days; day, weekday and
dayofyear add calendar days. All documented aliases for these units are accepted.
Fractions in the offset truncate through the existing integer conversion path.

For DATE inputs, the native functions return DATE, including empty and NULL
result sets. They
read bounded live vector slots, propagate NULL inputs, and reject values outside
0001-01-01 through 9999-12-31 with error 517. Subday units on DATE report 9810.
Arguments are passed once to the native function. Registration on every connection
also supports expressions retained in defaults and views.

The implementation follows the calendar, offset and return-type rules in the
[Microsoft DATEADD reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/dateadd-transact-sql?view=sql-server-2017).
This is a bounded implementation, not complete DATEADD compatibility. String
literals must eventually return legacy DATETIME; they currently reject explicitly.
DATETIME, SMALLDATETIME and newer BIGINT offsets
remain open. Error precedence involving unsupported units, NULLs and invalid
numbers has not been compared against a live SQL Server.

Validation covers month-end/leap-year examples, negative fractions, boundaries,
prepared rebinding, DATE wire metadata, column/datepart name collisions, stored
defaults and views. Native tests compare 6,000 rows per supported arithmetic unit
against DuckDB calendar arithmetic and separately check SQL Server DATE bounds.
The local audit captures `DATEADD typed DATE arithmetic`; a complete capture does
not establish agreement with a live SQL Server.

Verification for this increment: formatting and Clippy are clean; 151 Rust tests
and 240 client tests pass. All 136 local audit cases have complete captures.
The new capture contains DATE descriptors, 2024-02-29 / 2025-02-28 calendar
results, negative fractional truncation to 2023-12-31, NULL and caught error 517.

DATETIME2 arithmetic retains the input scale (0–7), operating directly on exact
100ns ticks. Calendar additions preserve the time of day, subday additions carry
across calendar boundaries, and nanosecond offsets round to 100ns increments.
Final scale rounding and calendar limits are checked. Large offsets use i128
intermediates to avoid overflow before the SQL Server range check.

Type annotation covers casts, parameters, columns, derived results and visible
correlated inputs, while respecting local name shadowing. Tests also cover scalar
subquery inputs, mixed-scale CASE/comparisons, views and defaults. Native tests
exercise all scales across 6,000 rows with independent NULL patterns.
Negative nanosecond ties and low-scale rounding are captured for future live
comparison; their local tests are implementation evidence, not reference-server
verification.

Upstream review: the cached mirek/mssqlite `packages/engine/src/date-functions.ts`
uses the same signed half-away-from-zero nanosecond-offset calculation and
calendar clamping. Its DATEADD formatter defaults non-offset values to scale 3,
so that representation cannot be reused for DATETIME2 precision preservation.
The Rust implementation retains the established tagged DATETIME2 representation.
Upstream `engine.test.ts` includes exact nanosecond and next-day cases for
DATETIMEOFFSET; these informed the offset-preserving extension below.

DATETIME2 extension verification: 153 Rust tests and 241 client tests pass;
formatting and Clippy are clean. All 137 local audit captures are complete.
The new capture preserves scales 7/0/3, exact 100ns increments, NULL and caught
517 overflow. Live SQL Server parity remains unverified.

Datepart keyword binding: type annotation now skips the first keyword argument
of DATEADD/DATEPART/DATENAME (also DATEDIFF spellings reserved for implementation).
Previously, a DATETIME2 source column named `year` could cause `year` inside
`MAX(DATEADD(year,1,year))`, CASE or comparisons to become a typed column
expression, making a valid function call fail validation. A shared visitor tracks
only the immediate keyword leaf; same-named value arguments still receive their
normal types. Client regression coverage includes nested calls, NULL rows,
grouping, windows, CASE/COALESCE, predicates and prepared rebinding.

Keyword-binding verification: formatting and Clippy are clean; all 153 Rust and
242 client tests pass. All 138 local audit captures are complete. The new case
returns DATETIME2(3) 2025-02-28 12:34:56.123, INT 2024, and February, followed
by the same DATETIME2 value from the CASE/predicate query, with no errors.

DATETIMEOFFSET arithmetic now retains the input scale and fixed offset. Calendar
units operate on local fields, including month-end clamping when the UTC date
falls on another day. Subday units use exact ticks and the existing nanosecond
rounding rule. Both local and UTC result ranges are checked, with error 517 on
overflow. Tests cover typed NULL/empty results, all scales, columns, updates,
prepared calls, nested arithmetic, comparisons, views and conditional results.
Native vector tests check retained offsets, NULL child validity and single
argument evaluation across 6,000 rows. Negative nanosecond ties, reduced-scale
rounding and error precedence still need live SQL Server comparison.

TIME DATEADD supports hour through nanosecond units, preserving declared scales
0–7 and wrapping forward/backward across midnight. Wide intermediates support
all INT amounts without arithmetic overflow. Calendar units reject with 9810.
Source-column, nested expression and result descriptor inference retain TIME
scale through columns, views, prepared execution, NULLs and empty results.
Native tests cover precision, INT limits, 6,000-row NULL patterns and single
argument evaluation. Reduced-scale rounding, negative nanosecond ties and
unsupported-unit error timing still require live SQL Server comparison. The
upstream formatter defaults non-offset values to date/time text; TIME uses a
separate native representation and cannot reuse that formatter directly.
