# COMPRESS and DECOMPRESS reference contract

The retained fixture in reference/compress-decompress.json contains 143 SQL
Server observations. Each was captured in two fresh databases in each of two
independent containers, using the pinned SQL Server 2025 image digest
86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a (product major
version 17, server and database collation SQL_Latin1_General_CP1_CI_AS). The
four raw captures matched exactly. The observations are:

- 125 ordinary batches: 2 setup statements, 93 named cases and 30 DECOMPRESS
  cases over hand-built GZIP members;
- 14 sp_executesql RPC calls;
- 4 prepared handles, with 17 sp_execute calls in total. Each handle's
  sp_prepare, every sp_execute and the sp_unprepare are retained separately.

The fixture keeps result rows, TDS column descriptors, error
number/state/class/line/text, information messages (none occurred), DONE tokens
and return statuses. Binary values are stored as hex.

**Bounded large values.** A binary value over 256 bytes, or a string over 256
UTF-16 code units, is retained as
`{ kind: 'digest', type, length, sha256 | sha256Utf16le, head }` rather than in
full. This applies to result cells and also to recorded RPC and prepared
parameter values. `head` holds the first 16 bytes (as hex) or the first 16
characters. The large-input cases also return server-side summaries: input
DATALENGTH, compressed DATALENGTH, `HASHBYTES('SHA2_256', COMPRESS(x))`, the
10-byte header, a round-trip equality flag and the decompressed DATALENGTH.
Small examples are kept exactly, including a 133-byte COMPRESS of 100,000 `'a'`
characters. The fixture is 426 KB.

scripts/capture-compress-decompress.mjs regenerates an artifact under
`artifacts/compress-decompress-reference-v1/` and compares it with the retained
fixture using the bounded `assertSameCapture` helper. It has three flags:

- `--write-fixture` refuses before starting any container when the fixture
  already exists (an existence check only), and writes with an exclusive-create
  flag.
- `--one-database` is a diagnostic mode that cannot write the fixture.
- `--check-fixture` validates the four retained runs, and checks that they are
  equal, without Docker.

