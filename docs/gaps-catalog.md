# Constraint, module and file catalogs

Issue #728 (gaps-catalog-v2, successor of gaps-catalog-v1 and
catalog-key-identities-v1). The catalog feature (`src/engine/ext/catalog`,
syntax and deterministic rules in `crates/msduck-sql/src/dialect/ext/catalog`)
provides the system catalog views, `OBJECT_DEFINITION`, `sp_pkeys`,
`sp_fkeys`, `sp_rename` and `INFORMATION_SCHEMA` over the stores that other
features keep. Every database has them.

## Catalog views

Column sets and order are SQL Server 2025's (17.0.4065.4), captured with
`SELECT TOP(0) *`.

| View | Rows | Source |
| --- | --- | --- |
| `sys.foreign_keys` | FOREIGN KEY constraints | constraints store; `key_index_id` is the referenced PRIMARY KEY or UNIQUE constraint's index |
| `sys.foreign_key_columns` | their columns | constraints store (unchanged) |
| `sys.key_constraints` | PRIMARY KEY and UNIQUE | keys store; `unique_index_id` from the index catalog |
| `sys.default_constraints` | every DEFAULT, named or not | `main.__msduck_default_constraints` with the declared text |
| `sys.check_constraints` | CHECK constraints | constraints store with SQL Server's definition text |
| `sys.computed_columns` | computed columns | `sys.columns`, computed-column store, declared text |
| `sys.triggers` | DML triggers | module store (`instead_of` property) |
| `sys.sql_modules` | procedures, functions, triggers, views | module store, view text |
| `sys.procedures` | procedures | `sys.objects` |
| `sys.parameters` | parameters of procedures and functions; a scalar function's return value as parameter 0 | module store `properties` |
| `sys.database_files` | the database's files | backup feature (unchanged) |

The constraints feature defines `sys.check_constraints`, `sys.foreign_keys`
and `sys.key_constraints`; this feature bootstraps after it and redefines
them with the same columns, adding definitions and index IDs.

### Definitions

SQL Server does not keep CHECK, DEFAULT and computed-column expressions as
written. It stores a normalized text, which `definition` derives from the
declared expression (`dialect::ext::catalog::definition`):

- the whole expression is parenthesized; columns are bracketed; numeric
  constants are parenthesized and normalized (`0001` is `(1)`, `1e2` is
  `(1.0000000000000000e+002)`, `2147483648` is `(2147483648.)`, `-1.5` is
  `(-1.5)`); strings, binary constants and NULL are not;
- `+ - & | ^` and unary minus are parenthesized as operands, `* / %` only
  inside another multiplicative or unary operator; T-SQL's left-to-right
  reading of `+ - & | ^` applies (`a | b ^ c` is `(a|b)^c`);
- AND/OR chains are flat on the left; an OR inside an AND, and a right
  operand of the same operator, keep parentheses;
- `IN` becomes an OR chain in reverse order, `BETWEEN` two comparisons,
  `!=` becomes `<>`, `NOT IN` and `NOT BETWEEN` a negated group;
- `CAST`/`CONVERT` become `CONVERT([type],value[,(style)])`, `TRY_CONVERT`
  becomes `TRY_CAST(value AS [type])`, `IIF` a searched CASE, `YEAR`,
  `MONTH` and `DAY` become `datepart`, datepart abbreviations are spelled
  out, `CURRENT_TIMESTAMP` is `getdate()`;
- CASE without ELSE keeps SQL Server's double space before `end`; `LIKE`
  with `ESCAPE` keeps its trailing space.

An expression outside these rules has a NULL definition rather than an
approximation (for example subqueries, ODBC date literals and money
constants).

The rules are checked against 97 CHECK, 24 DEFAULT and 10 computed
expressions of the v2 profile and the 24 of the earlier definition profile:
`definitions_match_every_captured_declaration` formats every captured
declaration from SQL Server's own statement text.

Other features rewrite declarations before the catalog sees them: session
functions in DEFAULTs become reads of connection variables, computed columns
over Unicode carriers become backend expressions, and the batch parser gives
`CAST(x AS VARCHAR)` its default length. The catalog therefore reads each
batch's declarations as written (`declarations`) when the batch starts, and
attaches them after the batch to the objects that the batch created (object
IDs only grow, so these are the IDs above the highest one observed at the
start). Statements inside procedure bodies are not batches: their DEFAULTs
and computed columns keep the text of the statement as executed, or none.
CHECK constraints without a name use the constraints store's normalized
text, so `CAST(x AS VARCHAR)` shows as `[varchar](30)` there.

`sys.computed_columns.is_nullable` follows SQL Server's inference: only
columns declared NOT NULL, non-NULL constants, `ISNULL` with a non-null
operand and CASE whose branches (with ELSE) are non-null are non-null, and
a column declared NOT NULL is not nullable.

