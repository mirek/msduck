# BulkLoad character metadata reference

Task #945 retains 160 explicit probes per database, four fresh databases in two SQL Server 17.0.4065.4 containers. Source SQL declarations, outgoing TYPE_INFO and target capacity vary independently. All prior task899/918/919 fixtures and shared helpers remain unchanged.

| Controls | Count per database |
|---|---:|
| CP1252/CP1251/UTF8 VARCHAR declaration1/8/MAX × wire1/8/MAX × target1/8/MAX | 81 |
| Same three profiles, CHAR declaration1/8 × wire1/8 × VARCHAR target1/8/MAX | 36 |
| Mixed CHAR/VARCHAR family directions, width8/target8 | 6 |
| Three mismatched declaration/wire pairs per profile, separate NULL-only/NULL+empty loads | 18 |
| Per-profile source-declaration overflow versus target-capacity overflow | 6 |
| NVARCHAR1/8/MAX declaration/wire cross and NCHAR1/8 cross, Unicode target8 | 13 |

Each baseline load supplies NULL then ASCII A. Fixed wire CHAR/NCHAR values are padded to exactly their wire width before sending; the original case/input records preserve those bytes. Other controls isolate NULL, empty, or AA. There are 311 supplied rows per database; earlier errors cannot hide a later challenge because each probe uses a separate table/connection. Unicode declaration/wire widths are units; the original TYPE_INFO and descriptor lengths retain the corresponding byte widths.

Every original run agrees: 91 successful loads, 50 errors4816/state2/class16, six errors4816/state1/class16, ten errors4815/state1/class17, and three errors2628/state1/class16. All rejected loads have empty table readback. Complete error messages, INFO, callback, DONE/completion, original @@ERROR/@@ROWCOUNT/XACT_STATE/@@TRANCOUNT, typed descriptors, native binaries, SQL UTF16-unit binaries, client display and packet exchanges remain retained. No error-state or counter normalization is used.

The fitting VARCHAR A controls admit unequal bounded widths1/8 and bounded declarations with MAX/PLP wire metadata against bounded targets. Declared MAX with bounded wire metadata rejects4816/state2, including the isolated NULL-only load. Bounded wire metadata against targetMAX likewise rejects4816/state2. These outcomes separate source declaration from target framing: the earlier targetMAX-only mismatch controls cannot establish general width equality.

Both mixed CHAR/VARCHAR family directions at width8 reject4816/state1. A declared VARCHAR1 with wireVARCHAR8 carrying AA raises4815/state1/class17 even though target8 fits. Declared/wireVARCHAR8 carrying AA into target1 instead raises2628/state1/class16. These distinctions must not become a single guessed capacity check.

Declared CHAR8 with wireCHAR1 supplying native `41` reads back `41` plus seven `20` bytes in targetVARCHAR8. NCHAR8 with wireNCHAR1 similarly supplies `4100` and reads back seven added `2000` units. Fixed-source declaration matters in these actual probes; no arbitrary padding rule is inferred. NVARCHAR/NCHAR wire width8 is independently checked against TYPE_INFO/descriptor length16 bytes, with original units intact.

The retained fixture SHA256 is `efaa65a629e2f0c4aad2f5c6b9e149c75de367fbeaadd9df1212318f280e558a` (6,767,369 bytes). Collector SHA256 is `9fdf66342c7db4549d082eec2f6556cfc916f01d0b1da89b810eeb7b25f0faf9`. All helper/ancestor hashes are embedded in provenance and checked before Docker. The first frozen-source four-run reproduction completed exit0 (SHA256 `65cac7e1c8723b43da15675098a4e639a428815fbfccf785b258694d7670939f`), passing every semantic/request/response/framing pin. Its full 9,205 recursive raw differences are preserved in the sidecar (SHA256 `087c8078e660d71cfae80aea5e40c21e2d1bebb527c32a0209bf57d82a714e12`). A second independent frozen-source reproduction is pending.

Run `node scripts/capture-bulk-character-metadata.mjs --replay-fixture` for offline validation and `node --test tests/bulk_character_metadata_capture.test.mjs` for guard/replay tests. The collector uses cached pinned SQL containers and reviewed Tedious20 dependencies, unique databases and output paths, connect2sec and bounded callbacks/exchanges. It enforces 48MiB/500,000-node/depth64 retention, 2MiB/1,024-frame phase bounds, original source-row limits and pre-Docker output/alias/symlink guards. Raw evidence survives semantic failure; derived comparison exhaustion preserves the original bounded object and an explicit omission/failure sidecar. Container/database cleanup is awaited. Uniform semantic, counter, width, ROW length and packet-framing corruption cannot pass by recomputing comparisons; tests also verify completeness/provenance and alias/limit failure retention.

This finite matrix does not establish arbitrary widths, values, source families/collations, invalid raw TYPE_INFO admission, general malformed decoding, every bulk protocol error or the order of arbitrary simultaneous failures. Metadata admission and source ROW length are distinct from target conversion/capacity; no runtime admission, collation gate, Rust engine or native build changes accompany this reference. Original/fresh raw captures and complete sidecars remain separately in the claiming worker's ignored staging. Identity-aware comparison digests check ephemeral names/SPIDs without altering original observations or difference arrays.
