# IDENTITY_INSERT value-conversion reference

The [retained SQL Server 2025 capture](../reference/identity-insert-conversion.json) records 19 ordered observations in each of two fresh databases on the pinned image `sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`. The [generator](../scripts/capture-identity-insert-conversion.mjs) preserves rows, full typed descriptors, diagnostics, ordered TDS event kinds, and raw DONE status and command words. It pins fixture SHA-256 `ff35b24b314cdd6529bfdcb56392a5f584e10f97fb4f7bd5674d746b4d922cab`, validates the fixed case plan and outcomes, and refuses fixture aliases, hard links, symlinks and unpinned images. A second independent container replayed both fresh-database runs with exact capture equality.

The table has `INT IDENTITY(10,2)`. After a generated row `(10,1)`, `SET IDENTITY_INSERT dbo.conversion ON` succeeds. The four explicit inserts below fail with no new row and leave `IDENT_CURRENT` at 10:

| Identity expression | Error number/state/class | Exact message | Failed DONE command | TDS event order |
| --- | --- | --- | --- | --- |
| `'bad'` | 245/1/16 | `Conversion failed when converting the varchar value 'bad' to data type int.` | 253 | ERROR, DONE |
| `'2147483648'` | 248/1/16 | `The conversion of the varchar value '2147483648' overflowed an int column.` | 195 | ERROR, INFO, DONE |
| `NULL` | 339/1/16 | `DEFAULT or NULL are not allowed as explicit identity values.` | 253 | ERROR, DONE |
| `CONVERT(INT,'bad')` | 245/1/16 | `Conversion failed when converting the varchar value 'bad' to data type int.` | 253 | ERROR, DONE |

Each failure has DONE status `0x0002` and no result set. The 248 overflow path includes an INFO token after ERROR; the captured fixture retains its full content. A subsequent explicit insert of ID 100 succeeds, moves `IDENT_CURRENT` to 100, and persists `(100,6)`. After `IDENTITY_INSERT` is turned OFF, the next generated row is `(102,7)`. Both successful SET statements use status zero and commands 183/184; successful inserts use command 195 with row count one. State queries retain an `Int` identity descriptor whose flags change from 16 while OFF to 24 while ON, plus a typed `VARCHAR(40)` allocator value, `INT` transaction count and `SMALLINT` transaction state.

These observations establish that the four tested identity-value failures occur before allocator advancement. They complement the [separate failed-write capture](identity-insert-errors-reference.md), where a valid explicit identity value can advance the allocator even when another source expression or a constraint fails. The capture does not establish every conversion type, scale, computed expression, multi-row ordering, or concurrency case. The current msduck engine does not yet execute `IDENTITY_INSERT`; these are reference rules for that integration, not runtime support claims.
