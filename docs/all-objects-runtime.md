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
remain on other machines. The measured CI latency warrants a separate startup
optimization, rather than treating longer deadlines as a performance fix.

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
