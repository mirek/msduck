# MERGE

msduck executes T-SQL `MERGE` (issue #722). Before this change, a MERGE
with a target hint failed with 102 ("Expected: USING, found: AS"), and every
other MERGE failed with 40515. The workload's upserts and relationship merges
use both forms.

## Supported syntax

```sql
[WITH cte AS (...)]
MERGE [TOP (n) [PERCENT]] [INTO] target [WITH (hint, ...)] [[AS] alias]
USING source [[AS] alias[(columns)]] ON condition
WHEN MATCHED [AND condition] THEN UPDATE SET column = value, ... | DELETE
WHEN NOT MATCHED [BY TARGET] [AND condition] THEN INSERT [(columns)] VALUES (...) | INSERT DEFAULT VALUES
WHEN NOT MATCHED BY SOURCE [AND condition] THEN UPDATE SET ... | DELETE
[OUTPUT $action, inserted.*, deleted.column, source.column, ... [INTO table[(columns)]]];
```

- **Target.** The target is a base table in the current database, written
  with one or two name parts (three parts when the first names the current
  database).
- **Table hints.** Target hints go between the name and the alias, as SQL
  Server requires. Locking hints such as `SERIALIZABLE`, `HOLDLOCK`,
  `UPDLOCK`, `ROWLOCK`, `TABLOCK` and `READPAST` are validated and then have
  no effect, as for SELECT, UPDATE and DELETE. msduck serializes writes
  through its sessions.
- **Hint errors.** These follow SQL Server:
  - `NOLOCK` or `READUNCOMMITTED` on the target fails with 1065;
  - an unknown hint fails with 321;
  - conflicting isolation hints fail with 1047;
  - a hint written after the alias fails with 156.
- **Source.** The source can be a table, a view, a derived query, a
  `VALUES` list with a column alias list, a table function such as
  `OPENJSON`, or a CTE. It can read the target itself.
- **Variables and parameters.** Local variables and RPC parameters
  (`sp_executesql`, which tedious uses for parameterized requests) bind
  anywhere in the statement: in the source, ON, the clause conditions, SET
  and VALUES, OUTPUT and `TOP`.
- **Statement checks.** The existing parse-time checks still run:
  - a missing semicolon fails with 10713;
  - a repeated action in one clause family fails with 10714;
  - a conditional clause after an unconditional one in the same family fails
    with 5324.

  The 10714 message now names the clause first and the action second, as
  SQL Server does.

## Semantics

The implementation is in `src/engine/ext/merge.rs` and its `merge/`
submodules. The syntax is in `crates/msduck-sql/src/merge.rs` and
`crates/msduck-sql/src/dialect/ext/merge.rs`. Both are wired through the
extension hooks in docs/extension-hooks.md.

1. **Classification.** One query joins the target and the source with the
   ON condition. It uses an inner, left, right or full join, depending on
   which WHEN families are present. The query passes through the ordinary
   SELECT pipeline, so names, collations, variables and conversions bind as
   in a SELECT. Unknown names fail with 207, 208 or 4104.

   Each joined row takes the first clause, in statement order, whose family
   and condition match. Conditions within a family are evaluated in order,
   as one CASE, so a later condition never sees a row an earlier clause took.
   A SET or VALUES expression is evaluated only for rows its clause applies
   to. Rows that match no clause take no action.

   The values are converted to the target column's storage type, exactly as
   INSERT and UPDATE convert them. A value too long for its column fails
   with 2628, which ends only the statement, inside a transaction too.

   Everything is materialized once, before any write. A later action
   therefore never changes how another row is classified. For example, an
   UPDATE that changes the join key does not turn its source row into an
   insert.
2. **TOP.** `TOP (n)` and `TOP (n) PERCENT` keep the first n action rows.
   Rows that take no action do not count. Which rows are kept is unspecified,
   as in SQL Server. Error behavior follows `reference/merge-top*.json`:
   - a negative TOP fails with 127;
   - NULL or a non-integer TOP fails with 1060;
   - an out-of-range PERCENT fails with 1031.
3. **Repeated matches.** A target row matched by several source rows fails
   with 8672 before anything is written, but only when one of its actions is
   an UPDATE. Other cases do not fail:
   - Several DELETEs remove the row once and count it once.
   - Several matches where only one selects an action apply that one action.
   - Matches whose conditions select no action change nothing.

   SQL Server ends the batch at 8672 and rolls back the caller's transaction,
   even with XACT_ABORT OFF. msduck does the same.
4. **New images.** msduck builds the complete new row of every selected
   action:
   - an UPDATE row takes the assigned values, and keeps the old values of
     unassigned columns;
   - an INSERT row takes the inserted values, plus the defaults of omitted
     columns, including IDENTITY values;
   - computed columns are evaluated from the other new values;
   - `DEFAULT` in SET or VALUES takes the column default.

   Writing a computed column fails with 271, an explicit IDENTITY value with
   544, and an assignment to an IDENTITY column with 8102. An INSERT column
   count mismatch fails with 109 or 110.
5. **Constraints.** Before writing, the new images are checked against NOT
   NULL and, inside a caller's transaction, PRIMARY KEY and UNIQUE
   constraints. A violation ends only the statement: msduck emits 515 or
   2627, then 3621 ("The statement has been terminated."), and the batch
   continues. The caller's transaction stays usable with its earlier work,
   as SQL Server keeps it with XACT_ABORT OFF. With XACT_ABORT ON, the
   engine's usual rule rolls the transaction back. A statement that swaps
   key values between rows is not a violation.
6. **Writes.** DELETEs run first, then one UPDATE for all UPDATE clauses
   (so a key swap between clauses is valid), then one INSERT per INSERT
   clause. Each is keyed by the target row ids captured
   in step 1. Without a caller's transaction, the statement runs in its own
   backend transaction, so a failure leaves no partial change.
7. **Results.**
   - **Row count.** The DONE row count and `@@ROWCOUNT` are the number of
     rows inserted, updated and deleted, and the DONE CurCmd is 279.
   - **OUTPUT.** OUTPUT rows come from the same images, so `$action`,
     `inserted`, `deleted` and source columns describe one action. `$action`
     is a NOT NULL `nvarchar(10)` column named `$action` when unaliased.
     `inserted` columns are nullable when a DELETE clause exists, `deleted`
     columns when an INSERT clause exists, and source columns when a NOT
     MATCHED BY SOURCE clause exists.
   - **OUTPUT INTO.** OUTPUT INTO writes its rows in the same transaction,
     before the statement completes.
   - **OUTPUT binding.** The OUTPUT projection is bound before the first
     write. A column qualified with anything other than `inserted`,
     `deleted` or the source fails with 4104; that includes the target
     alias.

## Evidence

- `reference/gaps-merge.json` holds 42 observations, captured by
  `scripts/capture-gaps-merge.mjs` in two fresh databases of one
  `mcr.microsoft.com/mssql/server:2022-latest` container (ProductVersion
  16.0.4236.2). The two runs agree once the generated database names are
  bound. `node scripts/capture-gaps-merge.mjs --check` revalidates the
  retained file. It covers:
  - the hinted upsert with and without hints;
  - hint errors;
  - every WHEN family with OUTPUT descriptors;
  - repeated matches, including inside a transaction;
  - CHECK, NOT NULL, key and truncation failures;
  - identity and computed-column errors;
  - EF Core's batched-insert shape;
  - `INSERT DEFAULT VALUES` and `$action` inside an expression.
- The earlier `reference/merge-execution.json`, `merge-top*.json` and
  `merge-transaction.json` captures supply the remaining cases. Those are
  matched updates, inserts and by-source deletes, mixed actions with OUTPUT
  INTO, CTE sources, transaction rollback, TOP, and the 5324 and 10714
  errors.
- `tests/gaps_merge.rs` replays those cases in process and asserts values,
  row counts and error numbers. It also covers snapshot classification,
  self-referencing sources, key swaps and computed columns.
- `tests/compat/merge.test.mjs` runs them through tedious. It checks OUTPUT
  rows and descriptors, parameter binding, EF Core's
  `OUTPUT INSERTED.Id, i._Position`, typed columns (`nvarchar`, `decimal`,
  `bit`, `datetime2`, `uniqueidentifier`), defaults, and MERGE inside IF and
  TRY...CATCH with NOCOUNT.

## Remaining differences and limits

- **Key diagnostics.** 2627 messages keep the backend's wording rather than
  SQL Server's "Violation of PRIMARY KEY constraint 'name'..." text. The
  number, state and class match.
- **CHECK and FOREIGN KEY.** The constraints feature
  (docs/gaps-constraints.md) enforces these around the whole MERGE, after it
  writes. A violation reports SQL Server's 547 message and undoes the
  statement, but ends the batch. Inside a caller's transaction, it leaves
  that transaction uncommittable.
- **Parse-time diagnostics.** Errors found while parsing (1065, 156, 10713,
  10714, 5324) keep the engine's parser prefix and class 16. A hint after the
  alias reports 156 without SQL Server's second error, 319.
- **CurCmd.** A statement-terminating error ends with the DONE CurCmd of
  UPDATE (197), not MERGE (279). The engine's batch loop recognizes only
  INSERT, UPDATE and DELETE as statement-terminating.
- **OUTPUT before 8672.** SQL Server streams the OUTPUT rows it produced
  before 8672. msduck detects 8672 before writing, so it returns none.
- **Other constraint failures.** Key violations outside a caller's
  transaction are detected by the backend while writing. Other failures can also happen after a write began, for example
  an OUTPUT INTO row the sink table rejects.
  - Without a caller's transaction, the statement is still rolled back
    completely.
  - Inside one, msduck cannot undo part of a statement while keeping the
    caller's earlier work. It marks the transaction uncommittable
    (`XACT_STATE() = -1`), and the transaction is rolled back at the end of
    the batch (3998) unless the batch rolls it back first.
- **Triggers.** MERGE writes do not fire triggers.
- **Identity functions.** MERGE inserts do not update `SCOPE_IDENTITY()`
  or `@@IDENTITY`.
- **IDENTITY_INSERT.** The session setting is not consulted, so explicit
  IDENTITY values fail with 544.
- **Unsupported forms.** Targets that are views, table variables,
  temporary tables or tables in another database fail with an explicit
  "unsupported" error or 208. `UPDATE SET` compound operators (`+=`) fail
  to parse (102).
- **Prepared statements.** `sp_prepare` and `sp_prepexec` still reject
  MERGE during preparation, because preparation has no extension hook.
  `sp_executesql` and plain batches work.
- **Qualified source names.** In ON and SET, refer to a table source by
  its name or alias (`src.id`). A schema-qualified column (`dbo.src.id`)
  fails to bind, because the source is wrapped to mark its rows.
- **Collation.** String comparisons in ON and the clause conditions behave
  as they do in a SELECT with the same operands. msduck's current VARCHAR
  comparisons are case sensitive, unlike SQL Server's default collation.
- **Locking.** Locking hints add no locking beyond msduck's session
  serialization. `TOP` selection order is unspecified and may differ from
  SQL Server's.