The script's prepared-statement capture copies the semantics of the
owner-authored `capturePrepared` helper on `work/prepared-capture-helper-v1`
(PR #312), because that helper is not on main yet. It uses one reusable
Request, waits for the `prepared`/`error` events, clears `request.error` before
each phase and records only errors raised during that phase. Each RPC and
prepared statement has unique text (a trailing `/*...*/` comment), so it never
shares a plan cache entry with another program. The setup table names its
primary key constraint explicitly.

msduck does not implement COMPRESS or DECOMPRESS. This document describes SQL
Server only.

## COMPRESS

| Input | Captured behavior |
| --- | --- |
| Result descriptor | VarBinary, TDS length 65535 (MAX), flags 33 (nullable), in every successful batch, RPC and prepared case. sp_describe_first_result_set reports `varbinary(max)`, max_length -1, nullable, tds_type_id 165. This holds for VARCHAR, NVARCHAR and VARBINARY(MAX) input, and for a typed NULL input. SELECT INTO creates a nullable `varbinary` with max_length -1. |
| Output format | One RFC 1952 GZIP member: the 10-byte header, a raw DEFLATE stream, then CRC-32 and ISIZE (little-endian, input length mod 2^32). Each captured non-empty member can be decompressed by Node's zlib `gunzipSync`, and has the correct trailer. |
| Header | Always exactly `1f 8b 08 00 00 00 00 00 04 00`: ID1/ID2, CM=8 (deflate), FLG=0 (no FTEXT, FHCRC, FEXTRA, FNAME or FCOMMENT), MTIME=0, **XFL=4** (the "fastest algorithm" marker) and **OS=0** (FAT). No timestamp is embedded. The header is the same for 1-byte, 8 MB and incompressible inputs. |
| Determinism | The same input gives the same bytes in every database, container and protocol, and `COMPRESS('abc')=COMPRESS('abc')`. However, COLUMNPROPERTY IsDeterministic is 0 (IsPrecise 1) for computed columns `COMPRESS(v)` and `DECOMPRESS(COMPRESS(v))`, and a PERSISTED `COMPRESS(v)` column raises 4936, state 1, class 16 ("Computed column 'c' in table 'compress_computed' cannot be persisted because the column is non-deterministic."). |
| Byte source | Compresses the raw storage bytes of the value. VARCHAR `'abc'`, VARBINARY 0x616263 and their MAX forms give identical output (`1f8b08000000000004004b4c4a0600c241243503000000`, 23 bytes). NVARCHAR N'abc' compresses UTF-16LE `610062006300`, giving `1f8b08000000000004004b64486248660000b07a95ad06000000` (26 bytes), which differs from the VARCHAR output. CHAR(5), NCHAR(5) and BINARY(5) include their padding. Trailing spaces are kept. VARCHAR `'café'` under the default collation compresses CP1252 byte 0xE9. Under Latin1_General_100_CI_AS_SC_UTF8 it compresses UTF-8 `c3a9`. NCHAR surrogate pairs are compressed as UTF-16LE. |
| Examples | `'hello'` gives `1f8b0800000000000400cb48cdc9c9070086a6103605000000`. 0x00 gives `1f8b08000000000004006300008def02d201000000`. REPLICATE('a',100) gives 24 bytes. DATALENGTH is 21 for one byte and 23 for three bytes. The fixture contains more exact examples. |
| Empty input | `''`, `N''`, `0x`, empty VARCHAR(MAX), empty RPC and empty prepared inputs all return **an empty VARBINARY (0x, DATALENGTH 0), not a GZIP member**. The result is not NULL. |
| NULL | A typed NULL (VARCHAR, NVARCHAR(MAX), VARBINARY), an untyped `NULL` literal, an RPC NULL and a prepared NULL all return NULL with the VarBinary(MAX) descriptor. The untyped literal raises no error. |
| Rejected types | Error 8116, state 1, class 16, "Argument data type T is invalid for argument 1 of Compress function.", before any metadata. The batch then ends with DONE (rowCount null). T is: int, numeric (for 1.5), float, bit, datetime, date, uniqueidentifier, xml, text, ntext, image, sql_variant, timestamp (ROWVERSION) or json. There is no implicit conversion. An INT sp_executesql parameter also gives 8116, followed by a lone DONEPROC with return status 8116. |
| Argument count | Zero or two arguments: error 174, state 1, class 15 ("The Compress function requires 1 argument(s)."). |
| Nesting | `COMPRESS(COMPRESS('abc'))` is a valid 39-byte member. The inner result round-trips through two DECOMPRESS calls. |
| Incompressible data | The deflate stream falls back to stored blocks. 256 bytes of SHA-256 noise give 279 bytes (header 10, one stored block header 5, data 256, trailer 8). 20,000 bytes give 20,028 bytes. 65,536 bytes give 65,574 bytes, which is 20 bytes of stored-block headers (four blocks). The first stored block of the 64 KB input has LEN 0x4007. The server-built and client-sent 64 KB inputs are the same bytes (SHA-256 `b9309a4e…`) and give the same compressed digest (`896f3ce3…`). |
| Large repetitive data | 100,000 `'a'` compress to 133 bytes. 900,000 bytes of repeated English text compress to 2,697 bytes, and the same text as NVARCHAR (1.8 MB) to 6,205. 1 MB of `'0123456789abcdef'` compresses to 2,085 bytes and 8 MB of zero bytes to 8,175. Every summary round trip was exact. |
| Bounded length | `COMPRESS(REPLICATE('x',8000))` returns 40 bytes, and 4,000 N'xy' pairs return 45. |

### DEFLATE encoder

The deflate body is not produced by stock zlib with any single configuration.
For short inputs (`'abc'`, N'abc', `'hello'`, CHAR(5) padding, 0x00 and
REPLICATE('a',100)), Node's zlib `deflateRawSync` produces identical bodies at
every level from 1 to 9. The 8000-byte, 4,000-pair and 100,000-byte repetitive
bodies match zlib only at levels 4 to 9, although the header claims XFL=4
("fastest"). The 900-byte repeated sentence (`compress repetitive text`) matched
no zlib level, windowBits from 9 to 15, memLevel or strategy. A byte-exact
COMPRESS therefore needs a separately specified encoder. The fixture is
evidence for that encoder, but it is not a specification of it.

## DECOMPRESS

| Input | Captured behavior |
| --- | --- |
| Result descriptor | VarBinary, TDS length 65535 (MAX), flags 33, for every successful and NULL case, and `varbinary(max)` in describe. No error preserves the input's type: DECOMPRESS of a compressed NVARCHAR returns the UTF-16LE bytes (`610062006300`). |
| Accepted input types | Only binary types. VARBINARY, VARBINARY(MAX) and BINARY(n) are accepted. A BINARY(30) holding a 25-byte member plus 5 zero-padding bytes decompresses to 'hello'. VARCHAR, NVARCHAR, VARCHAR(MAX) (even when it holds GZIP bytes) and INT raise 8116, state 1 ("Argument data type varchar(max) is invalid for argument 1 of Decompress function."). With zero arguments it raises 174, state 1, class 15. |
| NULL and empty | An untyped `NULL`, a typed NULL and an RPC or prepared NULL all return NULL. `DECOMPRESS(0x)` returns **0x** (empty, not NULL), so `DECOMPRESS(COMPRESS(''))` is 0x. |
| Header fields | These are parsed and skipped: FTEXT, FNAME, FCOMMENT, FEXTRA and FHCRC. A **wrong FHCRC is not checked**. The reserved flag bit 0x20 is ignored. Nonzero MTIME and OS 0 or 3 are ignored. Each of these returns 'hello'. |
| Rejected members | Error 9826, state 1, class 16 ("Uncompressed or corrupted data passed as argument to DECOMPRESS builtin."). It is raised for CM=7, a wrong second magic byte, a wrong CRC-32, a wrong ISIZE, a zlib (RFC 1950) wrapper, raw deflate without a header, a lone 0x00 byte, and an empty member whose ISIZE is 1. The column metadata is sent first, then the error with no rows, and then DONE (rowCount null). Over RPC and prepared execution the sequence is DONEINPROC (rowCount null), then DONEPROC. sp_executesql returned status 9826 and sp_execute returned -6 (see the protocol section). |
| Truncation returns NULL | Input that ends before the deflate stream completes returns **NULL with no error**. This covers `0x1f`, `0x1f8b`, `0x1f8b08`, a truncated header (7 bytes), a complete header with no deflate data, and a header with partial deflate data. The same applies to an empty member (`0x1f8b0800000000000003` + `0300` + zero CRC/ISIZE): although it is valid, it returns NULL, not 0x. |
| Trailer | When the deflate stream completes but the 8-byte trailer is missing or only half present (4 bytes), the data is returned and the check is skipped. When a full trailer is present, both CRC-32 and ISIZE are verified. |
| Trailing data | Bytes after the first member's trailer are ignored. Garbage returns 'hello'. For two concatenated members, only the first is returned ('hello', not 'helloworld'). |
| Stored blocks | A stored (BTYPE 00) block decompresses normally. |
| Round trips | `CAST(DECOMPRESS(COMPRESS(x)) AS <original type>)` restores VARCHAR, NVARCHAR (including a surrogate pair), VARCHAR under a UTF-8 collation (bytes `636166c3a9`), CHAR padding (`'abc  '`) and a 4-byte INT (258). The same holds when x is an RPC value (`café 😀`) or a stored table column. A cast to the wrong family reinterprets the bytes. Compressed N'abc' cast to VARCHAR(MAX) gives `a\0b\0c\0` (6 bytes). Compressed `'abcd'` cast to NVARCHAR(MAX) gives U+6261 U+6463. An odd byte count cast to NVARCHAR keeps the last byte: `'abc'` gives U+6261 followed by U+0063, so the lone final byte becomes a zero-extended code unit (DATALENGTH 4). Casting to VARCHAR(3) silently truncates `'abcdef'` to `'abc'`. UTF-8 bytes cast to VARCHAR(MAX) with a UTF-8 COLLATE clause returned the text `cafÃ©`. The bytes were reinterpreted in the database's CP1252 code page. Implicit assignment to a VARCHAR(MAX) variable works. |
| Conversion after DECOMPRESS | `CAST(CAST(DECOMPRESS(@v) AS VARCHAR(MAX)) AS INT)` prepared over 'hello' raises 245, state 1, class 16. It sends metadata, no rows and a lone DONEPROC with **no return status**. The next execution with valid input succeeds with status 0, and no stale error is recorded. |
| Large | 8 MB of zeros, 1 MB and 1.8 MB inputs and 64 KB of noise all round-trip. `DECOMPRESS` of the 64 KB noise member returns exactly the original 65,536 bytes (same SHA-256). 30,000 N'xyz' characters round-trip through NVARCHAR(MAX). |

## RPC and prepared protocol shapes

Successful executions: every successful sp_executesql call returned
DONEINPROC (rowCount 1, more) then DONEPROC (rowCount null) with return status
0. Every successful prepared execution returned one column set, the same
DONEINPROC/DONEPROC pair and status 0. The handles cover NVARCHAR(4000),
VARBINARY(MAX) and VARBINARY(8000) declarations, and include empty, NULL and
3,000-character or 20,000-byte values. Each sp_prepare returned the metadata
(an empty result set), then DONEINPROC (rowCount 0) and DONEPROC with status 0.
Each sp_unprepare returned only DONEPROC with status 0.

Failed executions differ, and the fixture keeps each variant:

| Call | Error | Completion tokens | Return status |
| --- | --- | --- | --- |
| `rpc compress int` (sp_executesql, compile-time 8116) | 8116 | DONEPROC only (rowCount null) | **8116** |
| `rpc decompress invalid` (sp_executesql, run-time) | 9826 | Metadata, no rows, DONEINPROC (rowCount null, more), DONEPROC | **9826** |
| `prepared decompress`, execution 2 (sp_execute) | 9826 | Metadata, no rows, DONEINPROC (rowCount null, more), DONEPROC | **-6** |
| `prepared decompress int conversion`, execution 2 (sp_execute, 245 after DECOMPRESS) | 245 | Metadata, no rows, DONEPROC only | **none** (no RETURNSTATUS token) |

So sp_executesql reported the error number as its return status, while
sp_execute reported -6 for the statement-level 9826 failure and sent no status
for the batch-aborting 245 conversion. In both prepared handles, later
executions with valid input returned status 0 and had no recorded errors. In
`prepared decompress`, those were a NULL, a truncated member (NULL result) and
a valid member.

## Gaps not captured

- Inputs over 8 MB, including values near the 2 GB MAX limit, and
  decompressed output that would exceed 2 GB (a decompression bomb). There is
  no observation of memory limits or of the error raised for them.
- The exact algorithm SQL Server's deflate encoder uses: block splitting,
  match selection and when it switches to stored blocks. The fixture has
  byte-exact outputs only up to 256 bytes, and digests above that.
- DECOMPRESS of a member whose deflate stream uses a preset dictionary, or
  invalid Huffman tables after a valid header. Only truncation, trailer and
  header corruption were captured.
- COMPRESS over TEXT/IMAGE columns (literal casts were captured as 8116),
  sparse columns, CLR/UDT, and hierarchyid/spatial types.
- Behavior under other database compatibility levels, other server collations
  and ANSI_WARNINGS/ARITHABORT settings. Only the defaults were captured.
- Use in WHERE, ORDER BY, GROUP BY, indexes, constraints, OUTPUT clauses and
  parallel plans, and repeated evaluation counts.
- TDS partially length-prefixed (PLP) chunking of large VARBINARY(MAX) results
  on the wire. tedious reassembles the chunks, so the fixture keeps only the
  values.

## Proposed successors

These are proposals only. They do not claim that msduck implements anything.

1. **Deterministic core (`msduck-core`): GZIP framing and DECOMPRESS rules.**
   This would be a pure function from bytes to
   `Ok(Some(bytes)) | Ok(None) | Err(9826)`. It would implement the observed
   header parsing (FEXTRA, FNAME, FCOMMENT and FHCRC skipped, FHCRC and reserved
   bits not checked, CM must be 8), NULL on truncation, trailer checks only when
   all 8 bytes are present, and first-member-only handling with trailing bytes
   ignored. An empty member would give NULL and `0x` would give `0x`. It would
   take an explicit maximum output length. The fixture's DECOMPRESS cases and
   the hand-built members in the script serve as test vectors. COMPRESS
   framing (the fixed header `1f8b0800000000000400`, the CRC-32/ISIZE trailer
   and empty input giving 0x) belongs here too. A byte-exact deflate encoder is
   a separate successor, blocked on a specification of SQL Server's encoder.
   Until then an implementation could emit valid, interoperable GZIP that is not
   byte-identical, and would need to record that as a known difference.
2. **Deterministic SQL typing (`msduck-sql`).** Both functions would bind
   exactly one argument. COMPRESS would accept only character and binary
   families (VARCHAR, NVARCHAR, CHAR, NCHAR, VARBINARY, BINARY and MAX forms,
   plus an untyped NULL) and raise 8116 otherwise. DECOMPRESS would accept
   only binary types (and an untyped NULL). Both return nullable
   `varbinary(max)` and are nondeterministic for computed-column and persisted
   checks (4936).
3. **Root adapter.** This would lower the functions onto DuckDB, preserve the
   9826 statement-level error and token sequence (metadata, then the error,
   then DONE with rowCount null), and check the output against the fixture over
   batches, sp_executesql and sp_prepare/sp_execute. It would also bound
   decompressed output against a configured memory limit before allocation.
