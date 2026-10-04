# Explicit native carrier projections

`msduck::ansi_carrier::projection::Plan` registers a caller-owned, connection-local DuckDB function over the existing tagged ANSI carrier. The explicit source identity, target identity, per-cell input/output byte limits and per-chunk input/output limits are declaration facts. The bridge calls the completed deterministic projection kernel and returns a native target carrier or the existing UTF16 carrier. It introduces no lossy text intermediate.

`Plan::new` admits CP1251/CP1252 complete projections and valid UTF8 identity/UTF16 projections. It remains strict: malformed UTF8 fails even when SQL Server admits those bytes in another context. Raw native storage still preserves malformed input independently. Unsupported plans fail before NULL handling; physical STRUCT shape never supplies logical ANSI identity by itself.

`Plan::stored_utf8_to_sql_utf16` explicitly selects the completed deterministic stored-value decoder for a UTF8 native carrier and a UTF16 target. Its source and target identities are declaration facts, including for NULL and empty values. It reproduces SQL-specific EOF fitting, selector pairing and malformed repair: `8042` projects to `FDFF4200`, `EDA080` to `FDFFFDFF`, and `41E1A0` to an empty non-NULL UTF16 payload. Standalone `80` preserves its native bytes but fails Unicode projection with the distinct stored-boundary error. This plan does not change the strict constructor or admit a BulkLoad row.

The callback validates physical type/capacity, source tag and non-NULL child validity before reading payloads. It checks the complete input chunk and prepares every output payload under checked cell/chunk budgets before mutating the output vector. The retained payloads occupy at most the output chunk budget; serializing UTF16 temporarily retains one additional bounded output cell. Limits count active payload bytes rather than allocator capacity. NULL/empty stay distinct. The function accepts one original operand, so its backend evaluation is not duplicated.

These functions are explicit backend adapter entry points, not SQL CAST/assignment/BulkLoad capacity rules or an operational collation gate. Normal Value conversion, catalog acquisition, engine registration/lowering and result output still require coordinated follow-ups. The current BulkLoad loader CP1252-decodes ANSI bytes and the current result writer does not consume native encoding facts. No server endpoint becomes compatible merely because these functions can be registered.

## Retained evidence and native verification

Native integration tests project original source bytes through the real callback, materialize the resulting carrier in a table and inspect its scalar/Arrow bytes. Four-run controls from conversion895, CP1252 table906, UTF8 boundary913 and bounded-target919 contribute 784 declaration-selected observations: 236 failed SQL loads remain failed, and 184 malformed rows remain outside strict UTF8. They supply 11,996 complete projection comparisons. Earlier encoding884 supplies another 104 observations, 400 original primary-value comparisons, 24 failed loads and 16 malformed rows outside this contract. Original fixtures and their SHA256 pins remain unchanged.

For a cross-codepage native result, a later SQL Unicode projection describes the *converted target*, not necessarily the original source. Tests compare that operation's actual native bytes; direct source-to-UTF16 expectations come only from an appropriate same-encoding, UTF8 or Unicode control. They do not reinterpret a lossy CP1251-to-CP1252 readback as direct CP1251 Unicode ground truth. Fixed padding/capacity probes and unknown admission failures are not converted into successful complete-projection expectations.

The actual predecessor storage module at `6695bf85a37226dfe0be93007374d25f982a9d72` compiles against compatible cached dependencies but rejects the new projection-plan API import. The corrected native suite verifies all original payload comparisons plus NULL/empty, supplementary units, zero-row physical shape checks, tag/child/encoding rejection, unchanged initialized output after late tag or aggregate-limit failure, materialization/Arrow identity, transaction rollback and connection reuse, and exactly one evaluation per volatile input across 10,000 rows/multiple chunks. Private metadata-only predecessor proof is not a substitute for the real native test suite or full workspace/client gates.

UTF16 target plans additionally obey the existing Unicode carrier ceilings: 16 MiB per cell and 64 MiB per chunk. A larger caller limit is rejected during plan construction, before any NULL/value or allocation; native targets retain their independently configured native-storage budgets. Exact-boundary and one-above constructor tests reproduce the previous adapter's acceptance bug and verify the corrected limits. These ceilings keep emitted UTF16 carriers readable by their existing consumers.

## Stored UTF8 adapter verification

Task #936 merged in PR #937. Native callback tests replay all6088 original outcomes from the unchanged stored projection fixture, SHA256 `04b4b49603116046ac475d31a244207aa61054a672aacaffa976a106800961e9`: all native identities,5476 Unicode successes including NULL/empty, and612 boundary failures. All33 frozen public EOF/selector controls are also exercised through DuckDB. Tests preserve the distinction between SQL Server9833 in the reference and the backend callback's stored-boundary error; the adapter does not fabricate SQL Server tokens or claim engine integration.

