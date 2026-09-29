# IDENTITY_INSERT target-name diagnostics

[`reference/identity-insert-name-errors.json`](../reference/identity-insert-name-errors.json)
retains 20 ordered observations from each of two fresh databases on the pinned
SQL Server 2025 image
`sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`.
The [capture script](../scripts/capture-identity-insert-name-errors.mjs) records
complete result sets and descriptors, errors, ordered TDS event kinds, and raw
DONE status and command words. It validates the fixed case plan and exact
diagnostics, compares both runs without normalizing names or messages, checks
the retained fixture SHA-256
`4bae40ae8637a86a86a4807a45d6137129fa6c6da7c4c54fab0c4505d7da805c`,
and refuses output aliases, hard links, symlinks and unpinned images.

With both `dbo.plain` and `other.plain` present, `SET IDENTITY_INSERT plain ON`
returns 8106/state 1/class 16 with the message
`Table 'plain' does not have the identity property. Cannot perform SET
operation.` The unqualified missing target returns 1088/state 11/class 16
with `Cannot find the object "missing" because it does not exist or you do not
have permissions.` Both have ordered `ERROR`, `DONE` events and raw DONE
status `0x0002`, command 253. The server keeps the requested name's spelling
in the error rather than expanding it to `dbo.<name>`.

Explicit `dbo.plain` and `dbo.missing` diagnostics contain those two-part
names. Brackets disappear from the displayed name: `[plain]` reports `plain`
and `[dbo].[plain]` reports `dbo.plain`. Case is retained even though the
lookup succeeds case-insensitively in this database: `[DbO].[PLAIN]` reports
`DbO.PLAIN`, and `[DbO].[MISSING]` reports `DbO.MISSING`. Explicit
`other.plain` and `other.missing` errors use those requested schema names.
All twelve error cases preserve the same error code/state/class and raw DONE
status/command for their missing or nonidentity category. Unqualified `ident`
ON succeeds with DONE command 183; `[DbO].[IDENT]` OFF succeeds with command
184. A final `SELECT 1` returns a typed `INT` row, proving the connection
remained reusable after the errors.

These observations establish the listed name forms only. They do not prove
three- or four-part names, non-default schemas for unqualified lookup,
temporary tables, permission-denied objects, or case-sensitive database
collations. The current root resolver's `Unsupported` result for unqualified
missing/nonidentity targets can be replaced in a separately claimed runtime
change using this fixture. This reference capture does not wire `SET
IDENTITY_INSERT` into server execution.
