# LEN and hexadecimal literals

LEN removes trailing ASCII spaces and counts UTF-16 code units for Unicode
text, matching non-SC collation behavior. Tabs and nonbreaking spaces remain.
NULL propagates; numeric inputs use the current backend text conversion.
Binary inputs count bytes after removing trailing 0x20 bytes. Hexadecimal SQL
literals are translated through from_hex so they become binary values rather
than DuckDB's textual interpretation of the rendered X-literal.

Known bounded arguments return INT, while MAX variables, parameters, casts and
known MAX concatenations return BIGINT. The interpreter retains VARCHAR,
NVARCHAR and VARBINARY declaration lengths until expression translation so
bound values do not lose this distinction. Typed NULLs follow the same rules.
Persisted column types still lose declared lengths in DuckDB; LEN of a MAX
column or an expression whose MAX type is not inferred can therefore have
incorrect INT metadata. Full schema/type inference remains required.

The copied mssqlite LEN implementation supplied UTF-16 and trailing-space test
cases. Tedious tests cover Unicode surrogate pairs, spaces/tabs/NBSP, NULL,
numbers, binary parameters/literals, stored values, MAX metadata, malformed
calls and recovery. Tiberius independently verifies INT/BIGINT SQL-batch results.
Native Rust scalar functions count UTF-16 units or binary bytes. A thin macro
selects the function from the bound input type and evaluates the argument once.
The wrapper lives in the shared database catalog so persisted defaults can
resolve it from other connections; native functions are registered at startup.
A volatile-expression regression checks that an empty string or a surrogate
pair can only produce lengths 0 or 2, never a mixed-evaluation length of 1.
LEN works in stored defaults, including after a database restart. Tests cover
inline and heap-backed strings, embedded NULs, arbitrary binary bytes and NULL
rows across native vector boundaries. The native functions replace a lambda
expansion that DuckDB rejected in DEFAULT expressions.

Remaining work includes SC/collation-aware character counting, SQL Server
code-page conversion, exact implicit conversion formatting, full MAX inference,
hex literal edge-case diagnostics and live SQL Server differential validation.

Reference: [Microsoft LEN](https://learn.microsoft.com/en-us/sql/t-sql/functions/len-transact-sql).
