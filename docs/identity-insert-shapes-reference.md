# IDENTITY_INSERT INSERT-shape reference

The [retained fixture](../reference/identity-insert-shapes.json) contains 34
ordered observations in each of two fresh databases on the pinned SQL Server
2025 image (`sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`).
The [capture script](../scripts/capture-identity-insert-shapes.mjs) also ran in
an independent container and matched the retained runs. Fixture SHA-256:
`425141ff3b10e7af1c0d7e6ee5615d5f7afb9d1bd4fc87e34892b79cb357dcd8`.
The script preserves raw rows, typed descriptors, diagnostics, ordered TDS event
kinds and DONE status/command words. Its validator pins the case plan,
diagnostics, state and allocator values; `--check` verifies the fixture hash.
Live capture refuses output aliases, symlinks, hard links and an unpinned image.

The table has `id INT IDENTITY(10,2)` and a non-null `v`. A baseline implicit
insert produces ID 10. `INSERT dbo.alpha VALUES(20,2)` and
`VALUES(DEFAULT,3)` both fail with 8101/state 1/class 16 and DONE status
`0x0002`/command 253 **while OFF**. They produce the same result while ON.
Neither changes rows or `IDENT_CURRENT`. SQL Server requires an explicit column
list for this positional source shape regardless of the setting. This is a
specific gap in the current [deterministic gate](../crates/msduck-sql/src/identity_insert_gate.rs):
its OFF/no-list path currently returns `Generated` before inspecting source
arity or values, and its ON/no-list `DEFAULT` path returns `Unsupported`.

An explicit listed identity while OFF wins over a competing conversion error:
`INSERT dbo.alpha(id,v) VALUES(30,CONVERT(INT,'bad'))` returns 544 and leaves
`IDENT_CURRENT` at 10. While ON, `INSERT dbo.alpha(v) SELECT v ...` and an
omitted-identity SELECT containing a failing conversion both return 545 before
source execution. Their failed completions use command 195. Duplicate listed
`id` columns return 264, and an invalid listed column returns 207; these use
command 253 and leave the allocator at 10. The current gate deliberately
returns `Unsupported` for duplicate and unresolved columns so the root binder
must supply those diagnostics without fabricating precedence.

With ON and an explicit listed identity, a conversion failure in `v` returns
245/state 1/class 16, DONE `0x0002`/command 253. No row appears, but
`IDENT_CURRENT` advances from 10 to 70. An explicit `INSERT ... SELECT` then
inserts ID 80; after OFF, an omitted-identity `INSERT ... SELECT` generates ID
82. The final rows are `(10,1)`, `(80,2)` and `(82,2)`. These observations
confirm that successful explicit SELECT and ordinary generated SELECT both
need the same shared allocator; a failed later expression can advance it even
without publishing a row.

This is SQL Server ground truth for the tested statements, not evidence that
msduck executes them yet. Runtime integration still needs session setting,
catalog resolution, source evaluation and native sequence coordination. The
capture does not establish error precedence for differing source arities,
multi-row mixtures of `DEFAULT` and explicit values, source subqueries with
side effects, trigger effects or races with another connection.
