# Constraints: ALTER TABLE lifecycle and foreign-key actions

Issue #716 (task `gaps-constraints-v1`). msduck stores and enforces CHECK and
FOREIGN KEY constraints itself, because DuckDB can neither add, drop nor
disable them after CREATE TABLE, and does not support ON DELETE / ON UPDATE
actions. PRIMARY KEY and UNIQUE stay native DuckDB constraints.

The expected behavior comes from SQL Server 2022 (16.0.4236.2), captured by
`scripts/capture-gaps-constraints.mjs` into `reference/gaps-constraints.json`.
`tests/compat/constraints.test.mjs` replays every captured step against msduck
and compares error numbers, states, classes, messages, informational messages
and rows. The same file also runs against a SQL Server instance when
`MSDUCK_CONSTRAINTS_REFERENCE='{"port":N,"password":"..."}'` is set, which
checks its hand-written expectations against the reference.

## Supported statements

ALTER TABLE, with or without `WITH CHECK` / `WITH NOCHECK`:

- `ADD [CONSTRAINT name] CHECK (expr)`;
- `ADD [CONSTRAINT name] DEFAULT expr FOR column`;
- `ADD [CONSTRAINT name] UNIQUE [CLUSTERED | NONCLUSTERED] (columns)` and
  `PRIMARY KEY (columns)`; index options and `ON filegroup` are accepted and
  ignored;
- `ADD [CONSTRAINT name] FOREIGN KEY (columns) REFERENCES table [(columns)]
  [ON DELETE action] [ON UPDATE action]`, where the action is `NO ACTION`,
  `CASCADE`, `SET NULL` or `SET DEFAULT`;
- several items in one ADD, mixing column definitions and constraints. Columns
  are added first, so constraints may use them. A column definition may carry
  named constraints, for example
  `ADD col int NOT NULL CONSTRAINT df DEFAULT 0 WITH VALUES`, or inline CHECK,
  REFERENCES, PRIMARY KEY and UNIQUE;
- `{CHECK | NOCHECK} CONSTRAINT {ALL | name [, ...]}`, including
  `WITH CHECK CHECK CONSTRAINT ALL`;
- `DROP [CONSTRAINT] [IF EXISTS] name [, ...]` for DEFAULT, CHECK, UNIQUE,
  PRIMARY KEY and FOREIGN KEY constraints, optionally mixed with
  `COLUMN name` items.

CREATE TABLE accepts the same constraints inline (column level) and at table
level, including foreign keys with actions.

## Semantics

