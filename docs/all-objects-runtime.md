# Built-in object catalog runtime

`sys.objects`, `sys.system_objects`, and `sys.all_objects` use separate membership
from the [owner SQL Server capture](../reference/all-objects.json). The generated
seed in `src/object_catalog/system_objects.json` contains the captured 118
shipped `objects` rows and 2,624 `system_objects` rows. The generator compares
both retained fresh-database runs by object ID, all stable fields, and explicit
membership before writing the seed. The only changing baseline field is the
create/modify clock of `sys.wpr_bucket_table`; a newly opened msduck database
gives that row its own initialization clock.

The server stores these rows in a database-owned table. `sys.objects` unions
the shipped `objects` membership with transactionally synchronized user
tables/views. `sys.system_objects` selects the disjoint system membership, and
`sys.all_objects` unions those two views. A rollback removes only the pending
user rows. Reopening a database keeps the same built-in and user identities;
the built-in table is seeded only when empty. Object identity/name lookups use
the complete union.

Seeding this inventory increases fresh-server test startup cost on GitHub's
two-worker runner. Its public client tests keep the same assertions but use
fourfold connection, request, and test deadlines there; the original deadlines
remain on other machines. The candidate loader builds one typed Arrow record batch
and appends it to the transaction-owned seed staging table instead of calling
the DuckDB row appender 2,742 times. The final timestamp cast, source membership
and committed public table remain the same.

`scripts/bench-object-catalog-startup.mjs` runs the ignored benchmark in
`tests/all_objects.rs` with affinity to exactly two specified Linux CPUs. It
reports p50 and p95 separately for fresh in-memory `Server::open`, persistent
reopen, and the first `sys.all_objects` count query after each open. The timing
starts inside the test executable, after compilation. To compare revisions,
copy the identical benchmark test into an isolated checkout of merged PR #174,
run both trees on the same Linux host with the same `--cpus` and `--samples`,
and compare the reported harness hashes. A synchronized source snapshot without
`.git` can supply `--revision` explicitly.

On `linux.local`, the merged #174 tree (`40259cc`, byte-identical to the
successful PR head `68f5f22`) and candidate `810bd42` used the same benchmark
harness (SHA-256 `075f080522861b11f41ea3c736ecbdf8142a4c1a299eb2f4fad8c899067f225d`),
20 samples each, and CPU affinity `0,1`. Times are milliseconds:

| Phase | #174 p50 / p95 | Arrow candidate p50 / p95 |
| --- | ---: | ---: |
| Fresh in-memory open | 489.7 / 505.6 | 482.6 / 495.6 |
| Persistent reopen | 496.4 / 507.5 | 500.1 / 516.5 |
| First query after fresh open | 3.64 / 3.73 | 3.64 / 6.03 |
| First query after reopen | 4.12 / 5.29 | 4.11 / 4.81 |

The fresh-open median fell 1.45%, while the reopen median rose 0.74%; this
does not establish a material startup improvement. The #174 CI run
`36075844399` spent 37 minutes 53 seconds in its two-worker independent-client
step. The candidate's corresponding CI duration is needed before concluding
whether the test-suite regression changed.

A separate five-sample diagnostic run timed the candidate's entire built-in
object registration at 39–43 ms on a fresh database and about 1.7 ms on
reopen, against roughly 480–500 ms for complete `Server::open`. This locates
most startup time outside the scoped seed loader. That run temporarily logged
durations and was not used for the comparison table; the instrumentation was
removed afterward. The remote runner once reported a 0.06-second build after
source bytes changed but retained an older file modification time, so the
candidate comparison used a targeted `cargo clean -p msduck` before compiling.
Remote source synchronization needs an independent fingerprint-invalidation
fix. Further startup work should profile the surrounding catalog and server
registration phases once their active claims release those files.

The three views have independent captured metadata. In particular,
`sys.objects` returns nonnullable `Bit` flags, while the union and system view
declare nullable `BitN(1)` flags. The system view declares a nullable
`parent_object_id` and a nonnullable `type`; the other two use the opposite
nullability for those fields. Metadata is declared in `query_catalog.rs`,
separately from the DuckDB storage types.

Catalog membership does not implement the corresponding built-in procedures,
functions, or types. User-object synchronization currently covers tables and
views; procedures, functions, constraints, synonyms, triggers, and sequences
need their own transactional DDL adapters. `sys.all_columns` still lacks
built-in column rows, and SQL Server catalog locking and visibility rules have
not been implemented. The reference capture's fourteen-created-object case
and observer lock-timeout case therefore remain open compatibility targets.
