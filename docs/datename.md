# DATENAME

DATENAME returns Unicode text for calendar and clock parts, using the documented
keyword aliases. Month and weekday names use the currently supported US English
language. Numeric fields retain exact DATETIME2 fractions, including nanoseconds
in multiples of 100. Week numbers follow session DATEFIRST; ISO weeks use ISO
numbering and weekday names describe the actual calendar day.

The shared temporal input path preserves typed DATE/TIME restrictions, string
literal default fields, integer day offsets, range checks and NULLs. Missing
fields on typed inputs raise 9810 with DATENAME in the message. An input without
a timezone yields `0` for tzoffset. Typed DATETIMEOFFSET uses local fields and returns a signed `HH:MM` offset,
including `-00:30` and `+00:00`.
Known DATENAME results also participate in integer/character type precedence.

Tedious tests cover every keyword alias, all twelve month names, DATEFIRST 1–7,
exact fractional values, time-only strings, integer inputs, source columns,
NULL/empty result families, typed errors and prepared recovery. A sequence test
verifies one input evaluation per row across 6,000 rows.

Known DATENAME projections now use NVARCHAR(30), following the upstream type
inference record, with a 60-byte TDS limit and bounded UTF-16 value framing.
ISNULL also truncates replacements to the first DATENAME result width in
UTF-16 units (surrogate-splitting truncation remains unsupported).
The width survives typed NULL/empty results, known conditional expressions and
set operations. Width propagation through arbitrary functions, source columns
and stored views still needs broader descriptor inference and live validation.
Other languages, complete
string formats, fractional numeric dates, legacy smalldatetime fidelity and live
SQL Server comparison also remain unfinished.

Microsoft's [DATENAME reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/datename-transact-sql)
provides the alias, return-family and missing-field rules. The inspected upstream
`date-functions.ts` likewise derives English names from civil date parts and
uses DATEPART for numeric text. The msduck implementation retains its exact
DATETIME2 conversion and connection-local DATEFIRST behavior.

DATETIMEOFFSET shares exact scale conversion with DATEPART. Month and weekday
names describe the local date even across a UTC date boundary. Native chunk tests
verify NULL handling and single input evaluation for month and offset names;
client checks include prepared casts and bounded empty result metadata.
