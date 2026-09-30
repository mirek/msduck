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