### DEFAULT constraints

Every DEFAULT is an object, as in SQL Server. An unnamed one gets SQL
Server's generated name, `DF__table__column__XXXXXXXX`, ending in its object
ID in hexadecimal; the table and column parts share 14 characters, the table
keeping at least nine and the column at least five
(`declarations::constraint_name`; unnamed CHECK and FOREIGN KEY constraints
use the same rule, and a table-level CHECK keeps 16 characters of the table).
CREATE TABLE and `ALTER TABLE ... ADD` record them in their DDL transaction;
for the latter the constraints feature runs the new columns without their
DEFAULT names and then gives the generated objects their declared names.
DEFAULTs that the identity and rowversion features add as backend
allocators are not objects.

`sys.columns.default_object_id` therefore names the DEFAULT of every column
that has one, and the DEFAULT can be dropped by its generated name. As in
SQL Server, a column with a DEFAULT cannot be dropped (5074, 4922), and
ALTER COLUMN keeps a DEFAULT only when the type stays the same (another
length, precision or scale is allowed) and a CHECK only when a
variable-length type changes its length.

### Modules and views

`sys.sql_modules.definition` and `OBJECT_DEFINITION` give the batch text of
the module or view, with a leading `ALTER` replaced by `CREATE` and the
`OR ALTER` of `CREATE OR ALTER` removed, as SQL Server keeps it. A view's
text is recorded when its CREATE or ALTER VIEW batch succeeds, in the same
transaction; a view created before this feature, or through dynamic SQL,
has a NULL definition. SET options are those every msduck session uses:
`uses_ansi_nulls` and `uses_quoted_identifier` are 1. A scalar function is
reported inlineable whenever inlining is on; SQL Server also checks its
body.

`sys.parameters` reports types through `sys.types`, with lengths, precision
and scale from the declaration. `has_default_value` is 0 for T-SQL modules,
as in SQL Server.

### Key constraints

Key constraint object IDs are `2000000000 + tag` of their keys-store row:
they survive restarts and ALTER TABLE rebuilds, a rolled back drop keeps
them, and dropping and creating a table again gives new ones. `sp_rename`
keeps them.

The CLUSTERED or NONCLUSTERED keyword of PRIMARY KEY and UNIQUE
constraints, which tokenizing drops, is read from the batch text, and DESC
key columns from the statement; both are recorded in
`main.__msduck_key_layout` for CREATE TABLE and for `ALTER TABLE ... ADD`.
`sys.indexes` and `sys.index_columns` use them: `PRIMARY KEY NONCLUSTERED`,
`UNIQUE CLUSTERED` and DESC columns appear as in SQL Server. Without a
keyword, SQL Server's defaults apply.

### Temporary objects

The backend tables of `#temp` tables, table variables and `##global`
tables live in the current database, where other features find them
through `sys.objects` and `OBJECT_ID`. SQL Server shows them only in
tempdb, so a user query's `sys.objects`, `sys.all_objects` and `sys.tables`
read a derived table without them, their constraints and their triggers.
The derived table selects from the system view, so result metadata is
unchanged. The views defined here leave them out too.

## INFORMATION_SCHEMA

`INFORMATION_SCHEMA.TABLES`, `COLUMNS`, `VIEWS`, `SCHEMATA`,
`TABLE_CONSTRAINTS`, `KEY_COLUMN_USAGE`, `CONSTRAINT_COLUMN_USAGE`,
`REFERENTIAL_CONSTRAINTS`, `CHECK_CONSTRAINTS`, `ROUTINES` and `PARAMETERS`
have SQL Server's columns and rows for the current database: a reference
becomes a derived table over msduck's catalog with SQL Server's column names
and declared types and the database's name. Previously these were DuckDB's
own views, listing msduck's internal tables and other databases.

## OBJECT_DEFINITION

Returns the text of procedures, functions, triggers and views, and the
definitions of DEFAULT and CHECK constraints; NULL for tables, unknown IDs
and NULL. System objects have no text here.

## sp_pkeys and sp_fkeys

Named and positional arguments bind like SQL Server's (8145, 8144, 8143,
119, 201). Names match exactly, ignoring case, without wildcards. A
qualifier other than the current database's name fails with 15250 and
status -6; `sp_fkeys` without either table fails with 15252 and status -6.
The result sets have SQL Server's columns and descriptors; `KEY_SEQ`
follows the key's column order. `sp_fkeys` maps the referential actions to
ODBC rules (CASCADE 0, NO ACTION 1, SET NULL 2, SET DEFAULT 3) and, like SQL
Server, reports 1 for both when only the foreign key table is given. The
completion tokens follow the procedures' bodies (five DONEINPROC before
`sp_pkeys`' result, none before `sp_fkeys`'), and SET NOCOUNT ON omits them.

