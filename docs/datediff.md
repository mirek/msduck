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

Numeric operands now follow the legacy DATETIME 1/300-second grid. The
[38-observation SQL Server 2025 fixture](../reference/datediff-numeric.json)
was captured in four fresh databases across two independent pinned containers
and independently replayed. It covers literals, stored columns, RPC inputs,
typed NULLs, BIT, integer, DECIMAL/NUMERIC, REAL/FLOAT, MONEY/SMALLMONEY,
range failures and mixed numeric/DATETIME2 input. DATEDIFF retains the grid
position through boundary counting: one legacy tick crosses 3,333,333
nanoseconds, which cannot be represented by the DATETIME2 100ns carrier. Exact
decimal tokens are bound without an intermediate floating-point conversion.
The native regression covers 6,000 rows, NULL validity and one evaluation of
each numeric operand per row.

The standalone client replay is `node --test tests/datediff_numeric.test.mjs`.
All 37 non-version cases match SQL Server rows and error numbers; two match
complete captures. The raw replay in `artifacts/compatibility/datediff-numeric-replay.json`
retains every difference. The remaining 35 cases expose shared result-column
flags (`1` rather than SQL Server's `33`); the three overflow cases additionally
have native wrapper prefixes and state `1` rather than SQL Server state `2`
(numeric conversion) or `0` (DATEDIFF overflow). These are explicit metadata
and diagnostic gaps, not full compatibility passes. The reference image is
SQL Server `17.0.4065.4` at the pinned digest recorded in the fixture.

Open work includes complete legacy DATETIME/SMALLDATETIME coercion,
non-ISO/dateformat-sensitive text, broader mixed-type precedence, and the
shared metadata/diagnostic differences above. DATETIMEOFFSET inputs use UTC
boundaries; their focused tests remain separate from this numeric capture.

Verification: 155 Rust tests and 243 client tests pass; formatting and Clippy are
clean. All 139 local audit cases have complete captures. The new case records
boundary counts 1/1/100, exact BIGINT 315537897599999999, NULL, Sunday/Monday
counts 1/0 under DATEFIRST 1, and caught error 535 with the expected integer widths.
