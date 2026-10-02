# Quoted columns in storage declarations

Shared storage inference uses scalar parameter declarations only for unquoted
identifiers. A delimited column such as `[@p]` or `"@p"` uses the explicit
column callback, including when a same-named parameter has a conflicting type.
If the column declaration is unknown, it remains unknown.

Regressions derive INT, SMALLINT and NVARCHAR(20) column declarations from both
matching SQL Server captures in `reference/quoted-session-identifiers.json`.
They cover conflicting BIGINT parameters with 42/NULL values, qualified and
parenthesized names, unary and character aggregate consumers, unknown column
declarations and immutable inputs. DATALENGTH lowering retains the original
column operand and uses its four-byte INT width rather than the parameter's
eight-byte BIGINT width. Unquoted scalar binding keeps its existing behavior.

This follows the variable/column AST distinction in owner-pinned mssqlite
[`expression.ts`](https://github.com/mirek/mssqlite/blob/7f71f2081602f8e3051998f5c11f058e65fe24ec/packages/transpile/src/expression.ts).
Its SQLite rendering is not copied into the Rust/DuckDB adapters.

The rule depends only on explicit declarations and does not evaluate operands,
inspect rows or consult sessions. This is a compiler declaration correction;
root substitution, catalog binding and the recorded quoted-identifier wire
differences remain separate work. Full exact-head workspace/client/audit checks
and review are required before merge.
