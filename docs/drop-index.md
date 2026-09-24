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

The isolated deterministic module provides `parse` for one complete statement
and `bind` over a complete caller-supplied table/index catalog. The caller also
supplies schema search order and identifier equality, so neither collation nor
default schema is hidden in the binder. Backend index names are explicit AST
identities supplied by the catalog; requested ON qualifiers are never stripped
to derive a backend name. Quoted dots and backend identifier escaping survive.

The plan contains ordered backend DROP statements and an optional terminal
diagnostic. Execute the successful prefix, then emit that diagnostic. Preserve
successful earlier drops when a later binding failure occurs; wrapping the
entire statement in a transaction that rolls them back would contradict the
captures. Ordinary backend failures still stop execution and need adapter-owned
error handling. A plan does not itself execute, commit or emit completion tokens.

The binder rejects ambiguous catalog records. Incomplete snapshots are invalid
inputs, never evidence of absence. Cross-database names, options beyond the
captured forms, clustered-index options and constraint-backed indexes remain
explicitly unsupported. Unsupported inputs do not yield an executable plan.
The tests compare exact captured diagnostics and remaining index inventories
for all 24 cases, plus explicit identifier comparison, schema precedence,
ambiguous catalogs and escaped backend identities. Tests include the module by
path until the separately owned SQL export integration is ready.

This task does not edit the SQL export module or root engine. Root integration,
transaction policy, catalog acquisition and end-to-end execution verification
remain separately owned follow-up work. The captures do not establish complete
DROP INDEX compatibility.
