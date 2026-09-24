# DATEPART

DATEPART returns INT for year, quarter, month, day-of-year, day, week, weekday,
hour, minute, second, millisecond, microsecond, nanosecond, tzoffset and ISO week,
including the documented keyword abbreviations. It validates argument shape and
the datepart keyword before lowering to a native scalar function.

The ordinary input path converts through the exact DATETIME2 codec, preserving year
1–9999 and all seven fractional digits. Fractional fields describe the fraction
within the second; nanoseconds are always multiples of 100. DATETIME2 tzoffset
is zero. NULL and empty results retain INT metadata, and integer expression
inference recognizes DATEPART. Invalid ISO input reports conversion error 241. Time-only text supplies the
1900-01-01 calendar fields while retaining exact clock fractions.

Week and weekday read the connection's DATEFIRST setting at execution time.
SET DATEFIRST accepts values 1–7 and local variables; @@DATEFIRST returns TINYINT.
Prepared statements and stored views see subsequent setting changes. Connections
are isolated, invalid values preserve the prior setting, preparation is inert,
and transaction rollback does not undo the setting. SET LANGUAGE US_ENGLISH
resets it to 7. ISO week is independent of DATEFIRST. A native test compares every date in a 400-year cycle
with DuckDB's ISO week calculation, checks NULL/fraction validity across 6,000
rows, and verifies one input evaluation per row. Tedious tests cover every
keyword alias, scale rounding, range endpoints, week/year boundaries, prepared
reuse after conversion errors, and typed empty results.

Complete string formats and language behavior,
fractional numeric date conversion, BIT inputs, and broader diagnostic
fidelity remain unfinished.
This is not complete DATEPART compatibility; live SQL Server comparison remains
outstanding.

The upstream `date-part.ts` and `functions.ts` normalize keywords before calling
`mssqlite_datepart`; its inference modules also assign an INT result. The Rust
implementation uses the exact tick representation and additionally handles
ISO week aliases from Microsoft's
[DATEPART reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/datepart-transact-sql).

DATEFIRST behavior follows Microsoft's [SET DATEFIRST](https://learn.microsoft.com/en-us/sql/t-sql/statements/set-datefirst-transact-sql)
and [@@DATEFIRST](https://learn.microsoft.com/en-us/sql/t-sql/functions/datefirst-transact-sql)
references. The inspected upstream global binder returns a fixed 7 and its
week helpers assume Sunday; msduck now reads connection-local state instead.
Exact setting-error numbers and broader language defaults remain open.

DATEPART checks the original bound type before DATETIME2 conversion. Calendar
fields on typed TIME, clock fields on typed DATE, and tzoffset on DATE/TIME or
legacy datetime raise error 9810. Explicit conversion to DATETIME2 supplies its
fields; time-only and date-only string literals retain their documented defaults.
Tests cover source columns, local variables, typed NULLs, RPC values, TRY/CATCH,
valid prepared requests after errors, and single input evaluation. Error timing
for optimized-away or empty queries still needs live SQL Server comparison.

Integer TINYINT/SMALLINT/INT/BIGINT inputs use legacy DATETIME day offsets from
1900-01-01, sharing the checked YEAR/MONTH/DAY conversion. Values outside
1753-01-01 through 9999-12-31 raise 8115. Clock fields are zero for these midnight
values, and week fields follow DATEFIRST; tzoffset remains unsupported for legacy
datetime inputs. Tests cover all four widths, NULL/empty metadata, source columns,
range endpoints, overflow, prepared reuse and one evaluation per row.
The upstream UDF routes DATEPART values through text conversion; this path keeps
integer day-offset conversion explicit before extracting parts.

Typed DATETIMEOFFSET inputs at every scale use retained local calendar and clock
fields, including when UTC falls on another day or year. tzoffset returns signed
minutes, including negative subhour offsets. DATEFIRST applies to these local
fields. Client checks cover the documented +05:10 example, ±14-hour offsets,
scale rounding, source columns, prepared casts and NULL/empty metadata. Native
checks cover all scales across 6,000 rows and one input evaluation per row.
