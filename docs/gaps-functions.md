# User-defined functions

msduck supports T-SQL scalar functions, inline table-valued functions and
multi-statement table-valued functions:

```sql
CREATE FUNCTION dbo.foo(@x int) RETURNS int AS BEGIN RETURN @x+1; END;
SELECT id, dbo.foo(id) FROM dbo.t WHERE dbo.foo(id) > 2 ORDER BY dbo.foo(id);

CREATE FUNCTION dbo.rows_for(@id int) RETURNS TABLE AS
RETURN SELECT id, v FROM dbo.t WHERE id <= @id;
SELECT t.id, r.v FROM dbo.t CROSS APPLY dbo.rows_for(t.id) r;

CREATE FUNCTION dbo.mt(@n int) RETURNS @r TABLE (i int NOT NULL, s varchar(5))
AS BEGIN INSERT @r VALUES (@n, 'a'); RETURN END;
SELECT * FROM dbo.mt(10);
```

Expected behavior comes from `reference/gaps-functions.json`, captured from
SQL Server 2022 (16.0.4236) by `scripts/capture-gaps-functions.mjs`.
`tests/compat/functions.test.mjs` replays every captured case against msduck
through tedious, and `tests/gaps_functions.rs` checks values, the module store
and error numbers in Rust. The feature lives in
`crates/msduck-sql/src/dialect/ext/functions/` (syntax, compile checks,
folding and the interpreter) and `src/engine/ext/functions/` (definitions,
call expansion, dependencies), behind the extension hooks of
[extension-hooks.md](extension-hooks.md).

## Definitions

```
{ CREATE | ALTER | CREATE OR ALTER } FUNCTION [schema.]name
  ( [ @parameter [AS] type [ = default ] [ READONLY ] [, ...] ] )
RETURNS { type | TABLE | @variable TABLE ( column definitions ) }
[ WITH option [, ...] ]
[ AS ] { BEGIN statements END | RETURN [ ( ] query [ ) ] }

option: SCHEMABINDING | ENCRYPTION | NATIVE_COMPILATION
      | RETURNS NULL ON NULL INPUT | CALLED ON NULL INPUT
      | EXECUTE AS { CALLER | SELF | OWNER | 'user' } | INLINE = { ON | OFF }

DROP FUNCTION [ IF EXISTS ] [schema.]name [, ...]
```

- A definition must be the first statement of its batch. The batch hook takes
  the whole batch, so parameters and body variables are never checked as
  variables of the caller (the cause of the 137 and 40515 failures in
  v0.2.4). A definition later in a batch fails with 111 before anything runs.
  Through `sp_executesql` a definition is accepted without parameters; with
  parameters SQL Server compiles a parameterized batch and reports 156, and so
  does msduck.
- Definitions are stored in the module store (`main.__msduck_modules`) as
  `FN`, `IF` or `TF` with the original text, and appear in `sys.objects`,
  `OBJECT_ID` and `sys.all_objects`. The properties JSON uses the procedure
  format: `{"parameters":[{"name","type","output","default","readonly"}],
  "returns": type | "TABLE" | {"variable","columns":[{"name","type",
  "nullable"}]}, "options": {...}}`.
- Success sends DONE CurCmd 222; DROP FUNCTION sends CurCmd 179.
- Errors follow the capture: an existing name 2714 (state 3); ALTER of a
  missing function 208; ALTER or CREATE OR ALTER to another kind (scalar,
  inline, multi-statement) or of a table 2010; DROP of a missing function 3701
  (severity 11) unless IF EXISTS, of a table, view or procedure 3705; a
  missing schema 2760; a database prefix 166. A multi-name DROP removes the
  names before the first failure.
- Compile checks: undeclared variables 137, a final statement other than
  RETURN 455, SELECT that returns data 444, side effects (INSERT, UPDATE,
  DELETE, MERGE or TRUNCATE of a table, PRINT, RAISERROR, THROW, transaction
  statements, NEWID, RAND) 443, RETURN with a value in a table function 178,
  RETURN without one in a scalar function 1075, and body syntax 102.
- Definitions are transactional: a definition inside a transaction that rolls
  back disappears with it.

## Calls

Scalar functions are called with a schema-qualified name anywhere an
expression is allowed: select lists, WHERE, ORDER BY, GROUP BY, SET, DECLARE,
IF and WHILE conditions, RETURN, DML values, DEFAULT and computed column
definitions, other functions and any statement the engine executes for
another feature (procedure and trigger bodies run through the same path). Table-valued functions (one- or two-part names) work in
FROM, joins, subqueries, and CROSS and OUTER APPLY. Arguments bind by
position and convert to the parameter types; `DEFAULT` selects the
parameter's default, or NULL when it has none. Too many arguments fail with
8144 and too few with 313; an unknown function qualified with an existing
schema fails with 4121 (scalar) or 208 (table).

