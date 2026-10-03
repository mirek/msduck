# Lossless BulkLoad character adapters

This is an implementation contract for task #890, reviewed against source at
`27e3fa4170a30288815bb0052df537d0545c672a`. It does not implement these adapters
or establish BulkLoad compatibility. The target is the complete path from client
bytes through SQL conversion and storage to native binary and character results.

The first-party SQL Server evidence is [the character reference](bulk-character-encoding-reference.md)
and [its raw fixture](../reference/bulk-character-encoding.json): four independent
databases, two pinned containers, 33 cases per database, SQL Server 17.0.4065.4,
SHA256 `0d2cff08bc7f59d271955311ac540f95a37f31b456bc008f739117c1a09d3a83`.
The fixture remains immutable. Copied upstream codec behavior is implementation
input, not a substitute for these observed SQL Server results.

## Current path and information loss

| Boundary | Current source contract | Remaining requirement |
| --- | --- | --- |
| Wire tokens | [`bulk_load::TypeInfo`](../crates/msduck-tds/src/bulk_load.rs) retains `collation: Option<[u8; 5]>`; `Value` retains `Option<Vec<u8>>`. The decoder preserves bytes, NULL/empty and PLP independently of character decoding. | Carry both facts into the root adapter; do not decode a partial packet or PLP chunk. |
| Statement syntax | [`bulk::Column`](../crates/msduck-sql/src/dialect/ext/bulk.rs) retains `COLLATE` separately from `data_type`. | Preserve the original declaration collation and distinguish it from wire collation and target column collation. Their precedence and disagreement diagnostics need reference probes. |
| Target binding | [`plan::prepare`/`Bound`](../src/engine/ext/bulk/plan.rs) resolve only `column.data_type`, build `declared_text` from that type and retain no source collation. `TargetColumn` retains flags but no logical target encoding/collation. | Acquire explicit catalog facts in the root and retain them in the bound plan. Declaration text must not silently drop COLLATE when constructing a stage. |
| Row conversion | [`wire::value`](../src/engine/ext/bulk/wire.rs) decodes every ANSI character/text wire family with `decode_cp1252`. [`Load::add`](../src/engine/ext/bulk/load.rs) immediately packages that result with `bound.declared`. | Keep native source bytes until an admitted source-to-target conversion is selected. Character decoding failure must not become a generic internal error or a guessed SQL error. |
| Logical binding | [`Parameter`](../crates/msduck-sql/src/parameter.rs) carries logical `Type` and core `Value`; [`Value`](../crates/msduck-core/src/value.rs) has `Text(String)`, raw UTF16 `Unicode`, and unrelated binary `Blob`. | Accepted malformed ANSI bytes need a distinct carrier. `Text` cannot represent them; passing a `Blob` under a character declaration does not establish character conversion semantics. |
| Backend storage | [`backend_value`](../src/backend_value.rs) maps Text to DuckDB text and Unicode to a binary binding. [`unicode_carrier`](../src/unicode_carrier.rs) supplies existing tagged UTF16 storage and native conversion adapters. | Add a distinct native ANSI representation with checked tag/payload handling. DuckDB UTF8 text alone cannot retain all accepted native UTF8 bytes. Keep SQL VARBINARY distinct. |
| Catalog support | [`declared_columns::lower_collation`](../src/declared_columns.rs) gates column collations through operational `Collation::for_name`; [`query_catalog::wire_collation`](../src/query_catalog.rs) uses that gate for descriptors. | Extend operational support only together with storage, conversion and output evidence. Recognizing metadata is not enough to enable a collation. |
| Result output | [`Aligned::column`](../src/result_metadata.rs) supplies a collation descriptor. [`engine::encode_column`](../src/engine.rs) passes only kind/fixedness to `encode_value_mode`, whose VARCHAR text branch encodes CP1252. | Pass explicit result encoding/native-byte facts to the writer. The actual bytes must agree with the descriptor, including empty/NULL/error results. |

[`Collation::descriptor_for_name`](../crates/msduck-tds/src/collation.rs) deliberately
separates additional captured descriptor names from operational `for_name`.
Keep that separation. Suffix matching or adding a name to the operational gate
would enable unsupported consumers elsewhere.

## Required value and conversion contract

The core should own an immutable native ANSI byte carrier and a borrowed view.
Its semantic encoding identifier is supplied explicitly; it does not depend on
TDS types, parser nodes, a catalog lookup, environment state or DuckDB. Original
five-byte descriptors remain at the TDS/root boundary. NULL is separate from an
empty byte sequence. Constructors and adapters enforce explicit allocation and
length limits before copying, with checked byte accounting. The carrier does
not certify a collation, decode unknown bytes, or infer SQL character counts.

