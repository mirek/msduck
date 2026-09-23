# NVARCHAR cast coverage

Explicit CAST, TRY_CAST and style-free CONVERT/TRY_CONVERT to NVARCHAR(n)
now enforce n in 1–4000 UTF-16 units. Omitted cast lengths use 30;
NVARCHAR(MAX) keeps the existing unbounded text path. Text truncates without
padding. Supplementary characters count as two units. Numeric text that cannot
fit raises arithmetic overflow (8115); TRY variants return NULL.

Known explicit cast projections retain bounded NVARCHAR metadata, including
NULL and empty results. Known conditional expressions combine widths, and
ISNULL uses the first argument's width. Client tests cover prepared execution,
Unicode boundaries, invalid lengths, MAX values and recovery after errors.
Native tests exercise mixed NULL/numeric batches and single evaluation across
6,000 rows. The local audit records an explicit cast probe for future comparison.

This is not complete NVARCHAR compatibility. Truncation inside a surrogate pair
is explicitly unsupported because the current text representation cannot hold
lone surrogates. Collation-specific behavior, stored-column and variable width
propagation, styled conversions, all source-type formatting (including numeric
and temporal formatting), and complete descriptor inference remain open. Numeric
formatting currently comes from DuckDB before the width check. No live SQL Server
differential run has verified this increment.

References: [nchar and nvarchar](https://learn.microsoft.com/en-us/sql/t-sql/data-types/nchar-and-nvarchar-transact-sql?view=sql-server-ver17),
[CAST and CONVERT](https://learn.microsoft.com/en-us/sql/t-sql/functions/cast-and-convert-transact-sql?view=sql-server-ver17).
