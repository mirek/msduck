# Quoted operands in checked arithmetic

Delimited `[@p]` and `"@p"` are columns, while unquoted `@p` is a scalar
variable. The two matching 30-record [SQL Server captures](../reference/quoted-session-identifiers.json)
retain nullable INT column declarations, bound values 42/NULL, the stored column
value 7 and empty-result descriptors. They establish column identity independently
of a parameter with the same spelling.

The deterministic checked-expression APIs preserve that distinction:

- `checked_expression::plan` has only parameter declarations, so a quoted
  column operand remains unresolved even if the parameter map contains its name.
- `plan_in_scope` binds quoted operands through explicit row-source declarations.
  A BIGINT parameter named `@p` cannot override an INT column named `@p`.
  Unknown, ambiguous and nearer unknown row sources block fallback; genuinely
  unquoted parameters keep their explicit declarations across row-scope barriers.
- `checked_projection::plan` keeps quoted operands row-dependent. A projection
  such as `SELECT [@p]/0 FROM t WHERE 1=0` has no scalar prechecks. The otherwise
  identical unquoted parameter expression retains the existing precheck policy.

Original leaf ASTs are inserted once into materialized plans, retaining delimiters
and qualification. Neither declaration selection nor empty-source classification
reads values or evaluates expressions. Parentheses do not change column identity.

This reuses the upstream distinction between variable and column AST cases in
[mirek/mssqlite's expression translator](https://github.com/mirek/mssqlite/blob/7f71f2081602f8e3051998f5c11f058e65fe24ec/packages/transpile/src/expression.ts#L346-L349):
variables bind through `Context.parameter`, while columns bind through
`Context.columnExpression` and quoted column names. The Rust implementation
uses the existing parsed identifier delimiter and explicit declaration scopes;
SQLite-specific code is not copied.

The focused tests also use conflicting caller-supplied BIGINT parameter and INT
column declarations to expose accidental type selection. That deterministic
binding test is separate from a SQL Server arithmetic capture; the retained
fixture proves name identity and does not establish wider arithmetic behavior.

Two gaps remain outside this task. `expression_metadata::storage::kind` still
consults the parameter map before its column callback for quoted `@p`; that file
is reserved by other workers. Root preflight, session substitution, parameter
binding and wire descriptor/error behavior are tracked by the separate quoted
session tasks. Pure checked planning does not establish a corrected server replay
or full SQL Server compatibility.
