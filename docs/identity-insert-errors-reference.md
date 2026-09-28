# IDENTITY_INSERT error and allocation reference

[`reference/identity-insert-errors.json`](../reference/identity-insert-errors.json)
retains 34 ordered batch observations from each of two fresh databases on the
pinned SQL Server 2025 image
`sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`.
The [generator](../scripts/capture-identity-insert-errors.mjs) retains exact
rows, full column descriptors, error number/state/class/message, ordered TDS
event kinds, and raw DONE status words and command codes. A separate container
replayed the two-database plan successfully. The fixture SHA-256 is
`45ae04f29a976b6b38866ea4c8142a8d28c354792b787c816d59d6471fa9b6ee`.
The validator checks that hash, the case plan, all expected diagnostics and
completions, typed state descriptors, rows and allocator values. It refuses
fixture output aliases, symlinks, hard links, and unpinned images. CHECK and
NOT NULL errors name each random fresh database; the fixture retains both raw
names and masks only that generated substring during run-equivalence checks.

For `INT IDENTITY(10,2)`, the baseline generated row is `(10,1)`. Setting
`IDENTITY_INSERT dbo.alpha ON` twice succeeds with DONE status zero and command
183 both times. Setting `dbo.beta OFF` while alpha is ON succeeds with command
184 and does not clear alpha's setting. ON for a table without an identity
column fails with 8106/state 1/class 16, and ON for a missing table fails with
1088/state 11/class 16; both have DONE status `0x0002` and command 253.
`INSERT dbo.alpha DEFAULT VALUES` while ON fails with 545/state 1/class 16,
DONE `0x0002`/command 195. Neither error path adds a row or moves
`IDENT_CURRENT` from 10.

An explicit `(50,2)` succeeds and moves `IDENT_CURRENT` to 50. Failed explicit
values then move it to 100 after a UNIQUE violation (2627/state 1/class 14),
200 after a CHECK violation (547/state 0/class 16), 300 after a NOT NULL
violation (515/state 2/class 16), and 400 after a source conversion error
(245/state 1/class 16). Each failure leaves the row set at `(10,1),(50,2)`.
The first three failures have DONE `0x0002`/command 195; source conversion has
DONE `0x0002`/command 253. The conversion result is particularly relevant:
an explicit identity value can advance the allocator even when a later source
expression fails before a row is inserted.

An explicit `(500,3)` inside a transaction is visible before rollback, then
absent afterward; `IDENT_CURRENT` remains 500. Repeated OFF succeeds with DONE
zero/command 184. The next generated row is `(502,4)`, and the final query
shows no active transaction. State queries retain `IDENT_CURRENT` as typed
`VARCHAR(40)`, `@@TRANCOUNT` as nonnullable `INT`, and `XACT_STATE()` as
nullable `SMALLINT` (`IntN`, length 2) over TDS.

This is first-party reference evidence, not a claim that msduck implements
these semantics. The [batch/session capture](identity-insert-reference.md)
covers concurrent sessions and one-ON-table rules; the
[RPC capture](identity-insert-rpc-reference.md) covers nested execution scope
and a descending identity. This capture does not cover multi-row statement
atomicity, triggers, bulk copy, noninteger identity types, overflow, or an
explicit identity value whose conversion fails before reaching the allocator.
