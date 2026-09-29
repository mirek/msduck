# IDENTITY_INSERT multi-row and OUTPUT reference

[`reference/identity-insert-multirow.json`](../reference/identity-insert-multirow.json)
retains 45 ordered observations from each of two fresh databases on the pinned
SQL Server 2025 image
`sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`.
The [generator](../scripts/capture-identity-insert-multirow.mjs) retains exact
rows, complete column descriptors, diagnostics, ordered TDS event kinds and raw
DONE status words/command codes. A second independent container replayed the
same two-database plan. The fixture SHA-256 is
`f8d284ef1ab7c5bce67bf67edc32b31c50fdcf97655488b4eefaac467e5c1498`.
The validator checks the case plan, outcomes, descriptors, completions, token
order and checksum; it refuses fixture output aliases and unpinned images.
The CHECK error names each random database. Both raw messages remain in the
fixture; only that generated substring is masked for run-equivalence checks.

With `INT IDENTITY(10,2)`, a generated baseline row has ID 10. While
`IDENTITY_INSERT` is ON, a two-row `VALUES` insert with `OUTPUT inserted.id,
inserted.v` emits `(20,2)` and `(30,3)`, commits both rows and moves
`IDENT_CURRENT` to 30. A two-row `INSERT ... SELECT` without `OUTPUT` commits
`(40,4)` and `(50,5)`, returns no result set and moves the allocator to 50.
Both successful inserts end with DONE status `0x0010`, command 195, row count
2. The OUTPUT descriptors are nonnullable `Int` columns; the identity column
has flags 24 and `v` has flags 8.

A later-row UNIQUE violation in `VALUES(60,6),(70,2)` emits an OUTPUT result
set containing `(60,6)` **before** the ERROR token. The ordered events are
COLMETADATA, ROW, ERROR, INFO, DONE. No row from that statement is present in
the next table snapshot, but `IDENT_CURRENT` is 70; after OFF, the next
generated row is `(72,7)`. A later-row CHECK violation in
`VALUES(80,8),(90,-1)` emits no row set, leaves neither row in the table,
advances `IDENT_CURRENT` to 90 and yields generated `(92,9)`. A later-row
source-conversion failure in `VALUES(100,10),(110,CONVERT(INT,'bad'))` also
leaves neither row, advances the allocator to 110 and yields `(112,11)`.
The UNIQUE and CHECK failures have DONE status `0x0002`/command 195; the
conversion failure uses command 253.

The same distinction holds for `INSERT ... SELECT` with `OUTPUT`: when the
ordered source contains `(120,12)` followed by duplicate value `(130,2)`, the
stream emits OUTPUT `(120,12)`, then ERROR and INFO, then failed DONE. Neither
source row remains stored. `IDENT_CURRENT` becomes 130 and the next generated
row is `(132,13)`. Both failed OUTPUT cases have a NULL DONE row count despite
the preceding ROW token. A consumer must not interpret emitted OUTPUT rows as
committed rows after a failed statement.

A successful two-row explicit insert inside a transaction emits both OUTPUT
rows and makes `(150,15),(160,16)` visible within that transaction. Rollback
removes both rows but leaves `IDENT_CURRENT` at 160. After OFF, the next
generated row is `(162,17)`. The final query has no active transaction and the
session is reusable.

This is first-party SQL Server evidence, not an msduck runtime claim. The
[setting/error capture](identity-insert-errors-reference.md) covers single-row
failure allocation; the [batch](identity-insert-reference.md) and
[RPC](identity-insert-rpc-reference.md) captures cover session and nested
setting scope. This capture does not establish trigger behavior, MERGE,
multi-row `OUTPUT INTO`, bulk copy, parallel source plans, cross-session
concurrent inserts, or allocator behavior after overflow. Root INSERT/session
integration must preserve both statement atomicity and the observable OUTPUT
stream on a later failure.
