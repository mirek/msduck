# Prepared RPC metadata

`sp_prepare` descriptions use the original SQL AST, an explicit catalog snapshot
and parameter declarations. Preparation validates native binding but never steps
the statement. Bound values, returned rows and volatile expression evaluation do
not supply metadata. Effects stay in the root RPC adapter.

The retained fixture `reference/prepared-rpc-metadata.json` contains two matching
fresh pinned SQL Server 2025 captures, with 50 records per run: version/setup and
eight profiles across six preparation variants. Its SHA-256 is
`3018bee5ac4d4bf2019c4238e174d1f83e60842bbcb986682722961be098d2d0`.
Run `node scripts/capture-prepared-rpc-metadata.mjs --check` to validate it.
The generator refuses to replace the immutable fixture.

The profiles cover COUNT/COUNT_BIG, ROW_NUMBER, typed columns/constants, empty
results, INSERT, multiple results and seeded RAND. Each retains preparation,
four executions using NULL/0/9/1, unprepare and before/after state probes.
Omission and explicit option 1 produce metadata; typed RPC options 0, 2 and NULL
produce error 214/state 3, return status 214 and a NULL handle without allocating
one. Two SELECT results produce status 8182 with a valid reusable handle and no
preparation metadata. INSERT preparation emits completion without inserting.
Preparation does not advance the captured RAND stream.

Known single-result declarations produce COLMETADATA, any proven ORDER token,
DONEINPROC, RETURNSTATUS, RETURNVALUE and DONEPROC in captured order. Types,
capacity, scale, precision, flags and collation come from declarations. Unknown
result types or ORDER plans are rejected explicitly. Other non-result/control
flow shapes retain their existing preparation path; their descriptions remain
unproven. This is not complete prepared-statement compatibility.

Run the dedicated tests explicitly:

```
node --test tests/prepared_rpc_metadata_reference.test.mjs tests/prepared_rpc_metadata.test.mjs
```

The public test compares complete preparation responses and execution rows and
descriptors, including NULL and empty results. It retains complete raw responses
in `artifacts/prepared-rpc-metadata/runtime.json`; execution ORDER/completion and
unprepare differences remain separate follow-up work. The version probe also
retains the unsupported SERVERPROPERTY response instead of treating it as a
SQL Server version. The reference captures still require the pinned version.
The package test inventory is separately claimed, so these tests are invoked
explicitly alongside the default clients.

SQL invocation of preparation, wider control flow, session diagnostic-state
effects, typeless NULL error precedence, all unproven ORDER shapes and wider
type coverage need further ground truth and integration. `sp_prepexec` and
`sp_unprepare` keep their existing execution behavior. No changes to engine.rs,
root module registration or catalog acquisition are part of this task.

The `--regressions PATH` capture mode retains a separate preparation-only
plan of 23 profiles across API-default and named option 1. Two fresh matching
48-record runs are retained in the Linux ignored artifact
`artifacts/prepared-rpc-metadata/regression-reference.json` (SHA-256
`5f71ef2b908936b40784b5f2b59c56a8fdf5ffd2d3c7b5daae7d5e71d229563c`). The public scalar regression test embeds the complete
preparation responses for calendar, mixed BIT/integer bitwise, catalog, identity, JSON-presence and bare
NULL projections from that capture. Its smaller plan asserts consecutive handles
starting at 1; every other preparation response field is compared verbatim.
Both plans verify preparation leaves table rows, seeded RAND and transaction
depth unchanged. INSERT OUTPUT INTO retains its captured no-result completion
command without performing either write. Single-table CTE DELETE uses the bounded
root adapter described below and retains the captured no-result command 196.
INSERT OUTPUT uses the existing pure logical projection over target catalog
declarations and retains the INSERT completion command. Other captured regression
shapes remain implementation work, including wider ORDER and temporal
derived declarations. The preparation ORDER fallback preserves existing proven
plans and uses original typed source identities for direct columns, projected
INT-to-variant casts and captured SUM keys. It rejects unresolved/computed keys,
ambiguous aliases and hidden DISTINCT keys. Captured INT-column variant casts
retain preparation fComputed even though execution inference omits it.

DATETIME2 conditionals use explicit contributing declarations: ISNULL retains
the first argument scale, while CASE/COALESCE use the largest known DATETIME2
scale. Any unresolved or mixed-family contributing branch remains a barrier.
The retained ISNULL scale-2/scale-7 response is compared verbatim. This does not
add temporal derived-table/set type inference.

A bare projected NULL is declared INT, while NULL function operands retain their
original declaration barriers. VARBINARY(MAX) uses the existing PLP binary codec.


## Prepared single-table CTE DELETE

Companion task #704 adds `src/rpc/prepare_delete.rs` and the immutable
`reference/prepared-cte-delete.json` (SHA-256
`82b301242462bc0ce227173523a96d4b06b15ddf85ec1c8c2fdd9c4ad0120a81`). Two fresh
pinned SQL Server runs agree on all ten records per run: version/setup and plain
or qualified source profiles, through API/named preparation, reuse and rollback.
The capture mode is `--cte-delete PATH`.

The adapter accepts one nonrecursive CTE over a plain single-table wildcard
projection, optional source alias and predicate, with no other query/DELETE
modifiers. It preserves the base relation, alias and predicate once as AST nodes.
A same-named unqualified source remains a self-reference barrier. The existing
native binder validates the equivalent DELETE without stepping it before a
handle is allocated. Original logical SQL supplies preparation framing; the
validated transformed SQL is cached for execution and its actual byte length
counts toward capacity. Wider shapes retain their original binding behavior.
Ordinary batch CTE writes and `sp_prepexec` are not adapted here.

At implementation checkpoint `45d61e5`, all 32 complete prepared DELETE execution
responses match SQL Server verbatim. Parameters NULL, 9, 1 and 0 produce counts
0/0/3/1 for reuse and 0/0/3/4 when each execution is rolled back. All preparation,
unprepare, before/after table rows, descriptors and transaction depths match.
The complete raw runtime comparison is retained in
`artifacts/prepared-rpc-metadata/cte-delete-runtime.json`.

The strict rollback test still fails on ordinary SQL transaction wire framing:
BEGIN/ROLLBACK advertise DONE command 0 instead of the captured 212/210. These
are tracked in owner-approved backlog issue #705; no transaction semantics are
normalized away. Surrounding ordered row snapshots also retain the existing
missing ORDER / ROW instead of NBCROW gaps. The public comparison remains
failing on those exact surrounding responses, so this checkpoint is not merge
ready and does not establish complete CTE or transaction compatibility.
