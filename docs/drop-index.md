# DROP INDEX binding evidence

`reference/drop-index.json` retains 24 first-party SQL Server observations,
each captured identically in two fresh databases against the pinned SQL Server
image. Each case includes the complete diagnostics, completion tokens,
`@@ROWCOUNT`/`@@ERROR`, and remaining index inventory. Regenerate and compare:

```sh
node scripts/capture-drop-index.mjs
```

The script writes raw repeated captures before comparison. During initial
capture, an absent fixture permits writing the candidate to the artifact
directory; it never overwrites the checked-in fixture automatically.

The captures establish:

- Index identity includes the owning table. Dropping `ix` on `dbo.a` preserves
  the same name on `dbo.b` and `alt.a`; a wrong-table index must not be dropped.
- Missing indexes and wrong targets produce 3701/state 7/class 11. Missing
  tables or schemas produce 3701/state 6/class 11. The message includes the
  requested table/index path. `IF EXISTS` suppresses all these missing cases.
- Multiple targets execute in order. An earlier successful drop survives a
  later missing-index failure. A first missing target prevents later drops.
  Repeating one target drops it once, then emits 3701/state 7.
- Quoted dots remain part of identifiers. Legacy `dbo.a.ix` syntax succeeds;
  a lone index name emits 159, and `dbo.ix ON dbo.a` emits syntax error 156.
- For this nonclustered index, `MAXDOP = 1` emits 3748 while `ONLINE = OFF`
  succeeds. Other options and clustered index behavior are not established.

The deterministic binder and its tests remain implementation work. Its adapter
contract must accept explicit table/index identity records and return ordered
operations with a possible terminal diagnostic. It must not silently strip ON,
look up an index by name alone, fabricate success for unknown catalog state, or
collapse quoted identifiers. Execution must preserve successful earlier drops
when a later binding failure occurs; wrapping the entire statement in a
transaction that rolls back those drops would contradict the captures.

This task does not edit the SQL export module or root engine. Root integration,
transaction policy, catalog acquisition and end-to-end execution verification
remain separately owned follow-up work. The captures do not establish complete
DROP INDEX compatibility.
