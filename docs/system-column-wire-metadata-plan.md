# System column TDS descriptor plan

This inventory uses the two identical fresh captures in `reference/system-all-columns.json` from the pinned SQL Server 2025 image (`mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`). It starts from owner-authored reference revision `43d740e2b0f16bdac1661a4bd275b50859b4e527` (merged in `2a8d235`) and runtime PR #179 revision `55f1feda043cd8bf8e8001338da148fae0639448`, a descendant of `849e5acf79b4bb936cfd8d91a3d6accd20f28ed9`. Each descriptor query selected `* WHERE 1=0`: the 43 columns and their metadata are present even with zero rows. Both reference runs agree byte-for-byte as JSON on the descriptor arrays. The value seed in PR #179 is separate evidence and cannot prove any wire descriptor.

The table is the complete captured TDS inventory. `C`, `S`, and `A` are the numeric flags for `sys.columns`, `sys.system_columns`, and `sys.all_columns` respectively. Type and length are the same across views except where the type cell lists `C/A ; S`. `—` means the capture field is JSON null, rather than an inferred value. **Every one of the 43 fields has null precision and null scale in all three captured wire descriptors.** Collation `D` is LCID 1033, flags 13, version 0, sort ID 52, code page CP1252; `R` is LCID 1033, flags 1, version 0, sort ID 0, code page CP1252. The capture has `buffer.kind = missing` for both collations, so no buffer bytes are asserted. `—` in the collation column is JSON null. These are wire fields, not the numeric declarations returned by querying `sys.columns` itself.

| # | Field | TDS type (length) | C | S | A | Collation |
| ---: | --- | --- | ---: | ---: | ---: | --- |
| 1 | `object_id` | `Int` | 8 | 8 | 8 | — |
| 2 | `name` | `NVarChar(256)` | 9 | 33 | 9 | D |
| 3 | `column_id` | `Int` | 8 | 8 | 8 | — |
| 4 | `system_type_id` | `TinyInt` | 8 | 8 | 8 | — |
| 5 | `user_type_id` | `Int` | 8 | 8 | 8 | — |
| 6 | `max_length` | `SmallInt` | 8 | 8 | 8 | — |
| 7 | `precision` | `TinyInt` | 8 | 8 | 8 | — |
| 8 | `scale` | `TinyInt` | 8 | 8 | 8 | — |
| 9 | `collation_name` | `NVarChar(256)` | 33 | 33 | 9 | D |
| 10 | `is_nullable` | `BitN(1)` | 33 | 33 | 9 | — |
| 11 | `is_ansi_padded` | `Bit` | 32 | 32 | 8 | — |
| 12 | `is_rowguidcol` | `Bit` | 32 | 32 | 8 | — |
| 13 | `is_identity` | `Bit` | 32 | 32 | 8 | — |
| 14 | `is_computed` | `BitN(1) C/A; Bit S` | 33 | 32 | 9 | — |
| 15 | `is_filestream` | `Bit` | 32 | 32 | 8 | — |
| 16 | `is_replicated` | `BitN(1) C/A; Bit S` | 33 | 32 | 9 | — |
| 17 | `is_non_sql_subscribed` | `BitN(1) C/A; Bit S` | 33 | 32 | 9 | — |
| 18 | `is_merge_published` | `BitN(1) C/A; Bit S` | 33 | 32 | 9 | — |
| 19 | `is_dts_replicated` | `BitN(1) C/A; Bit S` | 33 | 32 | 9 | — |
| 20 | `is_xml_document` | `Bit` | 32 | 32 | 8 | — |
| 21 | `xml_collection_id` | `Int` | 8 | 32 | 8 | — |
| 22 | `default_object_id` | `Int` | 8 | 32 | 8 | — |
| 23 | `rule_object_id` | `Int` | 8 | 32 | 8 | — |
| 24 | `is_sparse` | `BitN(1) C/A; Bit S` | 33 | 32 | 9 | — |
| 25 | `is_column_set` | `BitN(1) C/A; Bit S` | 33 | 32 | 9 | — |
| 26 | `generated_always_type` | `IntN(1)` | 33 | 33 | 9 | — |
| 27 | `generated_always_type_desc` | `NVarChar(120)` | 33 | 33 | 9 | R |
| 28 | `encryption_type` | `IntN(4)` | 33 | 33 | 9 | — |
| 29 | `encryption_type_desc` | `NVarChar(128)` | 33 | 33 | 9 | R |
| 30 | `encryption_algorithm_name` | `NVarChar(256)` | 33 | 33 | 9 | R |
| 31 | `column_encryption_key_id` | `IntN(4)` | 9 | 33 | 9 | — |
| 32 | `column_encryption_key_database_name` | `NVarChar(256)` | 33 | 33 | 9 | D |
| 33 | `is_hidden` | `BitN(1) C/A; Bit S` | 33 | 32 | 9 | — |
| 34 | `is_masked` | `Bit` | 32 | 32 | 8 | — |
| 35 | `graph_type` | `IntN(4)` | 33 | 33 | 9 | — |
| 36 | `graph_type_desc` | `NVarChar(120)` | 9 | 33 | 9 | R |
| 37 | `is_data_deletion_filter_column` | `BitN(1) C/A; Bit S` | 33 | 32 | 9 | — |
| 38 | `ledger_view_column_type` | `IntN(4)` | 33 | 33 | 9 | — |
| 39 | `ledger_view_column_type_desc` | `NVarChar(120)` | 9 | 33 | 9 | R |
| 40 | `is_dropped_ledger_column` | `BitN(1) C/A; Bit S` | 33 | 32 | 9 | — |
| 41 | `vector_dimensions` | `IntN(4)` | 33 | 33 | 9 | — |
| 42 | `vector_base_type` | `IntN(1)` | 33 | 33 | 9 | — |
| 43 | `vector_base_type_desc` | `NVarChar(20)` | 33 | 33 | 9 | R |

