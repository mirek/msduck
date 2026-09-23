# SPACE

SPACE converts its count to INT using the shared SQL Server conversion path.
Zero returns an empty string, negative and NULL counts return NULL, and positive
counts return up to 8,000 ASCII spaces. Numeric fractions truncate; invalid
integer text reports 245 and out-of-range numeric counts report 8115.

The native function evaluates one count per row and uses an 8,000-byte buffer
per chunk, so even INT_MAX cannot trigger an unbounded allocation. It supports
parameters, source columns and stored defaults. Known SPACE results participate
in character concatenation and integer/character comparison and arithmetic
conversion. Shared scalar-argument validation is also used by UNICODE.

Behavior follows Microsoft's [SPACE reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/space-transact-sql?view=sql-server-ver17).
Client tests cover negative/NULL/fractional counts, integer text, the cap,
prepared reuse after conversion errors, columns, defaults and nested functions.
A native test exercises 10,000 rows across vector chunks and inline/heap output.

Direct SPACE projections now use bounded VARCHAR metadata: literal integer
counts determine the width (capped at 8,000), and other counts use 8,000. This
matches the inspected mssqlite expression descriptor implementation in
`packages/transpile/src/implicit.ts`. The descriptor survives aliases, parentheses,
prepared execution, NULLs and empty result sets. TDS uses Windows-1252 bytes,
two-byte lengths and 0xffff NULL markers instead of NVARCHAR(MAX)'s PLP framing.

Result hints are captured from the original outer SELECT before translation.
Known CHAR/SPACE descriptors combine through UNION, INTERSECT and EXCEPT,
using the widest VARCHAR width and preserving equal CHAR widths. Untyped NULL
branches preserve the other descriptor. Unknown branches and wildcards retain
the existing fallback. Stored views,
source-column propagation, nested text expressions, complex constant folding
and full VARCHAR width inference remain unfinished. Full implicit conversion,
exact diagnostic parity, collation support and live SQL Server differential
validation also remain outstanding.
