# SQL Server `FOR XML PATH` wire reference

`reference/for-xml-wire.json` retains six queries in each of two fresh databases
and a second independent fresh container with two more databases, all on the pinned SQL Server 2025 image
`mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`.
The generator attached its packet observer only after tedious completed login.
It records decrypted, post-login TDS packets in both directions, their complete
headers and reassembled response payloads, byte offsets and raw bytes for
tokens, and tedious's decoded descriptors, rows, errors and DONE events. No
credential or login payload is retained. The second container matched the
stable type, value, error and DONE invariants. All four raw runs are in the
fixture; its SHA-256 is
`745798c2b271b522faf53448774c72135106bb3ffb40dd35742c3602bed8c484`.
A subsequent exact-script recapture in two more fresh containers also matched.

The central finding is that **untyped FOR XML uses legacy NTEXT TYPE_INFO
`0x63`** in this SQL Server build, exactly as tedious reports. It does not use
NVARCHAR(MAX) `0xE7` or PLP for this query. The copied TDS skill's general
NVARCHAR(MAX) statement is inconsistent with these observed bytes. `TYPE`
uses XML TYPE_INFO `0xF1` and PLP. Do not convert the untyped path to a
NVARCHAR descriptor merely because its XML text is Unicode.

| Query | COLMETADATA | ROW value framing | Final DONE |
| --- | --- | --- | --- |
| Text `PATH('row')` | offset 0–105, flags `0x0001`, `0x63`, magic XML column name | offset 105–189, NTEXT 54-byte UTF-16 value | status `0x0010`, command `0x00c1`, count 1 |
| Zero source rows | same metadata | no ROW token | status `0x0010`, count 0 |
| Direct `TYPE` | offset 0–12, flags `0x0003`, unnamed `0xF1` | offset 12–83, XML PLP chunk 54 bytes | status `0x0010`, count 1 |
| Nested `TYPE` | offset 0–26, flags `0x0023`, name `payload`, `0xF1` | offset 26–101, XML PLP chunk 58 bytes | status `0x0010`, count 1 |
| >8 KiB text | same NTEXT metadata | three ROW tokens with UTF-16 values of 4066, 4066 and 1920 bytes | status `0x0010`, count **1** |
| Invalid attribute order | no COLMETADATA/ROW | ERROR `6852`, state 1, class 16 at offset 0–277 | status `0x0002`, count 0 |

Offsets are zero-based in the reassembled tabular response payload, excluding
the eight-byte TDS packet header. The ordinary text TYPE_INFO bytes are
`63 fe ff ff 7f 09 04 d0 00 34 01 01 00 78 00`: NTEXT ID, declared maximum
`0x7ffffffe` bytes, five-byte collation `09 04 d0 00 34`, and one table-name
part `x`. Its ROW starts with a one-byte text-pointer length `0x10`, the 16-byte
pointer `64756d6d792074657874707472000000`, an eight-byte timestamp
`64756d6d79545300`, a little-endian four-byte value length, then UTF-16LE
data. These pointer/timestamp bytes are observed placeholders, not a contract
that future server builds must repeat verbatim.

Direct and nested TYPE both have TYPE_INFO `f1 00`: XML ID and no schema
collection. Their ROW starts with the eight-byte PLP unknown-length marker
`fe ff ff ff ff ff ff ff`, then a four-byte chunk length and UTF-16LE data,
then a zero-length chunk terminator. The XML text values are 54 and 58 bytes.
Neither XML descriptor carries collation. The nested projection name changes
the metadata name and flags, not the type ID or PLP framing.

The large query explicitly casts the seed to `NVARCHAR(MAX)` before
`REPLICATE`, avoiding SQL Server's bounded `NVARCHAR(4000)` result. The XML
value is 10,052 UTF-16LE bytes across three NTEXT ROW tokens. The response
occupies three TDS packets of 4096, 4096 and 2092 bytes, while DONE_COUNT is
one source row. A client may concatenate the ROW values to reconstruct the
complete XML; the server adapter must preserve both the wire row chunks and
the statement's logical completion count. Packet boundaries and ROW chunk
boundaries serve different purposes and are not assumed to coincide generally.

The ERROR token's server-name field changed with the fresh container name,
so the two independent raw error payloads differ even though number, state,
class, message, line, empty procedure and DONE status matched. Packet SPIDs
also vary. The generator compares only semantic values, logical descriptors,
error identity and DONE tokens; it retains both raw runs so packet splits or
future server differences remain visible. It does not assert a universal
chunk size, text pointer, packet size or SQL Server version behavior beyond
this pinned image.

Run `node scripts/capture-for-xml-wire.mjs` from a checkout with Docker and
Node.js 24+ to write an ignored capture. `--write-fixture` creates the retained
fixture only when absent; it refuses to replace one. On any failed capture,
the script writes available raw packets to
`artifacts/compatibility/for-xml-wire/capture-failed.json`. A second invocation
starts a new container and compares stable invariants with the fixture.

This is reference evidence, not runtime `FOR XML` support. The deterministic
serializer in `crates/msduck-sql/src/for_xml_path.rs` has no shared SQL export
or root execution binding yet. The later adapter must provide syntax/alias
binding, evaluated ordered rows, XML serialization, exact NTEXT/XML metadata,
wire chunking, errors and DONE counts before claiming client compatibility.
