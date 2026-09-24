# DATETIMEOFFSETFROMPARTS

The ten-argument constructor accepts local date/time fields, fractional units,
signed offset hours/minutes and precision. It returns the existing exact
DATETIMEOFFSET storage shape: UTC ticks, signed offset minutes and declared
scale 0–7. Fractions count units of 10^-precision seconds.

Calendar fields use the DATETIME2 constructor's checked day/tick calculation.
Offset hours must be between -14 and 14, minutes between -59 and 59. Nonzero
hours and minutes must have matching signs; ±14 hours requires zero minutes.
Zero hours permit negative minutes, preserving offsets such as -00:30.
Construction validates both local and derived UTC values against years 1–9999.
Invalid component values report 289. NULL runtime components produce a typed
NULL with both native children marked NULL.

Precision uses shared nonexecuting constant-expression evaluation; unsupported
expressions report 10760. All runtime components use SQL integer conversion
before the native constructor. No timestamp or floating-point intermediate is
used in assembling the temporal value.

Tests cover all scales and 1681 valid offsets, 6000-row chunks with independent
NULLs, one evaluation of each of the nine runtime inputs, mixed signs, boundary
overflow, prepared calls, defaults/storage, views, nested temporal operations,
set queries and empty result metadata. The audit retains exact fractions and
checks offset text separately because the client's Date object exposes UTC.

Live SQL Server comparison remains open, including NULL-offset/error precedence,
exact diagnostic states, broader component coercion and additional precision
expression forms. The Microsoft page discusses omitted offset arguments despite
its ten-argument syntax; alternate arity is not implemented without reference
evidence. No implementation was found in the inspected upstream packages.

References:

- [Microsoft DATETIMEOFFSETFROMPARTS reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/datetimeoffsetfromparts-transact-sql)
- [Microsoft DATETIMEOFFSET range](https://learn.microsoft.com/en-us/sql/t-sql/data-types/datetimeoffset-transact-sql)
