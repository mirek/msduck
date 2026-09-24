# LTRIM, RTRIM and TRIM

Default trimming explicitly removes ASCII space (U+0020). DuckDB's default
trimming also removed nonbreaking spaces in a local probe, so relying on its
default changed SQL Server results. The AST translation now supplies a literal
space for one-argument LTRIM/RTRIM and TRIM without a character specification.
Tabs, nonbreaking spaces and other Unicode characters remain intact.

Explicit character sets and TRIM LEADING/TRAILING/BOTH use the backend's trim
operations. Known MAX character-set arguments are rejected; NULL and empty
inputs preserve their normal result behavior. The copied mssqlite transpiler
maps trim functions to SQLite counterparts; msduck needs this explicit-space
adaptation for DuckDB.

Tedious tests cover default and explicit sets, supplementary characters,
NULL/empty values, stored inputs, nested calls, prepared reuse and invalid
arguments. Tiberius verifies nonbreaking-space preservation independently.

Remaining work includes compatibility-level gating for SQL Server 2022 syntax,
full argument/result type inference, VARCHAR versus NVARCHAR result metadata,
collation-dependent character matching, binary/numeric implicit conversion,
and SQL Server differential validation. MAX rejection currently depends on
known expression types and does not cover all column/function-derived types.

References: [TRIM](https://learn.microsoft.com/en-us/sql/t-sql/functions/trim-transact-sql),
[LTRIM](https://learn.microsoft.com/en-us/sql/t-sql/functions/ltrim-transact-sql),
[RTRIM](https://learn.microsoft.com/en-us/sql/t-sql/functions/rtrim-transact-sql).