## sp_rename

Renames tables, views, procedures, functions, triggers, PRIMARY KEY,
UNIQUE, CHECK, FOREIGN KEY and DEFAULT constraints (`OBJECT`, or no type),
columns (`COLUMN`, or `table.column` without a type) and indexes (`INDEX`,
including the index of a key constraint, or `table.index` without a type),
and sends SQL Server's caution (15477, line 801). Object, column and index
IDs stay. Definitions that name the old object are not changed, as in SQL
Server; the functions feature's record of the objects that call a function
follows. A column that a CHECK constraint or computed column uses fails
with 15336, and one that a filtered index's filter uses sends the caution
and fails with 5074 and 4922; renaming a computed column sends the caution
and fails with 4928.
Other errors follow SQL Server's checks and order: unrecognized types
(15249), NULL names (15223), invalid new names (15004 and 15224), unknown
items (15225 without a type, 15248 with one), duplicates (15335). Errors are
raised like SQL Server's RAISERROR: the batch goes on with status 1 and
`@@ERROR` 0, XACT_ABORT does not apply, and a CATCH block around the call
receives them. A CATCH block of a calling procedure does not yet.

DuckDB refuses to rename a table, or one of its columns, while indexes
depend on it, so the table's indexes are dropped, the rename runs, and the
indexes return under new backend names in the same transaction; every
store that names the table, column or index follows. A rename inside a
transaction rolls back with it.

## Evidence

`reference/gaps-catalog.json` keeps the pinned SQL Server 2025 captures:

- `runs` (46 responses, two fresh containers), `definitionProfile` and
  `namespaceProfile`, captured by the gaps-catalog-v1 session at revisions
  5e7f21e through fc6221f. Removing `catalogV2Profile` reproduces their
  aggregate SHA-256
  `bd024b56b37216c580e4c4bc6e755aa1ac9f1b1b577d310537ebdca54ab9ae9c`.
- `catalogV2Profile` (128 responses from each of two fresh pinned
  containers, which agree completely): definitions, constraint naming, key
  layout, modules and parameters, `OBJECT_DEFINITION`, `sp_pkeys`,
  `sp_fkeys`, the `sp_rename` matrix with return statuses and TRY,
  INFORMATION_SCHEMA and temporary objects. Its queries select no IDs or
  clocks.

Recapture into a new path with
`node scripts/capture-gaps-catalog.mjs --v2 artifacts/new.json` (or
`--definitions`, `--namespace`, or no flag for the first profile); the
helper starts and removes its own containers.

`tests/compat/catalog.test.mjs` runs every statement of `runs` and
`catalogV2Profile` through tedious and compares columns, rows, errors,
messages and return statuses with SQL Server's, with the exceptions below
asserted exactly. `tests/gaps_catalog.rs` covers restarts, rollback of
renames and definitions, and key identities through the keys lifecycle.

## Remaining differences

- Wire descriptors of the views defined here, of INFORMATION_SCHEMA and of
  the constraints feature's views are DuckDB's types (`nvarchar(max)`,
  nullable `int`), not SQL Server's declarations. The root catalog adapter
  types only the views it knows (`sys.objects`, `sys.tables`,
  `sys.indexes`, ...).
- Binding errors of `sp_pkeys`, `sp_fkeys` and `sp_rename` (8145, 8144,
  201) end with RETURNSTATUS 1, as for every procedure hook; SQL Server sends
  none. The internal ENVCHANGE tokens of `sp_rename`'s own transaction and
  ORDER tokens are not sent.
- CHECK constraints and computed columns that the constraints and computed
  features cannot yet execute (varchar concatenation, CHARINDEX,
  `CONVERT(FLOAT(53), ...)`) cannot be created; their catalog text is still
  covered by the formatter tests.
- Unnamed CHECK constraints keep the constraints store's text, in which
  `CAST(x AS VARCHAR)` has the length 30.
- A PRIMARY KEY that is clustered by default does not reject a later
  `CREATE CLUSTERED INDEX` with 1902 (keys feature).
- `sys.schemas`, and so `INFORMATION_SCHEMA.SCHEMATA`, lacks the fixed role
  schemas (`db_owner`, ...). ORDER BY on names sorts by code point, not by
  the case-insensitive collation.
- `sp_rename` of databases, statistics and user data types is unsupported,
  as are temporary tables. Schema-bound views and functions are not recorded
  as dependencies, so the 15336 SQL Server raises for their tables and
  columns is not.
- `OBJECT_DEFINITION` of system objects is NULL.
- `sys.sql_modules` reports the session's SET options rather than those at
  creation, and `execute_as_principal_id` is NULL.
