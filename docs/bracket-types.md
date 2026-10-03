# Delimited system type names

SQL Server accepts a system type name written as a delimited identifier
wherever a data type can appear. Generated SQL uses this form heavily:

```sql
SELECT CONVERT([nvarchar](200), JSON_VALUE(N'{"a":"foo"}', N'$.a'));
CREATE TABLE [dbo].[items] ([id] [int] NOT NULL, [name] [nvarchar](100) NULL,
  [value] AS (CONVERT([nvarchar](200), JSON_VALUE([name], N'$.a'))) PERSISTED);
DECLARE @x [decimal](5, 2) = 1.239;
```

sqlparser reads a delimited type name as `DataType::Custom`, which no lowering
recognized. `CONVERT([nvarchar](200), x)` reached DuckDB as a call of an
unknown `convert` function (208), and `[int]` columns and variables failed.

## Behavior

A delimited name resolves like the plain spelling of a system type. SQL Server
ignores case and trailing spaces in the name (`[INT]`, `[int ]`), accepts
square brackets and, with `QUOTED_IDENTIFIER ON`, double quotes, and accepts
the `sys` schema as a qualifier (`[sys].[int]`, `sys.int`). Another schema is
not a system type: SQL Server reports 243 for `CAST(1 AS [dbo].[int])`.

`crates/msduck-sql/src/dialect/ext/conversion/bracket_types.rs` implements this
as an AST normalization that `msduck_sql::batch::parse` applies to every
statement it parses (task `v025-bracket-types-v1-batch`). It replaces each
such type, at any depth, by the type its plain spelling parses to:

- CAST, TRY_CAST, CONVERT and TRY_CONVERT targets;
- CREATE TABLE columns, including computed columns and DEFAULTs, ALTER TABLE
  ADD and ALTER COLUMN, and table variable columns;
- DECLARE and sp_executesql parameter declarations;
- OPENJSON WITH columns.

The result is the same AST as for the plain spelling, so descriptors, rows and
errors match it. Delimited identifiers that are not type names, such as a
column `[date]` or an alias `AS [int]`, are unchanged. So are delimited names
that are not system types (`[notatype]`, `[dbo].[int]`), which keep msduck's
existing errors.

`sysname` is an alias type for `nvarchar(128)`. Delimited or not, it now
resolves to `nvarchar(128)` as a CAST or CONVERT target, a variable, a
parameter, a function return type and an OPENJSON column, matching SQL Server's
descriptors (NVarChar, 256 bytes). As a table column it keeps its name, for
the catalog's `TYPE_NAME`, and remains unsupported (see below).

Procedure, function and trigger bodies, table variable definitions and
stored definitions are parsed through `batch::parse` as well. Three other
places resolve the names themselves:

- `msduck_sql::sql_type::declaration` resolves delimited names and `sysname`
  for procedure and function parameters and RETURNS types, which their
  definitions parse outside `batch::parse`
  (task `v025-bracket-types-v1-sql-type`).
- Catalog text (`sys.computed_columns`, `sys.default_constraints`) is
  rendered from the expression as written; its CONVERT type renders a
  delimited system type name like the plain one, as SQL Server does:
  `(CONVERT([nvarchar](200),json_value([body],N'$.a')))`
  (task `v025-bracket-types-v1-catalog-text`).
- The conversion runtime (`src/engine/ext/conversion.rs`) resolves CAST and
  CONVERT targets that features build from stored declarations, such as a
  scalar function's RETURNS type, before binding.

The runtime also rewrites CONVERT and TRY_CONVERT without a style to
`nvarchar(max)`, `varchar(max)` or `varbinary(max)` as the equivalent CAST or
TRY_CAST before binding. No lowering took over that CONVERT, so
`CONVERT(nvarchar(max), x)` failed with 208 whether or not the name was
delimited.

### Why batch parsing

The conversion feature's statement parse hook cannot normalize a statement:
it would have to call `parse_statement` on its own parser, and the dialect
would dispatch back to the hook at the same token. Parsing the statement with
a second parser instead means copying the rest of the batch for each
statement, which made a batch of 5,000 statements parse 15 to 35 times
slower in a debug build. Normalizing after `parse_statement` in
`batch::parse` costs one AST walk per statement.

## Evidence

`tests/compat/bracket_types.test.mjs` runs 14 cases, each in a fresh database,
and compares columns (name, type, length, precision, scale), rows and errors
(number, state, class, message) with values captured from
`mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`
(Microsoft SQL Server 2025 RTM-CU7, 17.0.4065.4). The capture ran each case's
statements through `scripts/lib/compatibility.mjs` `capture` in a database
created by `isolatedReference`, inside `withReferenceContainer`. The cases
include the report's reproduction, which returns `foo` as `nvarchar(200)`
(NVarChar, 400 bytes) like SQL Server.

`crates/msduck-sql/tests/bracket_types.rs` checks that delimited and plain
spellings parse to the same statements, that other delimited names are
unchanged, that statement boundaries survive the second parse, and that
declarations resolve.

## Remaining differences

Each of these also holds for the plain spelling; the compat test asserts
msduck's current behavior:

- `sysname` table columns fail with DuckDB's 208 (SQL Server creates them,
  `TYPE_NAME` = `sysname`).
- Names that are not system types fail with 208, or 40515 for DECLARE
  (SQL Server: 243 "Type notatype is not a defined system type.", 2715 for
  DECLARE).
- `int(5)` reaches DuckDB and fails with 50000 (SQL Server: 291).
- `numeric(p,s)` is described as decimal; `decimal` without a precision keeps
  the operand's scale (SQL Server: `decimal(18,0)`); a binary literal shorter
  than `binary(n)` fails instead of being padded.
- `sys.columns` descriptors differ from SQL Server's (the rows match).
- Arithmetic overflow 8115 has state 1 (SQL Server: 2), and a malformed CAST
  reports msduck's generic syntax error 102 (SQL Server: 156).