### Folding

A call is replaced in the calling statement by what its body computes, so the
backend evaluates it set-wise over many rows with the engine's ordinary
typing, checked arithmetic, conversion errors and result metadata:

- A scalar body runs symbolically: each variable is bound to the expression
  that computes its current value, `IF`/`ELSE` becomes `CASE`, and `RETURN`
  converts to the declared type. A variable's scope is the whole body, so a
  DECLARE in one branch is visible after it, and a DECLARE without a value
  does not reset the variable in a loop. `SELECT @v = expr FROM ...` takes
  the value of the last row in the query's ORDER BY (without ORDER BY, the
  first row, as the capture shows for a heap) and leaves the variable
  unchanged when there are no rows. `RETURNS NULL ON NULL INPUT` returns NULL when any
  argument is NULL.
- An inline function becomes a derived table of its query; a multi-statement
  function becomes the `UNION ALL` of the rows its `INSERT` statements add,
  each guarded by the conditions of the branches it is in, with omitted
  columns taking their DEFAULT or NULL.
- Arguments are substituted directly when that cannot change their meaning.
  An argument is evaluated once in a derived table the body reads when it
  references columns and the body reads tables (so the body's own tables can
  never capture a caller's column name, as in `CROSS APPLY dbo.f(id)` where
  the function also reads a table with an `id` column), or when the folded
  body uses it more than once and it is volatile (`RAND()`) or not a simple
  value (nested calls stay linear in size). Aggregate and window arguments
  belong to the calling query and are always substituted.
- Nested calls are folded in turn. Without recursion the nesting limit of 32
  applies to the folded calls too.

### Statement-by-statement execution

Folding cannot express `WHILE` loops or recursion. When every argument of
such a call is known before the calling statement runs (constants,
variables, parameters), the body runs statement by statement while the
statement is compiled: conditions are evaluated before a branch is chosen,
assigned variables are reduced to values, `BREAK` and `CONTINUE` work, and
the result becomes a literal of the return type (a multi-statement function's
rows become a literal row source). Recursion stops with 217 beyond 32 nesting
levels. All SQL Server data types used by function parameters round-trip
exactly, as the capture shows.

### Dependencies

Computed columns, DEFAULT and CHECK constraints keep the folded body, and so
do schema-bound functions. SQL Server refuses to ALTER or DROP a function
that such an object references (3729, naming the table or constraint), which
also keeps those stored expressions current. Schema-bound functions require
two-part table names (4512) and schema-bound callees (4513), and block DROP
TABLE or DROP VIEW of what they read (3729). References recorded for a
CREATE or ALTER TABLE that failed, and for dropped columns, are discarded.
Views that call a function, directly or through other functions, are
recreated from their source when the function is altered, so they use the
new body as on SQL Server. If a view cannot accept the new definition (for
example a changed parameter list), the ALTER is undone and the view's error
is reported; SQL Server would accept the ALTER and fail when the view is
queried.

## Remaining limits

- Loops and recursion with arguments that depend on rows
  (`SELECT dbo.fact(id) FROM t`) are refused with 40515 rather than
  evaluated per row.
- An error raised by a call that runs statement by statement arrives before
  the result's COLMETADATA (SQL Server sends the metadata first). A nesting
  error inside a folded body that is evaluated per row reports SQL Server's
  message with error number 50000.
- NOT NULL columns of a multi-statement function's return table are reported
  nullable, and the computed/updateable COLMETADATA flag bits of function
  results are not modeled.
- Table variables (including reads of the return table), cursors, `EXEC`,
  `UPDATE`/`DELETE` of the return table and accumulating assignments such as
  `SELECT @s = @s + col FROM t` are refused with 40515 when the function is
  called.
- Computed columns, DEFAULT and CHECK expressions that reach a function only
  through another function keep the body they were created with when that
  inner function is altered (SQL Server records only direct references, so
  it allows that ALTER).
- A one-part scalar call reports the backend's missing-function error instead
  of 195; calls to functions in another database are not resolved.
- A misplaced definition reports 111 only, with line 1; error line numbers
  inside definitions are not tracked.
- `sys.sql_modules`, `sys.parameters` and `OBJECT_DEFINITION` belong to the
  catalog work; the properties JSON above carries what they need.
- A view that calls a dropped function keeps the last folded body instead of
  failing when queried. Table-valued parameters (`READONLY`) are parsed but
  table types are not available as parameter types.
- Built-in functions the engine does not support (for example `CHARINDEX`
  and `CONVERT` today) fail inside function bodies as they do elsewhere.
