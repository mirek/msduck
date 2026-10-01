# Stored procedures, EXEC (string) and sp_executesql

Issue #719 (task `gaps-procedures-v1`). Against v0.2.4:

- `CREATE PROCEDURE foo AS SELECT 7 AS value;` failed with 40515 (unsupported
  statement).
- `EXEC sp_executesql N'...', N'@p int', @p = 1` in a SQL batch failed with
  137, and its parameterless form with 40515.
- A tedious request with an output parameter failed with 50000 at the
  parser, because the sp_executesql declaration `@x int OUTPUT` did not parse.

This document describes what now works, the SQL Server evidence for it, and
what remains.

## Evidence

`scripts/capture-gaps-procedures.mjs` runs 145 observations twice, each in a
fresh pinned SQL Server 2025 container (17.0.4065.4, the image in
`scripts/lib/reference-container.mjs`). It records result sets, ERROR and INFO
messages, RETURNVALUEs and the token stream: every DONE, DONEPROC and
DONEINPROC body, and every RETURNSTATUS. The retained runs are
`reference/gaps-procedures.json`. `node scripts/capture-gaps-procedures.mjs
--check` revalidates them without a container.

`tests/compat/procedures.test.mjs` replays every observation against msduck
through tedious, in order, on one connection. It compares values, error
numbers, return statuses, RETURNVALUEs and the DONE token sequence. The
differences that remain are listed in that test and below. The same file also
has focused tests, which pass unchanged against SQL Server 2022
(16.0.4236.2) as well as msduck. `tests/gaps_procedures.rs` checks the module store, restart
persistence, exact token bytes, and values and error numbers through tiberius.

## Definitions

A batch that begins with `CREATE [OR ALTER] PROC[EDURE]` or
`ALTER PROC[EDURE]` defines a procedure. Leading comments are allowed. The
batch hook handles it before parsing, because SQL Server keeps the whole batch
text as the definition.

- The header accepts:
  - a schema-qualified name (a database prefix fails with 166);
  - parameters, optionally in parentheses;
  - `@p [AS] type [NULL] [= default] [OUT | OUTPUT]`;
  - `WITH RECOMPILE | ENCRYPTION | EXECUTE AS ... | SCHEMABINDING |
    NATIVE_COMPILATION`;
  - `FOR REPLICATION`.
- The body is compiled at CREATE the way SQL Server does:
  - syntax errors (102, 156);
  - undeclared variables (137);
  - a variable that repeats a parameter (134);
  - `USE` (154);
  - a nested CREATE PROCEDURE (156).

  Table names are resolved when the procedure runs (deferred name
  resolution).
- CREATE fails with 2714 (state 3) when the name exists. ALTER of a missing
  procedure fails with 208 (state 6), and ALTER or CREATE OR ALTER of
  another kind of object with 2010. A CREATE PROCEDURE that does not start
  its batch fails with 111.
- Inside a parameterized sp_executesql request, CREATE PROCEDURE fails with
  156: SQL Server prefixes the parameter declarations to the batch.
- `EXEC sp_executesql N'CREATE PROCEDURE ...'` and `EXEC (N'CREATE ...')`
  define procedures.
- Completion is DONE with CurCmd 222 for CREATE and ALTER, and 223 for DROP.

The module store (`src/engine/ext/modules.rs`) keeps the definition text.
Its `properties` JSON records the parameters for the catalog task:

```json
{"parameters":[{"name":"@x","type":"int","output":false,"default":null}]}
```

`type` is the declared type in lower case, such as `nvarchar(10)` or
`decimal(5,2)`. `default` is the default's source text, such as `"5"`,
`"N'x'"` or `"NULL"`, or `null` without one. Procedures appear in
`sys.objects` as `P` / `SQL_STORED_PROCEDURE`, so `OBJECT_ID` finds them.
They persist across restarts.

`DROP PROC[EDURE] [IF EXISTS] name [, ...]` removes procedures:

- A missing procedure fails with 3701 (state 5, severity 11).
- A table, view, function or trigger name fails with 3705.

## Calls

`EXEC`/`EXECUTE` take these forms:

- `[@status =] name`, including a two- or three-part name in the current
  database;
- `EXEC @variable`, where the variable holds the procedure name;
- a bare procedure name at the start of a batch.

Arguments can be positional or `@name = value`, case-insensitively. Each
value is one of:

- a constant (a bare identifier is a string, `-1` is negative);
- a variable, optionally followed by `OUT`/`OUTPUT`;
- `DEFAULT`.

An expression such as `1 + 1` is a compilation error (102), except for
`sp_set_session_context`, whose binder reports it.

`WITH RECOMPILE` is accepted.

- Arguments convert to the parameter types. Character values are truncated
  to the declared length, as SQL Server does. A failed conversion is 8114,
  "Error converting data type varchar to int.".
- Defaults apply to omitted and `DEFAULT` arguments.
- OUTPUT values convert back to the caller variable's type. Without
  `OUTPUT`, the caller's variable is unchanged.
- Call errors end only the call. As in SQL Server, a later statement still
  runs:
  - 201: a missing value;
  - 8144: too many arguments;
  - 8145: an unknown name;
  - 8143: a repeated name;
  - 8162: OUTPUT to an input parameter;
  - 2812: an unknown procedure.
- These are compilation errors that stop the batch:
  - 119: a positional argument after a named one;
  - 179: OUTPUT after a constant.

The token stream matches the captures:

- Statements inside a call complete with DONEINPROC, always with DONE_MORE.
  `SET NOCOUNT ON` clears the count bit but keeps the count, and hides
  statement completions.
- A call from a batch ends with RETURNSTATUS and DONEPROC (CurCmd 224).
- A nested call ends with DONEINPROC (224) and no RETURNSTATUS.
- `RETURN` completes with CurCmd 219. `RETURN value` completes like a
  one-row SELECT.

The return status is:

- the RETURN value;
- otherwise 0, or `10 - severity` of the most severe error this procedure's
  own statements raised, even if its own CATCH handled it (captured: -6
  for severity 16, -4 for 14).

`RETURN NULL` returns 0 with message 282.

Each call runs in its own frame:

- Its own variables: the caller's are not visible (137 at CREATE).
- `SET NOCOUNT`, `XACT_ABORT`, `ANSI_WARNINGS` and `DATEFIRST` revert when
  it returns.
- `@@NESTLEVEL` counts frames. sp_executesql adds two levels, itself and
  its batch (captured 1 for `EXEC (string)` and 2 for sp_executesql).
- A 33rd nested level fails with 217 and aborts the batch, unless a
  caller's CATCH handler receives it.
- A changed `@@TRANCOUNT` raises 266 after a procedure, `EXEC (string)` or
  sp_executesql.
- An OUTPUT value or status that does not fit the caller's variable fails
  with 8114 (state 2) and ends the batch.
- `@@ROWCOUNT` after a call is that of the procedure's last statement (0
  after a bare RETURN).
- `ERROR_NUMBER()` and the other error functions inside a procedure see the
  caller's caught error.

### Errors inside a procedure

| Error | SQL Server and msduck |
| --- | --- |
| RAISERROR, divide by zero, a failed DML statement | Statement-terminating: the error is sent with DONEINPROC (DONE_ERROR) and the procedure continues |
| A compilation error found at run time (208 for a missing table) | Ends the procedure; the caller continues after the call |
| THROW, a conversion failure and other batch-aborting errors | End the batch |

When a caller's `BEGIN TRY` encloses the call, at any depth, SQL Server
transfers the first error to that CATCH handler instead. The parser marks
calls inside TRY bodies so that a procedure knows this. A nested procedure
ends with DONEINPROC (224) and the call with DONEPROC, without RETURNSTATUS.
`ERROR_PROCEDURE()` names the procedure in which the error arose. It is NULL
for errors outside procedures and in dynamic SQL. A batch statement outside
CATCH clears the remembered procedure, so a later error at batch level does
not inherit it.

