# Exact numeric RPC values

Decimal (0x6A), numeric (0x6C) and money (0x6E) RPC inputs are decoded without
floating-point intermediates. Decimal/numeric validate declared precision
1–38, scale 0–precision, storage/value lengths, sign and magnitude. NULL retains
type information. Shorter valid value storage is accepted when the declared
precision permits a larger maximum. Values become DuckDB Decimal with an exact
i128 scaled payload.

Decimal result metadata and rows now use 5/9/13/17-byte storage according to
precision instead of advertising 17 bytes for every result. The encoder uses
the stored scaled integer, validates precision/scale and does not stringify or
round-trip through a float.

MONEY uses signed high int32 followed by unsigned low uint32, unlike ordinary
little-endian int64. SMALLMONEY uses signed int32. Both have scale four, and
unit vectors cover their signed endpoints. RPC money declarations currently
lower to decimal(19,4) or decimal(10,4) for exact binding. Original money wire
metadata, money-specific arithmetic and general SQL MONEY type behavior still
need declared-type catalog work; this is not a complete MONEY implementation.

Verification includes tiberius round-trips of full 38-digit positive/negative
values, tedious decimal/numeric/money parameters, typed NULL result metadata,
persistent decimal table writes and prepared execution. Tests also cover each
precision/storage boundary, negative zero, shorter numeric values, malformed
sign/scale/precision/length, overflow and truncated values. JavaScript numbers
cannot prove full decimal precision, so the 38-digit checks use Rust i128 and
tiberius Numeric values.

References:

- [MS-TDS Decimals and Numerics](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tds/5e02042c-a741-4b5a-b91d-af5e236c5252).
- Copied `.agents/skills/tds-protocol/data-types.md` and upstream mssqlite
  `packages/tds/src/decimal.ts`, pinned in docs/reference-review.md.

Full SQL Server decimal expression precision/scale propagation, conversion and
rounding behavior remain to be differentially validated. Wire/storage accuracy
does not prove compatibility of every arithmetic expression.
