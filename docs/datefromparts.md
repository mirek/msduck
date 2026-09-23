# DATEFROMPARTS

DATEFROMPARTS constructs a DATE from INT year, month and day expressions.
It propagates NULL and validates the Gregorian calendar, including century
leap-year rules and the SQL Server year range 1 through 9999. Invalid calendar
parts report error 289, state 1, and can be handled by TRY/CATCH.

A registered Rust scalar function receives each evaluated argument once.
It returns DuckDB's native DATE type, preserving DATE wire metadata for NULL
and empty results. Registration belongs to the database owner so cloned client
connections share it. Stored defaults continue to work after reopening the
file and registering the function again.

The mssqlite constructor tests supplied leap-day, NULL, and error cases.
Independent tedious coverage checks bounds, metadata, prepared-query recovery,
defaults, malformed calls and connection reuse. Tiberius checks DATE metadata;
a restart test checks a persisted default. A native vector test compares every
valid day from 0001-01-01 through 9999-12-31 with DuckDB's calendar.

Arguments now use the shared integer conversion path, including BIGINT inputs
and truncation of decimal/float fractions. Full implicit-conversion semantics
and SQL Server overflow diagnostics remain incomplete; see
[integer conversion](integer-conversion.md).
Error message prefixes and live SQL Server differential validation also remain
unfinished. Other FROMPARTS constructors are not implemented by this change.

Reference: [Microsoft DATEFROMPARTS](https://learn.microsoft.com/en-us/sql/t-sql/functions/datefromparts-transact-sql).
