# Native integer SUM and AVG

Known integer SUM and AVG inputs now use native DuckDB aggregate callbacks.
TINYINT, SMALLINT and INT promote to an INT accumulator and result; BIGINT uses
BIGINT. Each non-NULL input contributes once. SUM and AVG check each state
update and state combination for overflow. AVG divides the bounded sum by its
non-NULL count using exact integer division that truncates toward zero.
Empty and all-NULL groups return typed NULLs.

Overflow is sticky within each aggregate state and reports error 8115 when
that state is finalized. Deferring the diagnostic until finalization is necessary
for window execution: DuckDB may build segment-tree states that are not used by
any requested frame. An unused, overflowing segment must not reject a valid
single-row frame. Selected states still preserve intermediate overflow even if
later inputs would cancel it.

The AST keeps DISTINCT and OVER clauses and supplies one evaluated argument.
Shared inference covers declared parameters, known expressions, catalog table
and view columns, integer VALUES columns with explicit aliases, and supported
CTE/derived output projections. DuckDB performs
grouping, DISTINCT elimination, window frame selection and parallel execution;
the Rust callbacks own typed accumulation, NULL counting and final values.
Callbacks contain Rust panics at the C boundary. Their state is plain data with
no external allocations or destructor requirements. Input vectors are flattened
by DuckDB's C API, and finalization honors its output offset.

The pinned duckdb-rs crate is vendored with one aggregate-registration method;
[the patch record](../vendor/duckdb/MSDUCK-PATCH.md) records provenance and its
safety contract. The bundled DuckDB aggregate C API has a
[state-vector flattening patch](../vendor/libduckdb-sys/MSDUCK-PATCH.md) for
constant window states. The source archive remains unchanged; the bundled cc
build applies the checked patch before compilation. Registration belongs
to the database owner and is repeated on reopen, preserving stored aggregate
views. Legacy final-SUM scalar helpers remain registered for older stored views;
such views must be recreated to use the new bounded aggregate definitions.

Native tests cover many groups across chunks, parallel execution, narrow window
frames, intermediate overflow, signed bounds and count overflow. A sequence
counter verifies one argument evaluation per row for SUM and AVG. Tedious tests
cover metadata, DISTINCT, windows, groups, NULL/empty inputs, CTEs, derived
sources, views, exact BIGINT values beyond floating-point precision, negative
truncation, AVG sum overflow, TRY/CATCH and prepared-query recovery. A restart
test checks persisted SUM/AVG views. Upstream mssqlite aggregate tests supplied
the checked sum and integer-average vectors; a local capture probe retains
values, metadata and overflow diagnostics for reference comparison.

Other numeric aggregate families, complete source inference, arithmetic session
options and full diagnostic fidelity remain unfinished. The exact order in
which parallel plans combine states can differ from SQL Server. No live SQL
Server comparison has yet verified this implementation.

References: Microsoft [SUM](https://learn.microsoft.com/en-us/sql/t-sql/functions/sum-transact-sql),
[AVG](https://learn.microsoft.com/en-us/sql/t-sql/functions/avg-transact-sql), and
[DuckDB aggregate C API](https://duckdb.org/docs/stable/clients/c/api).

A native regression checks 6,000-row whole-partition SUM/AVG windows, partitioned
BIGINT windows and a window aggregate over grouped native SUM results. These
exercise constant state vectors that previously caused a process crash.
