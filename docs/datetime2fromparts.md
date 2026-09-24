# DATETIME2FROMPARTS

`DATETIME2FROMPARTS(year, month, day, hour, minute, seconds, fractions, precision)`
constructs DATETIME2(precision), for precision 0–7. Fractions count units of
10^-precision seconds. The date must exist in the Gregorian calendar within
years 1–9999. Hours are 0–23; minutes and seconds are 0–59; the fraction must be
nonnegative and smaller than 10^precision.

The first seven arguments use shared SQL integer conversion. The native function
checks all component NULLs before validating ranges, then combines checked DATE
days with exact TIME ticks. It returns the existing tagged DATETIME2 storage
shape, preserving scale through wire output, comparisons, storage, views and
composed queries. Invalid component values report error 289.

Precision is resolved before execution. Integer literals, parentheses, unary
signs, arithmetic and bitwise integer constant expressions are supported.
NULL, parameter-dependent and unsupported precision expressions are rejected
with error 10760. Checked evaluation prevents integer overflow or division by
zero from escaping into the compiler. Exact error precedence for those invalid
expressions and out-of-range scales needs live SQL Server verification.

Tests cover all scales over 6000-row vectors, calendar/fraction bounds, parent
and child NULLs, one evaluation of each input, prepared calls, default/storage
rounding, views, DATEADD/TODATETIMEOFFSET composition and empty metadata. The
local audit retains 100ns fractions separately from JavaScript milliseconds.

Remaining work includes live SQL Server captures, additional constant-expression
forms (such as casts), precise error state/precedence and wider coercion parity.
Related upstream DATEFROMPARTS/DATETIMEFROMPARTS tests informed calendar and NULL
coverage; no DATETIME2FROMPARTS implementation was found in the inspected packages.

References:

- [Microsoft DATETIME2FROMPARTS reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/datetime2fromparts-transact-sql)
- [Microsoft error 10760 catalog](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-10000-to-10999)
- [Microsoft error 289 catalog](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-0-to-999)
