# Local C API registration bridges

Source: crates.io duckdb 1.10505.0 (duckdb-rs), copied from the Cargo registry.
Original crate archive SHA-256: `970e05eedd3f55c435194d9104f90a9b4a79a80d6e73251bc9ff43e178130c4e`.
The upstream license and source are retained.

`Connection::register_aggregate_function` and
`Connection::register_scalar_function_raw` were added to `src/lib.rs`.
Each borrows the existing connection and forwards a live C API function definition
to DuckDB, without exposing a raw connection handle or changing ownership.
The caller owns the definition; DuckDB copies registration metadata. Callback
lifetimes and safety remain the caller's responsibility. This can be replaced
by upstream registration APIs when available. The raw scalar bridge permits
binding callbacks for type validation before execution. The separate
libduckdb-sys patch is documented in its own MSDUCK-PATCH.md. Subsequent prepared
result introspection is described in ../../docs/error-metadata.md.

## Nanosecond Arrow time materialization

`src/arrow_interop/schema.rs` maps Arrow Time64(Nanosecond) to DuckDB TIME_NS.
`src/arrow_interop/to_duckdb.rs` copies its nanosecond values without converting
them to microseconds. The previous mapping produced TIME vectors, which failed
when appended to a TIME_NS destination and could discard sub-microsecond digits.
Other time units retain their existing conversion paths.

The root enables `appender-arrow` for lossless row-image materialization. This
feature uses the existing `vtab-arrow` dependency already enabled by `vscalar`.
`tests/arrow_images.rs` checks 6,001 rows through native appender chunking,
including NULLs, nested times, raw UTF16 carrier bytes, decimal values and
Boolean/UUID extension metadata. It asserts exact Arrow equality and distinct
TIME_NS versus TIME storage types after materialization.

`VScalar::special_null_handling()` is an opt-in binding extension. Both ordinary
and stateful registration apply DuckDB's scalar special-handling flag to every
overload when requested. The default remains false, preserving upstream NULL
propagation; callback containment, vector flattening and ownership are unchanged.
JSON extraction opts in because SQL Server rejects a NULL path even when its
source is NULL, whereas default DuckDB propagation can bypass the callback.
Regression: `tests/vscalar_nulls.rs` covers both registration APIs, multiple
signatures, default propagation and recoverable callback errors.
