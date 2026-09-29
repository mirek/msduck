# IDENTITY_INSERT RPC scope and descending allocator reference

[`reference/identity-insert-rpc.json`](../reference/identity-insert-rpc.json)
retains 42 ordered observations from each of two fresh databases on the pinned
SQL Server 2025 image
`sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`.
The [generator](../scripts/capture-identity-insert-rpc.mjs) sends parameterized
`sp_executesql` through Tedious `execSql` and real `sp_prepare`, `sp_execute`
and `sp_unprepare` through its prepared Request API. It retains rows, full
column descriptors, diagnostics, ordered TDS event kinds, raw DONE status
words/command codes and per-phase RETURNSTATUS values. A second independent
container replayed the same two-database plan byte for byte. The fixture
SHA-256 is
`ebd50b7da9e5ef01270fd0c6007e470e66f23cc97383b36188a804ca8203d8f2`.
The generator validates the retained hash, exact case count, salient values,
diagnostics and completions, and refuses fixture output aliases or unpinned
images. No result or error text is normalized for comparison.

`sp_executesql` executes `SET IDENTITY_INSERT dbo.alpha ON` and returns a
bound marker, but an ordinary batch in the caller immediately afterward still
gets 544 when inserting an explicit ID. The RPC's SET is scoped to the RPC.
The ON response contains DONEINPROC status `0x0001`/command 183, a result
completion status `0x0011`/command 193, RETURNSTATUS 0 and final DONEPROC
status zero/command 224. The corresponding OFF RPC uses command 184. With
the caller's own setting already ON, an inner OFF RPC does not clear it: the
caller next inserts explicit ID 110 successfully. A following RPC inserts
explicit ID 120 and sees `SCOPE_IDENTITY() = 120` and `@@IDENTITY = 120`
inside that RPC. The caller afterward sees `SCOPE_IDENTITY() = 110`,
`@@IDENTITY = 120` and `IDENT_CURRENT('dbo.alpha') = 120`. Thus setting
lifetime and identity scope are distinct from the shared allocation state.

Preparing `SET IDENTITY_INSERT dbo.alpha ON; SELECT @marker` does not turn it
ON in the caller: an explicit insert after preparation still raises 544.
Executing the prepared handle returns marker 9 and an inner SET completion,
but explicit inserts after execution and after unprepare each still raise
544. The prepare phase returns handle 1 through RETURNVALUE, no ERROR token,
and an observed RETURNSTATUS value of **8182**; the subsequent execute and
unprepare phases return 0. The fixture preserves this unusual value without
interpreting it as a generic success or failure code. The prepared execute
retains the same DONEINPROC command 183, result command 193 and final
DONEPROC command 224 as the direct RPC. Each 544 outer-batch failure has
DONE status `0x0002` and command 195.

For `INT IDENTITY(0,-2)`, the first generated value is 0. Explicit -20,
then explicit 5, followed by an ordinary generated insert yields -22: the
later higher value does not move a descending allocator upward. An explicit
-100 inserted inside a transaction and then rolled back leaves no -100 row,
yet `IDENT_CURRENT` becomes -100 and the next generated ID is -102.
The final query confirms `@@TRANCOUNT = 0`, `XACT_STATE() = 0` and the
connection remains reusable. These are observed values for a descending
integer identity; other widths and failure paths need separate evidence.

The earlier [batch/session capture](identity-insert-reference.md) establishes
two-connection independence, one ON table per session and the 544/545/8101/8107
diagnostics. The [runtime plan](identity-insert-runtime-plan.md) maps the
current Rust source and reserved files. These captures do not mean msduck
implements RPC-scoped SET state or explicit-value advancement. Runtime work
must save and restore the caller's active ON table at the RPC boundary without
rolling back shared identity allocation, and must preserve scoped versus
session identity values. This fixture does not cover RPC failure cleanup,
trigger/procedure nesting, `DEFAULT VALUES` while ON, failed explicit-write
allocation, temporary tables or reconnect behavior.
