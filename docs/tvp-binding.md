# Catalog-backed TVP binding

`msduck_tds::tvp` now publicly exports the bounded wire decoder.
`msduck::tvp_binding::bind` resolves its decoded input against a caller-supplied
procedure table-type declaration using the connection's current catalog.
Qualified names are bound query values. The expected type and ordered columns
are fetched with a bounded query; supplied identities must resolve to the same
user type and backing object. Catalog case lookup currently uses `lower`, as
the table-type foundation does; complete identifier-collation equivalence is
not established.

The explicit context includes the declared schema/name, session default schema,
parameter name/ordinal and default-status compatibility profile. Empty client
schema/name can use the procedure declaration, as captured in
`reference/tvp-binding.json`; a schema without a type name rejects with 8049.
A wrong existing identity rejects with 206, a missing identity with 2715, and
a wrong column count with 500. Captured diagnostics retain number, state,
severity and message, and line numbers where retained. Legacy
`tvp-wire` callback observations omit line numbers. Supplied identities are
resolved without inspecting unrelated column declarations. No catalog writes or row materialization
occur in binding. The eventual executor must hold its catalog/execution
transaction across binding and use; this API does not lease type identities
against concurrent DROP/recreate.

Supported destination columns are INT, bounded NVARCHAR and VARBINARY.
Declared names, nullability, widths and character collation come from the
catalog. Incoming metadata is not compared for exact width equality: captured
narrower/wider NVARCHAR and BIGINT metadata can bind to NVARCHAR(10) and INT.
BIGINT-to-INT overflow retains 8115/state 2 followed by 8061/state 1, including
their distinct line numbers. Cells are owned INT values, raw UTF-16 code-unit
vectors or binary vectors. NULL, empty Unicode and empty binary remain distinct;
isolated surrogates are copied unchanged rather than converted to Rust String.

Omitted input and default-bit NULL input yield empty rows with different supply
origins. An explicit nondefault NULL yields captured 8060. The default-bit form
requires explicit `CapturedSqlServer2025` selection: the retained build accepts
it, while MS-TDS requires that bit to be zero for TVPs. `Specification` rejects
it. Non-NULL/default-bit combinations are uncaptured and unsupported. This is a
version-specific observation, not a change to the protocol specification.

The adapter rechecks catalog column, row, total-cell, per-cell and owned-byte
limits even for manually constructed decoded values. Catalog rows are bounded
before collection; input row widths, integer byte widths, Unicode parity and
wire cell capacities are validated. Unsupported column defaults, alias types,
nonzero user types, unusual flags, uncaptured integer widths, nonzero incoming
Unicode collation bytes, PLP/MAX columns and other conversions reject explicitly.
Uncaptured character/binary assignment overflow and NULL-to-NOT-NULL diagnostics
also remain unsupported. Values are checked before any caller can write them;
no truncation or guessed SQL error identity turns an unsupported case into success.

Root unit tests replay all 80 outgoing RPC requests across every retained run
in `reference/tvp-wire.json`, `reference/tvp-default.json` and
`reference/tvp-binding.json`. They compare owned cells and captured error
sequences, check metadata against the declared catalog, exercise byte/row limits,
malformed decoded shapes, raw surrogate units, empty binary, explicit default
profile selection, bound hostile names, catalog rollback and file reopen.
Reference response observations are not raw response-token bytes; these tests
prove the adapter's binding contract, not procedure wire interoperability.

Remaining execution gates: the RPC dispatcher must preserve parameter status
and decode TVPs among other parameters; the procedure executor must obtain the
READONLY declared type/default schema, bind in its execution transaction,
materialize owned cells with their declarations and lifetime, and emit the
captured diagnostic/completion sequences. CREATE TYPE execution, constraints,
permissions, indexes, defaulted columns, other scalar families, PLP values,
prepared RPCs, streaming and full identifier-collation resolution remain open.
The existing server does not yet execute TVP procedure calls.
