# Correlated APPLY over FULL OUTER JOIN bodies

Issue #867 (umbrella #864). DuckDB cannot flatten a FULL OUTER JOIN inside a
lateral subquery when its operands or condition reference the enclosing
query. It used to fail with `Unsupported join type for flattening correlated
subquery` or `Non-inner join on correlated columns not supported`. A common
case is an inline function that diffs two JSON documents:

```sql
CREATE FUNCTION dbo.foo(@lhs NVARCHAR(MAX), @rhs NVARCHAR(MAX)) RETURNS TABLE AS RETURN (
  SELECT COALESCE(l.[key], r.[key]) AS [key], l.[value] AS old_value, r.[value] AS new_value
  FROM OPENJSON(@lhs) l FULL OUTER JOIN OPENJSON(@rhs) r ON l.[key] = r.[key]);

SELECT i.id, x.[key], x.old_value, x.new_value
FROM items i CROSS APPLY dbo.foo(i.lhs, i.rhs) x;   -- (1, 'a', '1', '2')
```

## Lowering

`apply::lower` makes each APPLY body lateral. It now also rewrites every
correlated FULL JOIN inside that body, including folded inline functions,
nested derived tables and subqueries. The rewrite is in
`crates/msduck-sql/src/apply/full_join.rs`:

```sql
FROM A FULL JOIN B ON c [rest] WHERE w
-- becomes
FROM (SELECT 1 AS side WHERE EXISTS (SELECT 1 FROM A)
      UNION ALL SELECT 2 WHERE EXISTS (SELECT 1 FROM B)) AS s(side)
LEFT JOIN A ON s.side = 1
LEFT JOIN LATERAL (SELECT * FROM B WHERE (s.side = 1 AND (c)) OR s.side = 2) AS b ON TRUE
[rest]
WHERE (s.side = 1 OR NOT EXISTS (SELECT 1 FROM A WHERE c)) AND (w)
```

- Side 1 is `A LEFT JOIN B ON c`.
- Side 2 is every row of B that no row of A matches.
- A side row exists only when its operand has rows, so an empty or NULL
  document never produces a NULL-extended phantom row.
- `c` is evaluated unchanged, so duplicates multiply and NULL keys never
  match, as in SQL Server.
- Both operands keep their aliases. Qualified and unqualified columns,
  `alias.*`, GROUP BY, aggregates, TOP and ORDER BY keep binding as before.
- An unqualified `*` excludes the internal side column by its qualified
  name. The side alias avoids every relation name and qualifier in the
  SELECT, so it never shadows an outer alias.
- An unaliased table B uses its base name as the derived alias, and an
  unaliased function such as OPENJSON gets a fresh one. When the
  SELECT uses three-part names, or B is an unaliased parenthesized join, B
  joins with `ON (s.side = 1 AND (c)) OR s.side = 2` instead. That form
  fails in DuckDB when `c` references the outer query.

The following FULL JOINs are left unchanged:

- Joins with no outer reference, which DuckDB runs natively.
- Joins outside APPLY bodies.
- Joins with volatile functions (NEWID, RAND, NEWSEQUENTIALID,
  CRYPT_GEN_RANDOM) or TABLESAMPLE operands, because A, B and `c` are
  evaluated more than once.
- Joins in a FROM item that later has a RIGHT or FULL join.
- A second FULL JOIN in the same FROM item.
- FULL JOINs inside parentheses.

The unchanged cases still fail with DuckDB's error.

The lowering has no catalog, so it cannot see volatility hidden inside a
named view operand (for example a view filtered by NEWID()). Such a view is
evaluated more than once after the rewrite, and the evaluations can
disagree. Base tables, which are the common case, are unaffected.

A join counts as correlated when it uses a qualifier that its operands do
not define in scope, or any unqualified column in its operands or condition
(for example `OPENJSON(lhs)`). Without a catalog, an unqualified column may
belong to the enclosing row. Rewriting a join that is in fact uncorrelated
gives the same rows, so only fully qualified uncorrelated joins stay
native.

## Evidence

- `scripts/capture-apply-full-join.mjs` captures 24 cases from the pinned
  SQL Server 2025 reference image (17.0.4065.4) into
  `reference/apply-full-join.json`. Two fresh captures were identical.
- The cases cover CROSS and OUTER APPLY of the function and of a derived
  table, and keys on one side only. They also cover NULL and empty documents,
  NULL and duplicate join values, aggregates, TOP, `*` and `alias.*`, outer
  references in the condition, and an uncorrelated FULL JOIN. Error cases
  are 8134 inside the body and an ambiguous column. A multirow AFTER UPDATE
  trigger diffs `FOR JSON` snapshots of `inserted` and `deleted` through the
  function.
- `tests/compat/apply_full_join.test.mjs` replays every case through tedious.
  It compares column names, types and lengths, rows, errors and DONE counts.
- `tests/apply_full_join.rs` checks row summaries, the 8134 error
  number, state and class, and the trigger, all with in-process sessions.
- `crates/msduck-sql/tests/apply_full_join.rs` checks the rewritten tree.

## Remaining differences

These differences are outside the APPLY lowering. The tedious test lists
them exactly:

- `COALESCE(l.[key], r.[key])` over OPENJSON keys is reported as
  `nvarchar(max)`; SQL Server reports `nvarchar(4000)`. This also happens
  without APPLY.
- The OPENJSON `type` column is `int`; SQL Server reports `tinyint`.
- `SELECT *` over both sides inside a derived table returns duplicate `key`
  columns; SQL Server raises 8156. Duplicate derived column names are not
  validated in general.
- An ambiguous unqualified column returns DuckDB's binder error as 50000;
  SQL Server raises 209. This also happens with an INNER JOIN of two
  OPENJSON calls.