- **Validation.** ADD of a CHECK or FOREIGN KEY validates existing rows unless
  `WITH NOCHECK` is given (547, "The ALTER TABLE statement conflicted with
  the ... constraint"). `WITH CHECK CHECK CONSTRAINT` validates too and marks
  the constraint trusted. `NOCHECK CONSTRAINT` disables a constraint and
  marks it untrusted; `CHECK CONSTRAINT` (without `WITH CHECK`) enables it
  again without validation, so it stays untrusted. UNIQUE and PRIMARY KEY are
  always validated (1505 then 1750 for duplicates, 8111 for nullable key
  columns, 1779 for a second primary key).
- **Enforcement.** INSERT, UPDATE, DELETE and MERGE on a table that has
  enabled CHECK or FOREIGN KEY constraints, or that foreign keys reference,
  run in a statement transaction. After the statement, referential actions
  run, and then every affected constraint is checked. CHECK constraints come
  first, then foreign keys, each in creation order. A violation fails the
  statement with 547 followed by informational 3621, with SQL Server's
  message: CHECK, FOREIGN KEY (referencing row without a key, reported
  against the referenced table), REFERENCE (a removed key that is still
  referenced, reported against the referencing table), and the SAME TABLE
  variants for self-referencing keys. A NULL in any referencing column means
  the row is not checked. An untrusted (WITH NOCHECK) constraint tolerates
  the violations that existed before the statement; new ones fail. Disabled
  constraints are neither checked nor acted on.
- **Referential actions.** ON DELETE and ON UPDATE CASCADE, SET NULL and
  SET DEFAULT work for inline, table-level and ALTER TABLE foreign keys,
  through any number of levels. Actions run in breadth-first order from the
  modified table. NO ACTION constraints are checked after all actions, so a
  cascade that removes the referencing row satisfies them. ON UPDATE
  CASCADE pairs each updated row's old and new key, so multi-row key updates
  such as `UPDATE parent SET id = id + 10` cascade row by row. SET DEFAULT
  uses the column's current default, or NULL; a default that is not a key
  fails with 547. Definitions that could reach a table along two cascade
  paths, or cascade into their own table, fail with 1785, like SQL Server.
- **Definition errors.** 1767 (referenced table missing), 1769 and 1770
  (unknown columns), 8139 (column count), 1773 (implicit reference without a
  primary key), 1776 (no matching key), 1778 and 1753 (type, length or scale
  mismatch), 1761 and 1762 (SET NULL / SET DEFAULT on columns that cannot
  take them), 2714 (name already used in the schema), 8168 (duplicate name
  in one statement), 1046 (subquery in CHECK), 207 (unknown column in CHECK),
  137 (variable), 4145 (non-boolean CHECK), 1752, 1754, 1781 and 128 for
  DEFAULT, 3728, 3733 and 3725 for DROP, 4917 and 11415 for CHECK/NOCHECK.
  As in SQL Server, these are followed by 1750, 3727 or 4916, and
  `ERROR_NUMBER()` in a CATCH block reports the last one.
- **Dependent objects.** DROP TABLE of a table that another table's foreign
  key references fails with 3726 (tables listed earlier in the same
  statement are already gone). TRUNCATE TABLE of a referenced table fails
  with 4712, even when the key is disabled. DROP COLUMN, and ALTER COLUMN to
  a different type, of a column that a constraint or named default uses fails
  with 5074 per object, then 4922. ALTER COLUMN that keeps the type and only
  changes nullability is allowed, including on key columns.
- **Atomicity.** Each statement is atomic in autocommit mode. In an explicit
  transaction, a failed INSERT is undone and the transaction stays usable,
  as in SQL Server. A failed UPDATE, DELETE or MERGE, or an ALTER TABLE that
  fails after it changed something, invalidates the native transaction: like
  any native DuckDB constraint error, later statements fail until ROLLBACK.
  ROLLBACK undoes constraint definitions with the rest of the transaction.
- **Catalog.** `sys.objects` lists CHECK (`C`), FOREIGN KEY (`F`), PRIMARY KEY
  (`PK`) and UNIQUE (`UQ`) constraints, so `OBJECT_ID` finds them.
  `sys.check_constraints`, `sys.foreign_keys`, `sys.foreign_key_columns` and
  `sys.key_constraints` are derived from the constraint store, with
  `is_disabled`, `is_not_trusted`, referential actions, `parent_column_id`
  and `is_system_named`. Unnamed constraints get SQL Server-style generated
  names (`CK__table__column__XXXXXXXX`, `FK__...`, `PK__table__...`).

## Design

The feature uses the extension hooks (docs/extension-hooks.md):

- `crates/msduck-sql/src/dialect/ext/constraints.rs` parses the ALTER TABLE
  forms above into a carrier whose payload is the canonical statement text.
  Plain column additions and drops are left to the built-in parser.
- `src/engine/ext/constraints.rs` implements the runtime: the `statement`
  hook handles the carriers, CREATE TABLE, DROP TABLE, TRUNCATE TABLE,
  ALTER TABLE column changes and DML; `bootstrap_database` creates the store
  and the catalog views in every database.
  - `catalog.rs`: the store `main.__msduck_constraints` (one row per CHECK,
    FOREIGN KEY, PRIMARY KEY and UNIQUE constraint) and the views. Named
    DEFAULT constraints use the built-in `main.__msduck_default_constraints`.
  - `define.rs`: definitions and their validation.
  - `enforce.rs`: DML enforcement and referential actions. Old and new keys
    for ON UPDATE CASCADE are materialized before the UPDATE runs, through
    the engine, as a `SELECT ... INTO` over the UPDATE's own source and
    predicate.
  - `translate.rs`: CHECK and DEFAULT expressions are lowered exactly as the
    engine lowers them in CREATE and ALTER TABLE.
  - `rebuild.rs`: DuckDB cannot add a UNIQUE constraint, add a primary key
    to a table with indexes, or drop either, so those changes rebuild the
    table under the same name. Rows (in order), defaults, identity sequences,
    indexes and the catalog identity of the table and its columns survive.
  - `guard.rs`: DROP TABLE, TRUNCATE TABLE and column changes.

## Remaining limits

- A pure `ALTER TABLE t ADD col type NULL CONSTRAINT df DEFAULT v`, without
  `WITH VALUES` and without other constraints in the statement, is still
  refused (40515) by the built-in path, because tests/tedious.test.mjs, owned
  by another task, expects that refusal. The NOT NULL and WITH VALUES forms
  work.
- UNIQUE constraints accept several NULLs, and 2627 duplicate-key messages
  are DuckDB's text rather than "Violation of UNIQUE KEY constraint ...".
  Key storage and UNIQUE NULL semantics belong to the keys task
  (gaps-keys-v1).
- Character keys match binary, like msduck's other comparisons and native
  keys: case-insensitive collation and trailing-space equivalence do not
  apply to foreign keys.
- `sys.check_constraints.definition` is the expression as written, in
  parentheses, not SQL Server's normalized text (`([value]>(0))`).
  `key_index_id` and `unique_index_id` are NULL, and sys.indexes does not
  list constraint indexes. Unnamed DEFAULT constraints are not cataloged
  (existing behavior), so they cannot be dropped by a generated name.
- A failed ADD UNIQUE or PRIMARY KEY reports 1505 and 1750 without the
  informational 3621 that SQL Server adds. The 4145 message lacks
  "near ')'". DEFAULT expressions are not checked for implicit conversion
  (SQL Server's 257).
- In an explicit transaction, failed UPDATE, DELETE and MERGE statements
  invalidate the transaction instead of rolling back only the statement (see
  Atomicity). Inside a constrained DML statement in autocommit mode,
  `@@TRANCOUNT` reads 1.
- Constraints are checked against the session's snapshot, without locks, so
  concurrent sessions can still race (for example, one deletes a key that
  another inserts a reference to).
- Validation scans whole tables per statement, and key changes rebuild the
  table: costs grow with table size.
- ON UPDATE CASCADE does not support UPDATE with TOP or ORDER BY, MERGE that
  changes a key referenced with cascading or differing actions, or a cascade
  through columns that are not a copy of the updated key; these fail with an
  explicit error.
- Rows changed by referential actions do not fire triggers.
- Bulk loads (INSERT BULK) do not check CHECK and FOREIGN KEY constraints,
  like SQL Server's default without CHECK_CONSTRAINTS, but they do not mark
  constraints untrusted.
- Native DuckDB CHECK and FOREIGN KEY constraints of databases created before
  this feature are not in the store: they are still enforced by DuckDB, but
  cannot be dropped, disabled or listed. A table that such a native foreign
  key references cannot be rebuilt.
- Renaming a column (sp_rename) does not update stored definitions.
- Foreign keys on temporary tables are skipped with warning 1756; references
  to other databases are not supported.
