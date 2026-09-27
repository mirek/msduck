# TVP RPC wire decoder

`crates/msduck-tds/src/tvp.rs` decodes one TDS 7.3+ table-valued RPC
parameter starting at `TVPTYPE` (`0xF3`). `decode_prefix` reports consumed
bytes so a future RPC adapter can continue with subsequent parameters;
`decode_exact` rejects trailing bytes. The decoder has no I/O or catalog
access. It retains the declared schema/type name, user type, flags, column
types and collations, and borrows cell bytes without converting them to SQL
values. `Null`, `Default`, and zero-length non-NULL bytes remain distinct.

The tests replay all 16 raw requests in the retained owner-run
[`reference/tvp-wire.json`](../reference/tvp-wire.json): null, empty, a
nullable row and a three-row TVP, each across four fresh databases. The null
wire form is decoded, but the captured SQL Server rejected its non-default RPC
use with error 8060; successful decoding does not imply execution acceptance.

Parsing is limited by the caller's input, column, row, total-cell and cell-byte
budgets. The defaults cap input at 4 MiB, columns at 1024, rows at 10,000,
cells at 100,000 and each cell at 65,534 bytes. Truncation, malformed lengths,
unexpected tokens and trailing bytes are errors. The implemented column codecs
cover bounded `INTN`, `NVARCHAR` and `VARBINARY`; `NVARCHAR(max)` and
`VARBINARY(max)`/PLP, other SQL types, and optional TVP ordering/uniqueness
metadata return explicit unsupported errors. Database and column names must be
empty as required for TVPs. Defaulted columns consume no bytes in each row.

This is a wire decoder, not a live server feature. The TDS crate's `lib.rs`
export is reserved by another claim, so tests import the module by path. Root
RPC binding, table type lookup, SQL value conversion, row streaming,
transaction behavior and client interoperability remain separate integration
work. Input TVPs and optional metadata are specified in Microsoft's
[TVP metadata](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tds/0dfc5367-a388-4c92-9ba4-4d28e775acbc)
and [optional metadata and row tokens](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tds/fcacd8f2-0bf0-4118-809f-8d460c4e1508).
