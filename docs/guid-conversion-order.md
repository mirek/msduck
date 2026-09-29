# SQL Server `uniqueidentifier` ordering and text conversion

`reference/guid-conversion-order.json` retains 25 requests from each of two
independent fresh databases on the pinned SQL Server 2025 image. The capture
contains SQL text, rows, TDS column descriptors, errors, informational tokens,
completion tokens and post-error recovery queries. Run
`node scripts/capture-guid-conversion-order.mjs --check` to verify the retained
fixture without starting a container. The fixture SHA-256 is
`ffebfcb92b7ee86e221974a6a488d36899f09b5e847bf4a7b15101c332e7bbb8`.

## Observed ordering

The fixed values vary one GUID segment at a time and, within the first three
segments, vary low and high textual byte positions. SQL Server's `ORDER BY g`
returned these labels in order:

`zero, first_high, first_low, second_high, second_low, third_high, third_low, fourth_low, fourth_high, last, last_high, max`.

`ORDER BY CONVERT(varchar(36),g)` returned:

`zero, last, last_high, fourth_low, fourth_high, third_low, third_high, second_low, second_high, first_low, first_high, max`.

`ORDER BY CONVERT(binary(16),g)` returned:

`zero, last, last_high, fourth_low, fourth_high, third_high, third_low, second_high, second_low, first_high, first_low, max`.

The three orders differ. For example, `01000000-0000-0000-0000-000000000000`
precedes `00000001-0000-0000-0000-000000000000` as a GUID, but follows it
as text. SQL Server's binary conversion exposes the mixed-endian representation:
the former is `0x00000001000000000000000000000000`, the latter
`0x01000000000000000000000000000000`. A direct `<` comparison also reports
`first_high < last`, with the reverse false. These observations constrain a
future comparator; they do not establish every possible GUID pair or every
comparison context.

The deterministic core now exposes
`msduck_core::types::uniqueidentifier::{order_key,compare}`. It accepts the
mixed-endian bytes returned by `CONVERT(binary(16), guid)` (the same byte layout
used by TDS), then compares these byte groups in precedence order:
`[10..16], [8..10], [6..8], [4..6], [0..4]`. Bytes stay in their original
order within each group. A test builds keys from every captured binary value
and reproduces the ascending, descending and direct comparison results. This
is a rule derived from the retained sample, not a claim that all GUID
comparison contexts have been differentially verified. Root execution still
uses DuckDB's UUID semantics until an adapter applies the core key.

The stored GUID column has TDS `UniqueIdentifier` width 16 and non-null flags
8 even for an empty filtered result. The conversion expressions below have
`UniqueIdentifier` width 16 and nullable flags 33. The fixture retains the
complete descriptors, including completion sequence and collation fields.

## Observed character conversion

Both `varchar` and `nvarchar` canonical text convert to the same GUID, and
uppercase input is accepted. An overlong value containing a valid 36-character
prefix followed by `EXTRA` converts to that prefix for both `CAST` and
`TRY_CONVERT`; trailing spaces also convert. A brace-wrapped GUID is accepted
by `TRY_CONVERT`. Hyphenless and short values return `NULL` with
`TRY_CONVERT`. Typed character `NULL` likewise returns `NULL`.

Malformed `varchar` and `nvarchar` `CAST` each emit error 8169, state 2,
class 16: “Conversion failed when converting from a character string to
uniqueidentifier.” Each failed result contains a GUID descriptor and no rows;
a later `SELECT 1` succeeds on the same connection. The fixture retains
the descriptor, completion tokens and diagnostic details. This capture format
does not record their interleaving order.

msduck currently transports GUID values and maps `NEWID()` to DuckDB's UUID
generator; see [GUID RPC and result types](guid-rpc.md). This reference does
not claim that msduck matches the ordering or character conversion cases.
Runtime comparison, conversion and broader differential tests remain separate
implementation work.
