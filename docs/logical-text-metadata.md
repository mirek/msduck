# Logical expression text metadata

CASE, COALESCE, IIF and CHOOSE now combine known character result descriptors
before SQL translation. CHAR(1) branches retain CHAR(1); a SPACE/VARCHAR branch
selects VARCHAR with the maximum known width. An omitted ELSE or untyped NULL
branch preserves the other descriptor. NULLIF retains its first argument's
result descriptor independently of its comparison operand.

This follows the result precedence rules for
[CASE](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/case-transact-sql?view=sql-server-ver17)
and [COALESCE](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/coalesce-transact-sql?view=sql-server-ver17).
The pass is independent of runtime branch selection and composes with known
set-operation descriptors. It changes metadata selection, not evaluation count
or short-circuit behavior. The existing native/translated expressions still
produce the values.

Client tests cover searched/simple CASE, NULLIF returning NULL, omitted ELSE,
out-of-range CHOOSE, prepared parameters, empty results, nested logical functions,
set operations and unknown Unicode branches. Known sources currently mean CHAR,
SPACE and supported compositions. Unknown branches retain backend metadata.

Declared character variables, casts, literals, columns and stored objects still
need broader family/width inference. General ISNULL width truncation and fixed-width padding,
collation precedence, accurate nullability and live SQL Server differential
validation remain unfinished.

ISNULL now preserves inferred CHAR/SPACE widths and applies truncation/padding
for known text replacements; see [ISNULL coverage](isnull.md).
