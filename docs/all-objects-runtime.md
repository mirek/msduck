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
remain on other machines. The loader now builds one typed Arrow record batch
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
