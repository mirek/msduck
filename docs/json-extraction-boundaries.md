# JSON extraction validation-order reference

`reference/json-extraction-boundaries.json` retains 80 `JSON_VALUE` and
`JSON_QUERY` requests against the pinned SQL Server 2025 image. The owner-run
capture repeats every request in two fresh databases, checks both runs exactly,
and retains rows, typed result descriptors, diagnostics, DONE events and their
relative event order. A later
independent capture matched the fixture. Regenerate a scratch capture with
`node scripts/capture-json-extraction-boundaries.mjs`; the fixture cannot be
overwritten by that command. The image digest and each SQL request are in the
fixture. The evidence is limited to these exact requests and that server image.

The captured selection order is observable:

| Request shape | Captured behavior |
| --- | --- |
| `$.a` finds an early scalar in `{"a":1,"b":x}` | `JSON_VALUE` returns `1`; the malformed suffix is not visited. |
| `$.a` finds an early container in `{"a":{},"b":x}` | `JSON_QUERY` returns `{}`. A strict `JSON_VALUE` reports 13623/state 2 before the malformed suffix. |
| `$.missing` on those malformed documents | Both functions report 13609/state 1 at the later `x`, including in strict mode, before a missing-path diagnostic. |
| `$.a` selects from an unclosed parent (`{"a":1` or `{"a":{}`) | The scalar or complete container can still return; a truncated selected object reports 13609. |
| `$` on a scalar JSON document (`1`, `"text"`, `null`) | Both functions report 13609/state 1 at position 0. The extraction APIs require an object or array root for these requests. |
| `$.a[*]` on `{"a":[1]}` | SQL Server 2025 accepts this path; `JSON_VALUE` returns `1`, while `JSON_QUERY` returns NULL. This is one wildcard shape, not full wildcard grammar. |
| A typed `NVARCHAR(100)` NULL path | Both functions emit an NVARCHAR descriptor, then 8116/state 8, and a final DONE with null row count. |
| A malformed `$.a[0` path | Both report 13607/state 14 at position 5 before result metadata. |

For ordinary scalar and fragment requests the descriptor is NVARCHAR with
flags 33 and source collation. `JSON_VALUE` has length 8000 bytes (4000 UTF-16
units). The query with an `NVARCHAR(MAX)` source expression has length 65535
for `JSON_QUERY`; descriptor width therefore depends on source expression in
this capture. A 4000-unit selected string returns from `JSON_VALUE`, while
4001 units returns NULL in lax mode. The overlong value followed by malformed
text also returns NULL without reporting the later syntax error. Most captured
extraction errors have no row or result metadata; the typed NULL-path case
emits a descriptor before its error.

The current implementation's `json_path::extract` already descends early for
selected paths and validates the full document for missing paths, but its
documented `JSON_VALUE`/`JSON_QUERY` behavior has not been compared against this
fixture at the wire level. The current core `root` check and path parser need a
focused successor for the captured root-scalar rejection and wildcard shape.
The root adapter and result-metadata path need separate work for typed NULL-path
error ordering and source-dependent descriptors. Broader wildcard/range syntax,
source collations, detailed syntax positions and error precedence remain open.
No runtime parity is claimed by this reference capture.
