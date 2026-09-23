# TIME RPC parameters

TIME (0x29) inputs validate scale 0–7, the corresponding 3/4/5-byte width,
and a value strictly below 24 hours. NULL uses zero length. The unsigned
little-endian count is converted with integer arithmetic to exact text with
seven fractional digits, then bound and cast to DuckDB TIME_NS. This avoids
microsecond truncation in duckdb-rs's Time64 parameter binder.

T-SQL TIME declarations currently lower to TIME_NS. Results read Arrow arrays
directly because duckdb-rs 1.10505.0's Row reader panics on nanosecond TIME
arrays. The direct reader covers all currently advertised result types and
returns errors for unexpected array representations. TIME results emit all
seven fractional digits using the existing TDS scale-7 encoder.

Unit tests cover every input scale, midnight, the smallest unit, the last unit
before midnight, NULL, invalid scales/lengths, out-of-range counts, and all
truncated prefixes. The independent tedious test verifies 23:59:59.9999999
through direct selection, table insertion/readback and prepared execution;
it also covers every wire scale and NULL metadata. Tedious uses differently
named properties for fractional input (`nanosecondDelta`) and output
(`nanosecondsDelta`), which the test checks explicitly.

Explicit TIME casts and RPC parameter declarations retain the requested scale
through translation. A session-local DuckDB macro rounds integer nanoseconds
to the requested unit and wraps a carry at midnight. Cast operands remain AST
nodes and are evaluated once; bound parameters are not interpolated into SQL.
The conversion preserves NULL and nested-cast ordering. Invalid scales are
rejected for casts, RPC declarations and CREATE TABLE columns.

Independent driver tests cover all eight scales, values below and at a rounding
tie, midnight carry, nested casts, NULL, RPC declaration conversion, prepared
binding order, invalid scales and connection reuse after errors. Rounding to
lower precision follows the Microsoft TIME conversion example:
[time (Transact-SQL)](https://learn.microsoft.com/en-us/sql/t-sql/data-types/time-transact-sql).
The midnight carry expectation also follows the upstream temporal codec;
a real SQL Server differential run is still required.

Remaining differences: result metadata always reports scale 7, and lower-scale
column assignments do not yet enforce their declared scale. Declared column
scales need to survive in a persistent compatibility catalog.
DATETIME2 now has an exact representation. CAST/TRY_CAST and style-free
CONVERT/TRY_CONVERT to TIME extract its time component with integer arithmetic
before rounding to the requested scale. The date is discarded before midnight
carry, including at year 9999. Client tests cover every target scale, range
endpoints, NULL/empty results, prepared queries, RPC and TRY failure recovery.
Native tests cover all source scales, chunk boundaries and single evaluation.
DATETIMEOFFSET conversion remains unfinished. Arrow batch fetch
errors can still panic inside the upstream iterator; general fetch-error
containment and streaming remain separate work.

References: copied `.agents/skills/tds-protocol/data-types.md` and the installed
tedious TIME codec; [DuckDB TIME types](https://duckdb.org/docs/lts/sql/data_types/time).
