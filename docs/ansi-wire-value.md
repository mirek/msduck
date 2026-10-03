# Native ANSI result-value framing

`crates/msduck-tds/src/ansi_value.rs` is an unregistered deterministic writer,
tested through a private path import. It appends the value portion of a ROW or
RETURNVALUE, without emitting a token, TYPE_INFO, descriptor or collation.
Registration and root output integration remain separate work; this does not
enable additional operational collations or establish runtime compatibility.

`Declaration::new` admits BIGCHAR (`0xAF`) and BIGVARCHAR (`0xA7`) with explicit
1–8000 **byte** capacities. Only VARCHAR admits MAX (`None`). Legacy TEXT,
Unicode, binary and other families are rejected. NULL is distinct from empty
bytes. Fixed CHAR requires exactly the declared byte width for non-NULL values;
the caller supplies any SQL-required padding. The writer never pads, truncates,
decodes, repairs malformed UTF8, chooses an encoding or performs best-fit
conversion. Bytes must already be converted into the target's native domain.

Bounded values emit a two-byte little-endian length and the original bytes;
NULL emits `FFFF`. MAX emits an eight-byte known total length, nonempty chunks
with four-byte lengths and one zero chunk terminator. MAX NULL emits eight
`FF` bytes, with no terminator; a non-NULL empty value emits twelve zero bytes.
`Value::Bytes` emits one data chunk when nonempty. `Value::Chunks` preserves
explicit caller-supplied PLP boundaries, including a boundary inside a UTF8
character. An empty chunk list represents empty; a zero-length data chunk is
rejected because it would prematurely terminate the value. Explicit chunks
are not admitted for bounded declarations. Unknown-total PLP and streaming
partial writes are outside this API.

Before modifying output, the writer validates the declaration/value contract,
checks all chunk and total lengths, computes complete framing growth with
checked arithmetic, includes existing output in the caller's resource limit,
and applies the server's 16 MiB message ceiling. It reserves the entire growth
before the first append. Validation, resource or allocation errors preserve
the caller's existing bytes. This bound includes prefixes and terminators,
rather than allowing a full-limit payload to overflow while framing it.

The [owner SQL Server reference](bulk-character-encoding-reference.md) retains
33 probes in four independent databases on SQL Server 17.0.4065.4. Its immutable
`reference/bulk-character-encoding.json` has SHA256
`0d2cff08bc7f59d271955311ac540f95a37f31b456bc008f739117c1a09d3a83`.
The test decodes **incoming readback responses**, retains their value frames
and chunk boundaries, and checks all 312 captured VARCHAR values (78 per run).
Every emitted value frame is compared against this writer's exact bytes.
NBCROW bitmap NULLs emit no value frame and are checked separately; a caller
must omit their value bytes, rather than invoke this ROW/RETURNVALUE writer.
The tests also compare native binary
projections, and requires the captured UTF8 duck character to cross a PLP
chunk boundary. Outgoing BulkLoad source frames are not result-value evidence.

Captured accepted `c328` and `eda080` native UTF8 bytes remain unchanged, even
when client text decoding and SQL Unicode conversion differ. Synthetic tests
also preserve all 256 possible byte values and other malformed sequences; that
proves byte framing, not SQL acceptance of those inputs. Fixed CHAR framing is
grounded by the TDS USHORTLEN convention and exact-width controls, rather than
claiming this VARCHAR reference captures CHAR conversion/padding behavior.
Unsupported encodings, conversion rules, legacy TEXT and root storage/output
adoption remain explicit gaps.