The flag patterns are not interchangeable: 8 means stored/non-null, 9 stored/nullable, 32 expression/non-null, and 33 expression/nullable in the current TDS encoder (`crates/msduck-tds/src/lib.rs`, `metadata`). `sys.system_columns` gives computed/non-null `Bit` flags 32 for many boolean fields and for `xml_collection_id`, `default_object_id`, and `rule_object_id`; `sys.all_columns` does not inherit those flags uniformly. The captured `Bit` versus `BitN(1)` and `Int` versus `IntN(1|4)` differences also require the correct nullability proof before choosing fixed-scalar TYPE_INFO. The catalog's visible values do not establish any such proof.

## Current implementation and evidence

PR #179 publishes 43 value columns in `src/columns.sql` and stores the two source memberships separately. Its owner-run empty-result TDS probe reported **43 descriptor differences in each of the three views**, with 43 columns and zero SQL errors. The retained description in `docs/system-all-columns-runtime.md` gives `object_id` as nullable `IntN(4)`, flags 1, instead of captured fixed `Int`, flags 8, and `name` as `NVarChar(65535)`, flags 1, instead of `NVarChar(256)`, flags 9 for `columns`/`all_columns` or 33 for `system_columns`. It does not retain all 129 current descriptor objects; therefore the aggregate 43/43 comparison is an observed count, while individual current descriptors beyond those examples remain unverified. A follow-up differential test must persist those arrays before claiming a complete comparison. The owner-run value probe found zero differences for two selected object IDs, not for all rows or descriptors.

The source trace identifies these boundaries:

