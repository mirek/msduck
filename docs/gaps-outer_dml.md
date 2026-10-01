# UPDATE and DELETE with outer joins in the target tree

Issue #723. SQL Server evaluates the whole FROM clause of `UPDATE ... FROM`
and `DELETE ... FROM`, then changes each target row that appears in the
result once. Before this change msduck accepted only flat inner and cross
join trees around the target and rejected everything else with
"unsupported outer/lateral join in UPDATE target tree" (or the DELETE
equivalent). Those statements now run when the target is joined through
any of these:

- LEFT, RIGHT or FULL joins;
- OUTER APPLY or CROSS APPLY;
- a parenthesized (nested) join;
- a mix of these with inner joins.

## Behavior

- **Rows changed.** Only target rows present in the joined result change.
  A target on the preserved side of a LEFT join is always present. A target
  on the null-extended side (RIGHT or FULL join, or the right side of a LEFT
  join) changes only where it matched. ON predicates stay in their join and
  WHERE filters the joined rows, so `WHERE source.id IS NULL` gives an
  anti-join DELETE.
- **Target binding.** The target binds the way SQL Server binds it:
  - to the FROM relation whose alias is the target name;
  - to an unaliased relation spelled like the target;
  - otherwise to the single reference to the same table under another alias
    or qualification, so `UPDATE items ... FROM items t LEFT JOIN ...`
    changes `t`.

  Among several such references SQL Server takes the unaliased one, and
  fails with 8154 when there is none. This rule now also applies to flat
  inner-join trees, which before fell through to a cross join. Without a
  catalog, two names denote the same table when their last parts match and
  their schemas agree, with `dbo` assumed for a missing schema.
- **Duplicate matches.** A target row matched by several joined rows changes
  once. UPDATE uses the values of one matching row, which SQL Server leaves
  unspecified. `@@ROWCOUNT` and the DONE count give the number of distinct
  target rows.
- **Name binding.** SET and WHERE expressions bind against the FROM tree as
  SQL Server binds them:
  - unqualified columns of the target or of a source;
  - qualified SET columns (`SET t.value = ...`);
  - compound assignment (`+=`) and `DEFAULT`;
  - key changes;
  - scalar subqueries;
  - table hints;
  - CTE sources;
  - variables and RPC parameters.
- **Errors.** In UPDATE SET expressions, a column found in both the target
  and a source fails with 209, an unknown column with 207 and an unknown
  qualifier with 4104. A runtime
  error such as divide by zero (8134) ends the statement with the rows
  unchanged. The batch continues and TRY/CATCH catches the error.
- **Transactions.** A ROLLBACK restores the rows.
- **OUTPUT.** UPDATE supports OUTPUT, including `deleted.*`, `inserted.*`,
  source columns and OUTPUT INTO. DELETE supports `OUTPUT deleted.*`.

## Implementation

The changes follow the extension hooks in docs/extension-hooks.md:

- **DELETE.** `msduck_sql::delete` rewrites a target in an outer tree to
  `DELETE FROM <table> AS __msduck_outer_target WHERE
  __msduck_outer_target.rowid IN (SELECT <alias>.rowid FROM <tree> WHERE
  <where>)`. The subquery keeps SQL Server's scope for the tree, and `IN`
  deletes each row once. The `outer_dml` statement hook applies this rewrite
  before OUTPUT planning, so `OUTPUT deleted.*` sees the base table's
  declarations.
- **UPDATE.** UPDATE has to choose one joined row per target row before it
  evaluates the assignments. The joined OUTPUT stages (`output_join`,
  `engine/joined_output.rs`) already do this:
  1. capture candidates deduplicated by row identity;
  2. evaluate the assignments into images;
  3. write the images by row identity.

  The `outer_dml` statement hook gives an outer-tree UPDATE without OUTPUT an
  empty OUTPUT list, which no user can write. The stages treat that list as
  "write, return no rows". The statement then completes with the UPDATE
  command and count, and its errors end the statement like any UPDATE error.
- **Nested joins.** Join scopes do not list the members of a parenthesized
  join. The joined binding therefore takes declarations from a copy of the
  tree with those members spliced in, keeping the nullability of outer joins.
  The rows still come from the original tree. `output_bind` lists the same
  factors (companion task gaps-outer-dml-v1-bind).
- **Canonical form.** `msduck_sql::update::canonicalize` no longer rejects
  outer trees. For binding and preparation it produces `UPDATE <table> AS
  __msduck_outer_target ... FROM <tree> WHERE __msduck_outer_target.rowid =
  <alias>.rowid AND (<where>)`.

## Evidence

- `reference/gaps-outer_dml.json` holds 46 programs captured from
  `mcr.microsoft.com/mssql/server:2022-latest` (16.0.4236.2) by
  `scripts/capture-gaps-outer_dml.mjs`. Each case records its setup, the DML
  batch with `SELECT @@ROWCOUNT`, and a readback, with rows, error numbers and
  messages, and DONE counts.
- `tests/compat/outer_dml.test.mjs` replays every case through tedious and
  requires an exact match. OUTPUT rows are sorted first, because SQL Server
  does not order them. It also checks the issue's two repros and RPC
  parameters.
- `tests/gaps_outer_dml.rs` checks rows, `@@ROWCOUNT`, error numbers,
  rollback and preparation on a session. It also replays the captured
  readbacks.

## Remaining limits

- Unknown or ambiguous names in WHERE or ON clauses fail with the backend's
  binder text and number 50000, not 207, 209 or 4104. The same is true of
  SELECT and flat joins elsewhere in msduck. For DELETE the text can name
  the internal alias `__msduck_outer_target`.
- An outer-tree UPDATE uses the joined OUTPUT stages, so it inherits their
  target limits even without OUTPUT, and the messages mention OUTPUT. These
  targets fail explicitly:
  - tables with computed (generated) columns;
  - views and CTE targets;
  - tables with a column named `rowid`.
- OUTPUT metadata can mark a column nullable when SQL Server does not. This
  happens when a parenthesized join contains a RIGHT or FULL join and sits
  under an inner join. Rows are unaffected.
- An outer-tree DELETE of a table with a stored column named `rowid` fails
  explicitly. The column would shadow the backend row identity.

- Preparing (sp_prepare) an outer-tree UPDATE whose SET or WHERE uses an
  unqualified target column fails with a backend binder error. Preparation
  binds the canonical form, in which the target appears twice. Direct
  execution and sp_executesql are not affected.
- DELETE OUTPUT cannot return source columns (`OUTPUT s.col`) for any joined
  DELETE; it fails explicitly, as before.
- `UPDATE TOP (n)` and `DELETE TOP (n)` are unsupported in general, joined or
  not.
- OUTPUT INTO a constant expression (for example `'x'`) fails for every
  UPDATE, with "OUTPUT destination requires a known logical source type".
- Inner-join UPDATE (outside this task) still runs natively. A target row
  matched by several source rows there is counted once per match.
