# Percentile windows

PERCENTILE_CONT and PERCENTILE_DISC translate ordered-set syntax to DuckDB
quantile windows. They require one numeric literal between zero and one, one
WITHIN GROUP ordering expression, and OVER with optional partitioning only.
Named windows are resolved before validation. Fractions are range-checked using
decimal digits and exponents rather than a rounded f64 comparison. Duplicate
modifiers, extra sort expressions, OVER ordering, explicit frames and nonliteral
fractions fail explicitly. Existing nesting and placement validation applies.

CONT uses a native binder to reject nonnumeric input types before execution,
including typed NULLs, empty tables and prepared parameters. Its scalar callback
converts accepted numeric vectors to DOUBLE before interpolation, producing SQL FLOAT(53)
rather than interpolating into the input decimal scale. DISC preserves the
selected input value and its backend type. Known integer DISC expressions also
participate in shared source/result inference for mixed arithmetic and outer
aggregates. Descending quantiles use negative fractions in DuckDB; descending
zero uses a maximum window because negative zero does not reverse its order.
NULL values are ignored, and all-NULL partitions produce typed NULLs.

Tedious tests cover even medians, interpolation, descending endpoints, named
partitions, NULLs, decimal interpolation, exact BIGINT results, TINYINT/SMALLINT
metadata, character selection, empty metadata, CTE aggregates and prepared
parameters. Additional native tests compare every supported numeric layout
and decimal storage width with DuckDB casts across chunks, including NULLs,
negative decimals and 128-bit decimals. A sequence verifies single evaluation.
The audit captures msduck values and descriptors for comparison. A separate
[live SQL Server 2025 reference](percentile-reference.md) records integer,
decimal, FLOAT and BIT interpolation, discrete text/date selection, NULLs,
partitions, descending order, invalid fractions and prepared bindings. It
includes the observed FLOAT interpolation value `0.15000000000000002` for
inputs 0.1 and 0.2. Those reference observations have not established general
msduck parity. No corresponding implementation was found in the inspected
mssqlite source.

Remaining gaps include exact character widths/collations,
all sortable DISC types, signed literal edge cases, compatibility-level gating,
precise diagnostics for other rejected percentile forms and broader reference
verification of floating-point interpolation across types and values. Missing
OVER/WITHIN GROUP and forbidden frames use 10753/10754/4106; several other
invalid forms currently use the generic unsupported-operation diagnostic. A
complete live msduck-versus-SQL Server
comparison has not been run.

Literal fractions below zero, above one, or NULL now use the captured error
8727, state 1 and severity 16. This is still a pre-execution lowering error;
SQL Server emits a typed result descriptor and ORDER token before reporting
8727 for the captured nonempty queries. That completion sequence remains a
compatibility gap. Character fractions such as the reference's `'0.5'` remain
unsupported by the current lowerer. Signed zero is accepted as an in-range
literal in both percentile functions.

References: Microsoft [PERCENTILE_CONT](https://learn.microsoft.com/en-us/sql/t-sql/functions/percentile-cont-transact-sql)
and [PERCENTILE_DISC](https://learn.microsoft.com/en-us/sql/t-sql/functions/percentile-disc-transact-sql).

Explicit DATETIME2 casts now use an exact tagged representation and are rejected
as nonnumeric by the percentile binder alongside DATETIME inputs.
Numeric input rejection currently carries DuckDB's binder-message wrapper and
the server's generic error 50000; SQL Server error-number/message parity is not
yet established for this case.
