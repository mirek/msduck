# msduck bundled DuckDB patches

Upstream libduckdb-sys 1.10505.0 from crates.io. License retained.
The original duckdb.tar.gz is unchanged (SHA-256 `e11f1209cdbb2a99b2ea2d348de21bdfb01dc494b55d1271df94b09c52bf229c`).

Only build_bundled_cc.rs changes upstream code. After extracting the pinned
archive, it flattens the state vector before C API aggregate update and flattens
the target vector before combine. Each exact replacement is asserted once to
fail on source drift. This restores the per-row state-pointer contract for
constant window vectors; without it, OVER () can read invalid state pointers
and crash the process. This patch applies to the bundled cc backend used by
msduck; linked/system and bundled-cmake builds are not covered.

Regression: integer_aggregate native whole-partition windows and tedious
aggregate argument validation grouped-window query.

Private, noncycling sequences whose names start `__msduck_identity_` receive a
second patch in `sequence_catalog_entry.cpp`. Reload reconstructs `last_value`
from the saved next counter minus increment; WAL replay updates it the same way.
Failed NextValue calls restore the original counter. These fixes make identity
current-value retrieval survive checkpoint/reopen, WAL recovery and exhaustion.
Each exact replacement is asserted once. Other DuckDB sequences retain upstream
behavior. The next counter uses unsigned modular arithmetic when a valid allocation's
successor exceeds BIGINT. With usage_count > 0, its wrapped range is disjoint
from reachable non-overflowing successors: below INT64_MIN + increment for
positive increments, or above INT64_MAX + increment for negative increments.
That range identifies exhaustion before another allocation. Last-value recovery
uses the inverse modular subtraction. This permits terminal BIGINT allocations
and retains exhaustion through checkpoint and WAL, without changing public or
cycling DuckDB sequences.

Regression: identity reopen/current-value assertions, concurrent connection
lookups, WAL recovery subprocess test and tedious IDENT_CURRENT exhaustion tests.


During initial WAL replay, `catalog.cpp` now resolves an implicit catalog against
the catalog retriever's explicit default when the database manager has no default
database yet. Default-expression binders already anchor their search path to the
owning table. Normal initialized sessions retain upstream lookup behavior. Without
this fallback, ALTER-installed nextval defaults fail during replay before the
initial database attachment is complete. One exact replacement is asserted.
Regression: the identity WAL subprocess test adds IDENTITY to populated rows and
verifies persisted values and subsequent allocation after an unclean exit.


Cross-product filter pushdown stops at volatile predicates. Upstream can push a
predicate containing nextval and a scalar catalog subquery into the one-row
subquery input, evaluating it once rather than for each source row. The guard
uses IsVolatile and FinishPushdown; deterministic predicates retain upstream
behavior. One exact insertion is asserted in `pushdown_cross_product.cpp`.
Regression: the object-catalog native test filters 6,000 rows with alternating
volatile name inputs, checking both 3,000 matches and 6,000 allocations.

Join-order relation extraction treats volatile filters as a relation boundary,
optimizing their child independently and retaining the filter above it. This
prevents join reconstruction from moving the predicate again after filter
pushdown. One exact insertion is asserted in `relation_manager.cpp`. The same
native regression runs with normal optimizer settings.