The root must retain three distinct inputs: wire collation, INSERT BULK source
declaration, and logical target declaration/catalog collation. After validating
their supported contract, it calls deterministic conversion with explicit
source/target encoding, family and capacity. Decoding, best-fit conversion,
storage overflow and final wire encoding are separate operations. A wire writer
must encode already converted native target bytes without doing best-fit work.
Compile metadata depends on declarations and explicit catalog facts, never on
the current row's value or whether its bytes happen to be ASCII.

For the captured same-encoding VARCHAR loads, accepted bytes must survive
unchanged through storage and `CAST(value AS VARBINARY(MAX))`. SQL Unicode
projection and client VARCHAR decoding remain different observations. CP1252
`818d90` maps to SQL units 0081/008D/0090 while tedious reports U+FFFD; replacing
the stored units with the client's display would lose data. For UTF8 `eda080`,
the native bytes remain `eda080`, tedious reports three U+FFFD characters and
SQL Unicode conversion reports two. Neither Rust lossy decoding nor strict
UTF8 validation is a universal SQL conversion rule.

The captured malformed loads include a valid ASCII row before the bad row.
`80` and `c0af` fail with 9833/state2/class16 plus 3621; `f09f92` fails with
7339/state1/class16 and no 3621. Both leave no target rows. `c328` and `eda080`
are accepted in the three probed targets. The diagnostic contract includes the
full error/info messages, DONE and callback facts and post-load transaction
state, not just a replacement string or an error number. Additional malformed
inputs remain unproven. A failure discovered after staging must still preserve
whole-load atomicity, caller transaction state and bounded resource cleanup.

The backend representation should be a distinct tagged native-byte carrier,
analogous to the existing UTF16 carrier, with encoding facts attached through a
validated plan. A user-created BLOB or arbitrary STRUCT must not automatically
become character data. This is a proposed representation: a native prototype
must prove storage/Arrow/vector NULL behavior and round-trip identity before it
is integrated. Ordinary valid text may use a fast path only when the same byte,
encoding and declaration contract is proved; it cannot erase the raw path.

An initial root bridge can keep a separate `BulkInput` character payload rather
than immediately converting it to the common `Parameter`. Bind raw bytes as a
genuine binary operand to an explicit native pack/store expression; retain the
original character declaration and source/target facts on that expression's
plan. This avoids pretending a BLOB is already a VARCHAR parameter. The prototype
must prove that logical metadata survives this lowering and that subsequent
casts/readback return the right representation. If that bridge is insufficient,
expanding `Value` or `Parameter` requires separately claimed changes to their
definitions and all affected adapters; it is not an implicit part of the small
carrier task.

## Evidence still needed

The 33 cases establish the listed observations, not a complete conversion map.
Additional independently repeated probes must establish:

- Every byte of the CP1251 source table, undefined-byte behavior and actual
  source-to-Unicode/native projections. A guessed Windows table is insufficient.
- Source declaration versus wire collation conflicts, missing/unknown wire
  descriptors, target inheritance, column COLLATE and legacy CHAR/TEXT behavior.
- UTF8 invalid leads/continuations, truncated sequences at each position,
  overlong/surrogate/out-of-range sequences, mixed valid/invalid inputs and
  boundaries inside outgoing packets and PLP chunks. The retained response
  already splits a four-byte character; outgoing observed boundaries are
  between characters and must not be described as inside-character evidence.
- Exact bounded-width overflow, fixed padding, conversion expansion and MAX
  behavior for each source/target pair. Capture conversion-before-capacity
  precedence rather than deriving it from character counts.
- Best-fit/unrepresentable mappings and Unicode surrogate handling beyond the
  finite éΩ🦆 and Привет controls. Current Ω-to-CP1252 O is a captured mapping,
  not a general transliteration rule.
- Atomic errors after the large-load staging threshold and in explicit
  transactions, composed with the separate defaults/triggers/constraints
  reference. The small character reference cannot prove those combinations.

Raw captures, supplied bytes, native projections, descriptors, error identity,
version/image/client provenance and complete differences must be retained before
gold validation. Evidence that differs between independent runs stays visible.
Do not weaken existing expectations or normalize invalid input to obtain a pass.

## Ordered implementation work

The following are proposed bounded tasks. They are not reservations or approved
implementation receipts; publish and claim each before editing. Recheck live
scopes at activation. Source changes beyond the listed paths require a companion.