1. `src/query_catalog.rs` takes an explicit catalog snapshot before lowering. `system_catalog_fields` handles index and object views but has no `columns`, `system_columns`, or `all_columns` case. Its fallback queries `sys.columns` joined to `sys.objects`. The retained capture assigns those three views IDs `-391`, `-392`, and `-103`, and puts all 43 of each view's own catalog rows in `sys.system_columns`, with **zero** in `sys.columns`. PR #179 preserves that membership in `src/columns.sql`. Thus the fallback returns no fields for these views, and the captured per-view origins, widths, nullability and collations never enter this snapshot. This is a source-proven cause of the base `SELECT *` inference gap, although later alignment and backend type selection still determine each final mismatch.
2. `crates/msduck-sql/src/projection.rs` propagates fields from explicit `CatalogSnapshot` inputs. `src/result_types.rs` builds some wire overrides from logical types; `from_fields`/`fill_fields` currently maps bounded character/binary and selected temporal/currency declarations but does not supply a complete catalog descriptor. The engine acquires `result_fields` before backend lowering, then acquires `result_types` and eventually calls `encode_batches_inner` (`src/engine.rs`).
3. `src/result_metadata.rs::Aligned` applies fields and declared types only when their complete widths match the physical Arrow schema; otherwise it discards that entire list. The engine chooses some types from declarations, some from Arrow, then calls `crates/msduck-tds/src/lib.rs::metadata`. That encoder already derives origin/nullable flags from `Properties`, chooses fixed scalar tokens only with `nullable == Some(false)`, and writes bounded NVARCHAR and named collations. The evidence therefore points first to catalog field acquisition and alignment rather than a missing TDS token kind. A full probe must verify that inference after integration.

Do not patch metadata after encoding or use runtime row values to infer types. The same descriptor must be returned for empty, filtered, and nonempty results, and the row encoder must obey its selected fixed/nullable shape. Unknown catalog facts should remain unknown, not be filled with a generic non-null guess.

## Bounded implementation sequence

1. **Captured field model and pure checks.** Add a root-side `src/system_column_metadata.rs` with explicit per-view `Field` declarations for these 43 columns, derived from the retained `… declarations` and descriptor observations. Add focused fixture-backed checks inside that module for all names/order, SQL type IDs/widths, nullability, expression origin, and collation, including the distinct `system_columns` cases. Scope only this new file. This task does not wire it into query binding. It can be published while the existing `src/query_catalog.rs` claim remains active.
2. **Catalog binding and wire replay.** After the active `concat-integration-v1` claim on `src/query_catalog.rs` is completed or explicitly handed off, connect the model through `system_catalog_fields`, and adjust `src/result_types.rs` only where a captured type cannot be produced by existing physical conversion. Scope `src/query_catalog.rs`, `src/result_types.rs`, `tests/system_column_wire.rs`, and a dedicated new `tests/system-column-wire.test.mjs`; do not include `src/engine.rs` or the already claimed `tests/tedious.test.mjs` unless a separate owner handoff is established. The integration test should query `SELECT * FROM sys.{columns,system_columns,all_columns} WHERE 1=0`, one filtered built-in owner, and a newly created user table with a named DEFAULT, comparing every descriptor property and all rows/errors/completions with the retained reference where applicable. Also test a projected subset and aliases so full-width alignment cannot conceal a partial mapping. Capture and retain the complete current/after TDS arrays as diagnostic artifacts; require all 129 descriptor objects to match before declaring this task done. Run deterministic, workspace, strict Clippy, client, and diagnostic audit checks at the exact head.
3. **Follow-on gaps.** If field binding alone leaves a proven mismatch, publish a narrower successor for the demonstrated `src/engine.rs` or TDS encoder path only after its active claim is released. Keep generated unnamed DEFAULT identities, rejected `ALTER TABLE ... ADD COLUMN ... CONSTRAINT ... DEFAULT`, and permission filtering as separate semantic tasks; correcting descriptors must not be presented as completing those behaviors.

The proposed scopes do not overlap one another. The second is intentionally **not ready for parallel claiming** while `concat-integration-v1` owns `src/query_catalog.rs`; owner publication must check the protected registry again and wait for completion or explicit handoff. No task here edits the value seed or SQL Server reference capture.
