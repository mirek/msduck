# NCHAR coverage

NCHAR uses the current non-SC collation model: its integer argument selects one
UTF-16 unit, with NULL for NULL inputs and values outside 0–65535. Input conversion
uses the shared INT path, including fractional truncation, invalid text errors
and integer overflow. BMP scalar values include U+0000 and U+FFFF.

Known projections return fixed NCHAR(1) metadata (TDS 0xEF, two bytes), including
NULL and empty results. Known conditional and set expressions retain this type;
mixing a known NVARCHAR result promotes the descriptor to NVARCHAR. ISNULL
replacements truncate to the first width and pad short values with spaces.

This follows [Microsoft's NCHAR reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/nchar-transact-sql?view=sql-server-ver17)
and the inspected upstream mssqlite `udf.ts` and character inference. Unlike
upstream's UTF-16 storage bridge, the current Rust/Arrow text storage cannot hold
isolated surrogates: arguments 55296–57343 raise an explicit unsupported error.
The upstream `server.test.ts` case "UTF-16 code-unit string semantics cross the
tedious boundary" explicitly expects a lone high surrogate from NCHAR(55357);
that remains a known gap here, not a passing compatibility case.
SC collations, declared NCHAR columns/variables, broader descriptor propagation,
exact error messages and live SQL Server differential validation remain unfinished.
Out-of-range integer text now reports 248 through the shared INT converter.

Tests cover all non-surrogate BMP values through UNICODE roundtrips, vectorized
NULL/range behavior, exact wire bytes, integer conversions, prepared recovery,
logical expressions, padding and empty metadata. The local audit records a probe
for future SQL Server comparison; execution alone does not establish equivalence.

Explicit CAST/TRY_CAST and style-free CONVERT/TRY_CONVERT now accept NCHAR(n),
with widths 1–4000 and default cast length 30. They truncate text by UTF-16 units,
then pad with ASCII spaces to the exact width. Numeric output that cannot fit
raises 8115; TRY variants return NULL. NULL and empty result projections retain
the declared fixed width. The shared Unicode converter still supplies backend
numeric formatting and its NVARCHAR wording for numeric overflow errors.

Client checks cover prepared values, supplementary characters, invalid widths,
ISNULL and known conditional results. Native tests verify NULLs, truncation and
UTF-16 padding across 6,000 rows. Stored-column widths, styled conversions and full source-type formatting remain open.

CASE, COALESCE, IIF and CHOOSE now combine known NCHAR branches at the largest
width and pad the selected result to that width. Known nested expressions and
ISNULL fallbacks preserve this width, including empty/NULL metadata. Conditions
and indexes are not wrapped. A native sequence test crosses 6,000 rows and
checks that only the selected branch is evaluated, once per applicable row.
This follows the inspected upstream `character.ts::preferred` maximum-width
rule; live SQL Server comparison is still pending. Unknown source-column widths
and mixtures with other character families require broader type inference.

UNION, UNION ALL, INTERSECT and EXCEPT now pad known NCHAR operands to the
largest width before comparison. This preserves duplicate/NULL behavior, named
outputs, nested set trees and branch TOP limits. The pass runs before DATETIME2
set normalization so mixed result columns retain both conversions. Tests cover
prepared values, empty metadata, mixed temporal columns and 12,000 input rows.
This implements the documented [set-result length rule](https://learn.microsoft.com/en-us/sql/t-sql/data-types/precision-scale-and-length-transact-sql?view=sql-server-ver17).
Known widths here come from explicit projections and their supported expressions;
wildcards, stored columns, CTE/derived-column width inference and complete
collation comparison still need work. Live SQL Server comparison is pending.
