# DATEDIFF and DATEDIFF_BIG

Both functions count datepart boundaries using exact DATETIME2 ticks. Calendar
units use year/month/day indexes; weeks begin on Sunday independently of
DATEFIRST. Subsecond calculations retain 100ns precision. DATEDIFF returns INT,
DATEDIFF_BIG returns BIGINT, and checked narrowing reports error 535 on overflow.
Intermediate nanosecond counts use i128 so subtraction cannot overflow first.

The implementation follows the documented
[DATEDIFF](https://learn.microsoft.com/en-us/sql/t-sql/functions/datediff-transact-sql?view=sql-server-ver17)
and [DATEDIFF_BIG](https://learn.microsoft.com/en-us/sql/t-sql/functions/datediff-big-transact-sql?view=sql-server-ver17)
boundary and result-width rules. Supported conversion paths include DATE,
DATETIME2, TIME, ISO date/time text, and integral day offsets from 1900-01-01.
NULLs propagate and result metadata retains its integer width for empty sets.

The shared datepart aliases also serve DATEADD. Type inference retains the INT
or BIGINT result through aggregates and conditional/arithmetic expressions.
Native registration supports persisted defaults and views. Input macros resolve
the type once at binding and evaluate each argument once at execution.

Tests exercise calendar and 100ns boundary crossings, reversals, Sunday weeks,
DATEFIRST independence, full-range microseconds, narrow and wide overflow,
NULL/empty metadata, prepared rebinding, mixed scales, same-named datepart
columns, aggregate widths, defaults, views and atomic UPDATE failure. Native
checks cover 6,000-row chunks and volatile arguments. The local audit records
exact values and overflow; it does not establish live SQL Server parity.

Upstream review: mirek/mssqlite's `packages/engine/src/date-functions.ts` also
uses integer boundary indexes and BigInt subday ticks, but converts the final
result to JavaScript Number. Rust retains integer precision and checks the
required result width. The upstream implementation also handles offsets; that
behavior remains pending here.

Open work includes DATETIMEOFFSET, complete legacy DATETIME/SMALLDATETIME
coercion, non-ISO/dateformat-sensitive text, fractional numeric legacy-date
inputs, exact error state/message parity, and live SQL Server comparison.

Verification: 155 Rust tests and 243 client tests pass; formatting and Clippy are
clean. All 139 local audit cases have complete captures. The new case records
boundary counts 1/1/100, exact BIGINT 315537897599999999, NULL, Sunday/Monday
counts 1/0 under DATEFIRST 1, and caught error 535 with the expected integer widths.