Stored-plan construction uses the same existing Unicode carrier ceilings and cell/chunk consistency checks. Original input budgets apply before EOF fitting, even when fitting would discard every byte. Private callback tests verify exact physical source/output shape on zero rows, typed boundary/resource causes, source-tag/NULL-child rejection and unchanged initialized UTF16 output after late errors. SQL tests verify statement-atomic failure across10000 rows and at per-cell/per-chunk output limits, rollback and connection reuse. Native bytes and projected UTF16 values survive database restart and Arrow reads. A volatile original is evaluated exactly once per row across10000 rows, with NULL/empty/malformed/supplementary/ASCII values shifting positions across chunks.

Full frozen-revision formatting, strict workspace Clippy, workspace Rust, all520 client tests, independent/Codex reviews and required CI passed at `218d546d02db08e956433bbf24651314aca992d2`. The325-case audit retained four raw cell differences against its baseline: the two rows of a derived-table APPLY query without ORDER BY exchanged positions. All other fields matched; raw evidence was preserved without normalization. This local audit records existing gaps, rather than proving full SQL Server compatibility. Operational UTF8 catalog/Value/BulkLoad/wire consumers remain separately coordinated; registering this internal function does not enable a collation gate or public endpoint behavior.

## Explicit BulkLoad capacity adapter

`Plan::bulk_capacity` takes a `CapacityDeclaration` containing source encoding,
bounded/MAX source form, target encoding, fixed/variable target family and SQL
capacity. It validates these declaration facts with the completed deterministic
`capacity::Plan` before any nullable input. CP1251/CP1252/UTF8 sources are supported;
opaque sources remain unsupported. Native CP1251/CP1252/UTF8 and SQL
UTF16 targets retain their exact physical identities. The strict and stored
projection constructors retain their independent behavior.

The root must first admit the real source declaration, TYPE_INFO and catalog
profile. This adapter does not infer those facts from a row, validate a source
CHAR-to-MAX wire declaration, or apply a universal SQL CAST/assignment policy.
The capacity kernel preserves the captured bounded/MAX overflow windows,
cropping and fixed padding described in [ansi-capacity.md](ansi-capacity.md).
SQL capacity and caller resource limits are independent: original input bytes
are bounded before fitting, fixed padding counts against output budgets, and
UTF16 outputs retain the existing Unicode carrier ceilings. NULL is distinct
from an empty input that pads to a nonempty fixed result.

The selected mode uses the same whole-chunk shape/tag/child/input preflight and
checked output preparation as complete projections. A truncation is a typed
`CapacityError`, distinct from byte/resource errors. Callback errors do not
emit SQL Server2628 tokens or establish whole-BulkLoad transaction, counter or
trigger behavior. Every fallible payload preparation precedes output mutation;
the backend statement-atomicity tests cover this callback boundary.

Task #938 merged as #939. Its native suite retains all476 admitted original
observations from the unchanged conversion895, capacity899 and trailing909
references (3248 row applications,64 failed loads), plus all1144 four-run
CP1251 native/error oracles (2336 applications,920 failed loads). The latter
are read directly from the original losslessly packed literal retained by the
completed core tests, pinned at SHA256
`3c03d3bc52f7aba997518a87cad832d11f296e71ce3cc13ca972665a4a49367b`.
No expected result is generated by the kernel being tested. Native failure
checks preserve the distinction between reference2628 and callback truncation;
they do not substitute cell evaluation for complete endpoint token evidence.

All27 focused native tests and strict workspace all-target Clippy passed in
the existing locked Linux cache. Full frozen-revision verification, independent/
Codex reviews and required CI passed at `02d28278eb5658745fecd0f3d9a6560bdebc7859`.
All520 clients passed and the325-case local audit retained zero raw differences
from #937; #939 merged as `ca9a19752d6a27bb16f5a557df3c997b18dd0063`. Engine/Value/catalog/BulkLoad/wire adoption is still required
for public runtime behavior; this internal adapter does not enable a collation.


Task #944 pairs with the deterministic UTF8 capacity extension #942. The bulk
constructor validates its own capacity domain before shared storage/resource
validation, allowing UTF8-to-codepage capacity without relaxing strict complete
projection. `Plan::new` still rejects UTF8-to-CP1251/CP1252 even for NULL. Whole
chunk shape/tag/child/input checks and fallible output preparation are unchanged.
The new callback suite replays all512 original task940 native/error outcomes
(264 failed loads), preserving exact target bytes and SQL UTF16 units. Its native
build/full verification is pending; source registration remains an internal
caller-owned operation with no endpoint collation gate enabled.
