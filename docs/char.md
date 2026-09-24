# CHAR

CHAR converts its argument to INT, then interprets codes 0 through 255 using
the server's current Windows-1252 character set. Negative, out-of-range and NULL
codes return NULL. Numeric fractions truncate during INT conversion; invalid
integer text reports 245 and numeric INT overflow reports 8115.

The native function supports source columns, bound parameters and stored defaults,
including NUL and control characters. Known CHAR expressions participate in string
concatenation and integer/character conversion. Direct outer SELECT projections
carry CHAR(1) metadata with code-page bytes and two-byte TDS value lengths/NULL
markers, including prepared, all-NULL and empty results.

Microsoft's [CHAR reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/char-transact-sql?view=sql-server-ver17)
specifies char(1). This differs from the inspected mssqlite inference's varchar(1)
descriptor; the implementation follows Microsoft's declared type, with live TDS
differential verification still required.

Tests cover ASCII, extended Windows-1252 characters, controls, range limits,
NULLs, prepared reuse after errors, defaults, columns, result metadata and nested
expressions. A native test covers every byte and invalid codes across 6,000 rows.

Other code pages, full implicit conversion (including binary inputs), stored
object/wildcard/general set-operation descriptor propagation and exact diagnostic parity
remain unfinished. This does not implement NCHAR or ASCII semantics.

Known CHAR and SPACE branches now combine their result descriptors through
UNION, INTERSECT and EXCEPT. A VARCHAR branch determines the common family,
with the maximum known width. Equal CHAR widths remain CHAR; NULL branches
preserve the other descriptor. Unknown branch types retain backend metadata.
These rules follow [set-operation type combination](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/set-operators-union-transact-sql?view=sql-server-ver17)
and [result length rules](https://learn.microsoft.com/en-us/sql/t-sql/data-types/precision-scale-and-length-transact-sql?view=sql-server-ver17).
This change does not implement string-padding comparisons, full nullability,
collation precedence or the complete implicit conversion matrix.
