# Contextual identifiers and database-qualified diagnostics

Task `gaps-identifiers-v1` (issue #729), with the companion
`gaps-identifiers-v1-dialect` for the parser delegation.

## Ground truth

`scripts/capture-gaps-identifiers.mjs` starts an owned SQL Server 2022
container (labelled `msduck.task=gaps-identifiers-v1`), creates a fresh user
database and records `reference/gaps-identifiers.json`:

- 73 words that DuckDB reserves (its `reserved`, `type_func_name` and
  `col_name` keyword categories) plus T-SQL reserved controls. For each word
  the capture runs CREATE TABLE, INSERT with and without a column list,
  SELECT, two- and three-part references, aliases with and without `AS`,
  UPDATE, a `@word` parameter, a CTE, GROUP BY, DELETE, CREATE INDEX,
  CREATE VIEW, a `sys.columns` lookup and DROP;
- string truncation (2628), PRIMARY KEY (2627), unique index (2601) and
  NOT NULL (515) diagnostics inside that user database.

SQL Server accepts every DuckDB keyword in the list as a regular identifier
in all of those positions. Only `pivot`, `unpivot`, `tablesample` and
`values`, which are T-SQL reserved words, fail with 156.

`node scripts/capture-gaps-identifiers.mjs --local PORT` replays the same
cases against a running msduck and prints each differing observation.

## Behavior

### Identifiers

The words that DuckDB reserves but SQL Server does not (for example
`offset`, `limit`, `qualify`, `at`, `using`, `window`, `interval`,
`lateral`, `trim`, `returning`, `natural`, `true`, `time`, `position`) work
unquoted as table, column, view, index, CTE and alias names and as parameter
names, in DDL and DML. Their delimited forms (`[offset]`, `"offset"`) keep
working, and names keep their written case.

Two layers make this work:

- **Parsing** (`crates/msduck-sql/src/dialect/ext/identifiers.rs`). The
  batch tokenizer drops the keyword meaning of words that are never T-SQL
  syntax (`LIMIT`, `QUALIFY`, `LATERAL`, `INTERVAL`, `RETURNING`,
  `NATURAL`, `ILIKE` and so on), and of `CAST`, `TRY_CAST`, `TRIM`,
  `EXTRACT`, `SUBSTRING` and `OVERLAY` when they are not directly followed by
  `(`. Words that are T-SQL syntax only in particular places are accepted
  as bare aliases elsewhere: `OFFSET` (syntax only after ORDER BY), `AT`
  (`AT TIME ZONE` is kept), `WINDOW` (a `WINDOW name AS (...)` clause is
  kept) and `USING` (a following table source keeps `MERGE ... USING`).
  `ServerDialect` no longer parses Snowflake/BigQuery table versioning
  (`AT(...)`, `BEFORE(...)`, `CHANGES`), which captured a table alias named
  `at`; SQL Server's `FOR SYSTEM_TIME` was never executable here.
- **Rendering** (`src/engine/ext/identifiers.rs`, the `rewrite_statement`
  and `rewrite_expr` hooks). Before translation, unquoted names in DuckDB's
  `reserved` and `type_func_name` keyword categories (minus T-SQL reserved
  words) are delimited with double quotes in queries, DML,
  CREATE/ALTER/DROP TABLE, VIEW and INDEX, and TRUNCATE, and in subqueries of
  scalar evaluations (`DECLARE @n int = (SELECT max(offset) FROM t)`, IF and
  WHILE conditions). Called function names are left alone, because there
  the word is syntax. DuckDB's `col_name` keywords (`time`, `int`,
  `position`, ...) are valid DuckDB names and double as type names in the
  lowered SQL, so they stay undelimited. DuckDB identifiers are
  case-insensitive even when quoted, so delimiting changes no resolution.

### Diagnostics

- **2628** (string or binary truncation) qualifies the table with the
  session's current database: `'foo.dbo.items'` in database `foo`,
  `'master.dbo.items'` in master. The storage target always lives in the
  current database (references to another database are refused before
  lowering), so `storage_diagnostic::contextualize` resolves the qualifier
  at execution with `__msduck_current_db_name()` rather than taking it from
  its callers. This covers INSERT and UPDATE, inside and outside explicit
  transactions.
- **515** now uses SQL Server's text and state 2:
  `Cannot insert the value NULL into column 'id', table 'foo.dbo.items';
  column does not allow nulls. INSERT fails.` (`UPDATE fails.` for an
  UPDATE). DuckDB's constraint message names only `table.column`; the schema
  comes from the current database's catalog, preferring the statement's own
  schema. The error number, batch continuation, TRY/CATCH and XACT_ABORT
  handling are unchanged. The backend text remains the error's display, so
  message-based classification still yields 515.
- **2627** and **2601** name the object as `'dbo.items'` in SQL Server, with
  no database part, so they never carried the wrong database. Their text is
  unchanged by this task (see below).

## Verification

- `tests/gaps_identifiers.rs`: the words above through DDL, DML, aliases,
  CTEs, parameters, indexes and views in a user database; ORDER BY ...
  OFFSET and AT TIME ZONE beside columns named `offset` and `at`; 2628 and
  515 texts, numbers, states and classes in a user database and master,
  including the materialized write path in a transaction and TRY/CATCH.
- `tests/compat/identifiers.test.mjs`: the same through tedious, including
  `CREATE TABLE items(offset int NOT NULL)`, `sys.columns` names, typed
  `@word` parameters, scalar subqueries in DECLARE/IF/SET and the
  truncation message in database `foo`.
- Unit tests in both modules cover the word lists, parsing of contextual
  words, preservation of T-SQL syntax and the rendering rule.
- Replaying the capture with `--local` differs only in the limits below.

## Remaining limits

- T-SQL reserved words used unquoted as names (`pivot`, `unpivot`,
  `tablesample`, `values`) are not rejected with SQL Server's 156 "Incorrect
  syntax near the keyword" diagnostic: `values` is accepted, and the others
  fail with a different number.
- 2627 and 2601 keep DuckDB's message text, report 2601 as 2627 and use
  class 16 instead of 14. SQL Server's text needs the constraint or index
  name and the formatted key, which DuckDB's message does not carry.
- 515 from a statement nested in a trigger or procedure body reports the
  outer statement's verb (`INSERT fails.`/`UPDATE fails.`). MERGE, SELECT
  INTO and ALTER TABLE paths keep DuckDB's NOT NULL text.
- The word lists are fixed for the bundled DuckDB grammar; a DuckDB upgrade
  that reserves new words needs them extended (the reference replay shows
  it).
- `lateral`, `interval` and similar words only become plain names in
  batches parsed through the msduck tokenizer; internal parses of generated
  SQL with the raw sqlparser tokenizer keep sqlparser's keywords.
