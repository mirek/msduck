# NVARCHAR cast coverage

Explicit CAST, TRY_CAST and style-free CONVERT/TRY_CONVERT to NVARCHAR(n)
now enforce n in 1–4000 UTF-16 units. Omitted cast lengths use 30;
NVARCHAR(MAX) keeps the existing unbounded text path. Text truncates without
padding. Supplementary characters count as two units. Numeric text that cannot
fit raises arithmetic overflow (8115); TRY variants return NULL.

The owner-run `reference/decimal-unicode.json` retains nine probes in each of
four fresh databases across two pinned SQL Server 2025 containers. All four
raw runs and an independent replay agreed. DECIMAL(38,38) converts with a
leading zero for NVARCHAR and NCHAR, including `0.000…01` and `-0.000…01`;
NCHAR pads after conversion. The corresponding descriptors are `NVarChar` or
`NChar`, with declared widths in bytes. A too-narrow conversion raises
8115/state 2/class 16 with `nvarchar` in the diagnostic even for NCHAR;
TRY conversion returns typed NULL. A float `0.5` and text `.25` retain their
distinct formatting. The adapter now restores a missing decimal leading zero
before width checks, without changing those other source families.

Known explicit cast projections retain bounded NVARCHAR metadata, including
NULL and empty results. Known conditional expressions combine widths, and
ISNULL uses the first argument's width. Client tests cover prepared execution,
Unicode boundaries, invalid lengths, MAX values and recovery after errors.
Native tests exercise mixed NULL/numeric batches and single evaluation across
6,000 rows, plus captured DECIMAL(38,38) values, width failures and decimal
single evaluation. A root session test runs the retained DECIMAL AVG query
through T-SQL lowering. The local audit records an explicit cast probe for
future comparison but does not compare it with SQL Server.

This is not complete NVARCHAR compatibility. Truncation inside a surrogate pair
is explicitly unsupported because the current text representation cannot hold
lone surrogates. Collation-specific behavior, stored-column and variable width
propagation, styled conversions, remaining source-type formatting (including
float and temporal formatting), and complete descriptor inference remain open.
The retained decimal probes establish only their captured conversion forms;
they do not prove general Unicode character compatibility.

References: [nchar and nvarchar](https://learn.microsoft.com/en-us/sql/t-sql/data-types/nchar-and-nvarchar-transact-sql?view=sql-server-ver17),
[CAST and CONVERT](https://learn.microsoft.com/en-us/sql/t-sql/functions/cast-and-convert-transact-sql?view=sql-server-ver17).
