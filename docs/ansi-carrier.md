# Native ANSI DuckDB storage

Task #904 implements the native storage prototype from the
[character adapter plan](bulk-character-adapter-plan.md). The public
`msduck::ansi_carrier::Plan` validates a named CP1251, CP1252 or UTF8 identity
and explicit per-cell and per-batch payload byte limits. Opaque identifiers do
not alias those profiles. This plan preserves bytes; it does not certify SQL
acceptance, decode malformed UTF8 or apply character capacity/padding rules.

The physical representation is
`STRUCT(__msduck_ansi_encoding UINTEGER, __msduck_ansi_bytes BLOB)`.
An explicit logical plan is required to pack, read or unpack it. A BLOB,
ordinary SQL STRUCT or existing UTF16 carrier does not automatically become
VARCHAR. The encoding field must match the plan. NULL parents remain NULL;
empty non-NULL payloads remain empty; a non-NULL parent with a NULL child fails.
Arrow readers validate fields, types, child lengths and row bounds even before
NULL shortcuts. Both Binary and LargeBinary Arrow payloads are supported.

`Plan::bind` supplies a checked binary operand to the matching fixed-name pack
function. `Plan::register` registers pack and unpack functions on the supplied
DuckDB connection with immutable plan state. The pack expression consumes its
operand once; it does not duplicate a volatile expression. Each native callback
preflights the complete chunk's cell lengths, tags and aggregate byte budget
before copying any output payload. Native access validates type, row bounds and
validity before reading string_t storage. Arrow batch reads likewise preflight
before allocating/copying, and owned Rust copies use fallible reservations.
DuckDB owns its native allocations and reports native query failures normally.
The budgets measure payload bytes, not total heap usage, nor a cumulative
statement size across multiple native chunks.

Tests use the public API, actual DuckDB storage, reopened databases, Arrow and
native vector bridges. They cover all256 CP1251/CP1252 bytes, preserved invalid
UTF8, NULL/empty, malformed tags and children, slices, limits, multi-chunk volatile
operands and statement/explicit-transaction rollback on native errors.
The three unchanged reference fixtures from #884/#895/#899 are pinned by SHA256.
Across all four runs of the selected observations, 252 native target cells
(28 CP1251, 44 CP1252, 180 UTF8) are stored, reopened and compared as bytes.
These include 40 accepted malformed UTF8 cells and 16 values over8000 bytes.
SQL UTF16 projections and client display are deliberately separate facts;
failed reference loads do not supply successful cells or become admission rules.

This prototype does not change common Value/Parameter, backend_value, catalog,
BulkLoad lowering or final wire encoding. Registration must be performed by a
future explicitly planned consumer; the server does not enable these functions
automatically. Operational CP1251/UTF8 collation gates remain closed. Root
adoption must carry declaration/catalog facts into the plan and preserve logical
metadata through later conversions and result encoding before support is enabled.