| Step | Proposed exact scope | Dependencies and proof |
| --- | --- | --- |
| Native-byte carrier | `crates/msduck-core/src/ansi_bytes.rs`, `crates/msduck-core/tests/ansi_bytes.rs`, `docs/ansi-bytes.md` | This plan. Preserve all bytes including malformed UTF8; explicit encoding identity, NULL/empty separation, borrowed/owned bounds and checked accounting. Pure tests; no conversion or server compatibility claim. Export needs a separately coordinated `crates/msduck-core/src/lib.rs` companion: existing `legacy-datetime-export-v1` owns that path. Recover its stale ownership only with the contribution protocol's evidence, never by borrowing its receipt. |
| Raw ANSI wire writer | `crates/msduck-tds/src/ansi_value.rs`, `crates/msduck-tds/tests/ansi_value.rs`, `docs/ansi-wire-value.md` | This plan and retained reference. Encode converted native bytes with exact bounded/MAX/NULL/empty framing; enforce byte capacity before output mutation. Replay original payload values and fragmentation controls without decoding/replacing them. Export requires coordination of `crates/msduck-tds/src/lib.rs`, reserved by owner task #853. |
| Conversion ground truth | `scripts/capture-bulk-character-conversion.mjs`, `reference/bulk-character-conversion.json`, `tests/bulk_character_conversion_capture.test.mjs`, `docs/bulk-character-conversion-reference.md` | This plan; reuse private reference resources, no shared builder writes. Cover the missing contracts above in bounded captures; separate actual native bytes, SQL units and client decoding. |
| Pure conversion rules | `crates/msduck-core/src/ansi_conversion.rs`, `crates/msduck-core/tests/ansi_conversion.rs`, `docs/ansi-conversion.md` | Reviewed conversion reference and carrier. Exact input domains, best-fit/padding/overflow and diagnostics established by that reference; unsupported profiles stay explicit. A finite evidence table must not be presented as a general decoder. Coordinate core export separately. |
| Native storage adapter | `src/ansi_carrier.rs`, `src/backend_value.rs`, `tests/ansi_carrier.rs`, `docs/ansi-carrier.md` | Carrier and conversion rules; export/registration companions for `src/lib.rs` and required root hooks. Prove raw/native identity, logical type separation, vector bounds/NULL and single evaluation using DuckDB. Do not alter the common Value enum without separately claiming every affected adapter. |
| Bulk binding/loading | `src/engine/ext/bulk/wire.rs`, `src/engine/ext/bulk/plan.rs`, `src/engine/ext/bulk/load.rs`, `tests/bulk_character_encoding.rs`, `tests/compat/bulk_character_encoding.test.mjs`, `docs/bulk-character-encoding.md` | Conversion rules, storage adapter, reviewed missing evidence, and `bulk-staging-semantics-v1` (#882), which shares plan/load. Retain all source/target facts, apply actual conversion/diagnostics and test real socket bytes, native binary projections, capacities and atomic staging failure. |
| Catalog and output integration | `src/declared_columns.rs`, `src/query_catalog.rs`, `src/result_metadata.rs`, `tests/ansi_result_encoding.rs`, `tests/compat/ansi_result_encoding.test.mjs`, `docs/ansi-result-encoding.md` | Complete storage/conversion/writer and catalog predecessors. Current #868 owns declared-columns/collation paths and #870 owns query_catalog. A coordinated owner-approved `src/engine.rs` companion is also necessary: the current output call discards column collation. Do not reserve or modify that file during the owner's #853 work. Verify exact bytes/descriptors for ordinary/empty/NULL/error results and subsequent character/binary operations. |

Keep each deterministic module in its appropriate crate. Catalog acquisition,
backend/vector registration, sessions, transaction cleanup and transport belong
in root adapters. Publish implementation tasks with these dependencies rather
than enabling names or conversions ahead of their consumers. Successful pure
carrier or codec tests alone cannot close the runtime task.

## Completion gates

Each implementation worker owns review follow-up, exact-head CI, merge and
registry/project completion. Rust behavior changes require formatter,
workspace Rust tests, strict all-targets Clippy, independent tedious clients and
diagnostic audit under the shared build lock. Save the exact revision and raw
audit differences; audit success is not comparison with SQL Server. Use the
owner-enabled Codex review or its genuine availability fallback, with independent
review of encoding/metadata/atomicity boundaries.

The combined implementation is complete only after the socket tests reproduce
the retained and additional reference's original rows, native bytes,
descriptors, errors/info, DONE/callbacks and transaction state for all admitted
inputs. Unknown profiles and unverified semantics remain recorded gaps in the
full server objective. This document does not close them.
