# SET IDENTITY_INSERT reference

[`reference/identity-insert.json`](../reference/identity-insert.json) retains 34
ordered observations from each of two fresh databases on the pinned SQL Server
2025 image `sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`.
The reproducible [capture](../scripts/capture-identity-insert.mjs) records rows,
complete Tedious column descriptors, diagnostics, ordered token kinds, raw DONE
status words and command codes. The fixture SHA-256 is
`b4ad94e51ba1d5c11ba9c983126ce88f5052ec1816846743140621a9ff77ef18`.
Two further fresh databases in an independent container matched all observations
after excluding only their randomly generated database name from error 8107
*during comparison*. The fixture retains each original error message unchanged.

Both connections can turn `IDENTITY_INSERT` ON for the **same** table at once.
Connection B cannot insert an explicit identity while its setting is OFF, even
when connection A has it ON: B receives 544/state 1/class 16 and DONE status
`0x0002`, command 195. B can then use a bracket-qualified
`SET IDENTITY_INSERT [dbo].[alpha] ON` and insert its own explicit value.
Each connection is restricted to one ON table: an attempt to turn it ON for
`dbo.beta` while `dbo.alpha` is ON yields 8107/state 1/class 16, with DONE
status `0x0002`, command 253. A failed switch leaves the original table ON.

For `dbo.alpha`, an explicit value without a column list yields 8101 even
while the setting is ON. With a column list, explicit IDs 100, 200 and then 5
are inserted. The lower value does not reduce the allocator: after switching
OFF, the next implicit ID is 201. For `dbo.beta`, the explicit ID 50 advances
the next implicit ID to 52. While either table's setting is ON, an insert that
omits its identity column fails with 545/state 1/class 16. Turning the setting
OFF restores implicit insertion; explicit insertion then fails with 544.

`BEGIN TRANSACTION; SET IDENTITY_INSERT dbo.alpha ON; ROLLBACK TRANSACTION`
returns to transaction count and state zero, yet a subsequent explicit insert
of ID 300 succeeds. The setting thus outlives this transaction rollback in the
observed session. Turning it OFF afterward again produces 544 for an explicit
insert. The fixture preserves the corresponding rows, `IDENT_CURRENT` values
and all three transaction-batch completions. Successful ON and OFF each have a
single DONE token with status `0x0000` and command 183 or 184, respectively.

This capture does not establish behavior for missing tables, permission errors,
temporary tables, triggers, procedures, reconnects, prepared RPCs, decimal
identity columns, replication, or every transaction/error combination. It is
not evidence that msduck implements the setting. The current
[`src/identity.rs`](../src/identity.rs) rejects explicit identity inserts, and
[`docs/identity.md`](identity.md) lists `SET IDENTITY_INSERT` as unfinished.
Runtime integration needs session-local ON-table state, table-name resolution,
statement diagnostics, and allocator advancement after successful explicit
values. In particular, a backend sequence default cannot determine whether
an omitted identity must raise 545 without the session setting.
