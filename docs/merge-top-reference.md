# SQL Server `MERGE TOP` reference

`reference/merge-top.json` retains 73 ordered observations from each of two fresh
databases in the pinned SQL Server 2025 image
`mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`
(ProductVersion `17.0.4065.4`, database collation
`SQL_Latin1_General_CP1_CI_AS`). A second fresh container with two more
databases matched every raw observation in this capture. The committed raw
fixture SHA-256 is
`24d768df7bc1dd7bc6d7d9ef539e9040eca150bfd4d67ca4aa08feaaca39a49e`.
Each record retains the original query, full ordered TDS descriptors and rows,
errors, information and DONE events, subsequent `@@ROWCOUNT`, and ordered final
target rows. There is no action-row sorting or inferred TOP tie-breaker in the
fixture.

The mixed case starts with target `(1,10),(2,20),(3,30)` and source
`(1,11),(2,22),(4,44)`. Its eligible actions are UPDATE 1, DELETE 2,
DELETE 3 by source, and INSERT 4. These are the observed results in all four
fresh databases, not guaranteed action-selection or output order:

| Probe | MERGE DONE / next `@@ROWCOUNT` | Observed OUTPUT actions | Final target |
| --- | --- | --- | --- |
| `TOP (0)` | `0` / `0` | none | `(1,10),(2,20),(3,30)` |
| `TOP (1)` | `1` / `1` | UPDATE 1 | `(1,11),(2,20),(3,30)` |
| `TOP (2)` | `2` / `2` | UPDATE 1, DELETE 2 | `(1,11),(3,30)` |
| `TOP (10)` | `4` / `4` | UPDATE 1, DELETE 2, DELETE 3, INSERT 4 | `(1,11),(4,44)` |
| reversed source, `TOP (1)` | `1` / `1` | INSERT 4 | `(1,10),(2,20),(3,30),(4,44)` |
| reversed source, `TOP (2)` | `2` / `2` | UPDATE 1, DELETE 2 | `(1,11),(3,30)` |

The reversed source changed the observed `TOP (1)` action, but does not prove
that source order determines selection. The capture validates that each selected
action and final row state agree, while comparing only count/descriptor
invariants across databases. It deliberately permits a future fresh run to
choose other eligible actions or return them in another order without erasing
that difference from its raw artifact.

An ON-matched row whose only `WHEN MATCHED` predicate is false produced no
action: with that sole candidate, `TOP (1)` returned an empty OUTPUT result
and both count observations were `0`. With a second, source-only candidate,
`TOP (1)` selected INSERT 4 in these runs; `TOP (2)` still affected only that
one row. The count can be below the TOP limit when a selected joined row has
no qualifying action.

For two source rows matching one target, `TOP (1)` updated it once to `n=11`.
`TOP (2)` raised `8672`, state `1`, class `16`, and left the target at `n=10`;
the next `@@ROWCOUNT` was `0`. The **direct OUTPUT stream contained one UPDATE
row before the error** even though the target write rolled back. A later
`OUTPUT INTO` sink has a distinct atomicity contract (see
`docs/merge-transaction-reference.md`); an implementation must not equate
already emitted direct rows with committed writes.

`TOP (1+1)`, `TOP (50) PERCENT`, and `TOP (@n)` with `@n=2` each inserted two
of three source rows in these captures. The variable batch emitted a separate
DECLARE DONE before the MERGE DONE. `TOP (-1)` raised `127/state 1/class 15`;
`TOP (NULL)` and `TOP (1.5)` each raised `1060/state 1/class 15`. All three
error forms left the target empty. These probes establish only the shown
expressions and percentage; they do not settle arbitrary expression types,
rounding boundaries, or parameter binding.

Direct `OUTPUT $action, inserted.id, deleted.id` retained a descriptor even
for `TOP (0)` and the nonqualifying empty result. `$action` was
`NVarChar` length 20 bytes, flags `0`, with database collation. In the mixed
case both image IDs were nullable `IntN(4)` with flags `9`; the single-action
nonqualifying case instead declared `inserted.id` as fixed `Int`, flags `8`.
Metadata must be bound from the action set and logical source declarations,
not inferred from the rows returned by a particular TOP selection.

The fixture is first-party reference evidence, not runtime MERGE support.
`msduck` still needs lossless TOP syntax/binding, evaluated count and percent
rules, atomic action execution, direct OUTPUT error ordering, sink behavior,
session/DONE integration and TDS descriptors. The existing action-selection
core can accept an explicit candidate subset, but this capture does not define
how SQL Server orders or locks candidates under concurrency. No concurrent
writer, index-plan variation, percentage boundary, or parameter-type matrix
was captured here.

To recapture on a machine with Docker and Node.js 24+, run
`node scripts/capture-merge-top.mjs artifacts/compatibility/merge-top/recheck.json`.
The generator starts its own pinned container, uses two fresh databases,
retains unmodified raw observations under ignored `artifacts/`, validates
action/final-state consistency, and compares only stable invariants with the
retained fixture. It refuses to overwrite that fixture. The independent
recapture above used the same command in a new container on `linux.local`.