## Dynamic SQL and sp_executesql

`EXEC (string)` runs a string expression, such as a literal, a variable or a
concatenation. It also accepts `AS LOGIN|USER = '...'`, which runs as the
caller. The string runs in a frame without the caller's variables (137).
`RETURN value` inside fails with 178. Statuses follow the procedure rule.

`EXEC sp_executesql @stmt, @params, values...` in a SQL batch:

- binds values positionally or by name (`@stmt`/`@params` may be named);
- returns OUTPUT values;
- reports the last error number as its status (50000 after RAISERROR).

Its errors are:

- 8178: a missing value (reported before an extra argument);
- 8144: an extra argument;
- 214: a non-Unicode statement;
- 178: `RETURN value`;
- 8162: OUTPUT to a parameter that was not declared OUTPUT.

`batch::declared_parameters` parses declaration lists with `OUT`/`OUTPUT`;
`batch::parameter_declarations` now accepts them too. A tedious
parameterized request already runs as an sp_executesql RPC through
`src/rpc.rs`, and procedure calls inside it work.

## Syntax encoding

A call stays a `Statement::Execute`, so the engine's procedure-call path
still writes RETURNSTATUS and DONEPROC. Other features' `exec` hooks also
still see it. `msduck_sql::dialect::ext::procedures::call` decodes it into
target, arguments, status variable and TRY marking. Inside `parameters`:

- `@name = value` stays an `Eq` binary expression;
- `OUTPUT` is `__msduck_output(value)`;
- `DEFAULT` is `__msduck_default()`.

`using` carries tagged markers: `__msduck_return` (the `@status`
variable), `__msduck_module` (`EXEC @variable`) and `__msduck_try`. Calls to
`sp_set_session_context` are not marked, because their binder accepts only
the plain form.

## Remaining differences

The replay test lists each of these:

- **RPC procedure calls and output parameters**, which need `src/rpc.rs`.
  The `prepared-rpc-metadata-v1` task reserves that file. The work left
  there:
  - accept parameters with the RPC status flag `fByRefValue` (1) in `bind`.
    It still fails with 40515 "unsupported output value parameter";
  - after the batch, send their final values as RETURNVALUE tokens before
    DONEPROC;
  - dispatch an RPC by procedure name (tedious `callProcedure`) to the
    procedure runtime. It still fails with "unsupported RPC procedure".

  The declaration parsing, binding and frame-variable write-back these need
  already exist here.
- A failed call sends RETURNSTATUS 1 before its DONEPROC. SQL Server sends
  no status for procedures. For sp_executesql, it sends the error number.
  This status comes from the engine's EXEC error path.
- `@@ERROR` after a call is 0. SQL Server keeps the error of the called
  scope's last statement.
- A batch that a procedure aborts ends with CurCmd 0 rather than 253.
  Compilation errors 119, 179 and 111 do the same.
- 8144 for a known procedure is a call error (the batch continues). SQL
  Server reports it while compiling the batch.
- A failed DROP PROCEDURE ends the batch instead of the statement.
- ERROR and INFO tokens carry no procedure name, and line numbers are 1.
- Messages that come from engine diagnostics use the engine's text. For
  example, 208 reads "Catalog Error: Table with name ... does not exist!".
- A duplicate key ends the batch rather than the statement, so the
  procedure does not continue.
- `@@NESTLEVEL` in a parameterized tedious request (an sp_executesql RPC)
  is 0; SQL Server reports 2.
- `EXEC ('')` sends RETURNSTATUS 0. SQL Server sends only DONEPROC.
- These are not supported:
  - numbered procedures (`;2`);
  - table-valued (`READONLY`) and `CURSOR VARYING` parameters;
  - `EXEC ... AT` linked servers;
  - `WITH RESULT SETS` definitions;
  - calls into another database.
- The sys.procedures, sys.parameters and sys.sql_modules views belong to
  the catalog task, which reads the `properties` format above.
