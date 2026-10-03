# Temporal and GUID text core

Task #836 adds an unregistered deterministic module, `temporal_guid_text`, over
original SQL declarations and exact already-stored values. It does not parse
source strings, choose result metadata, acquire session settings, or integrate
with the engine. Callers pass the formatting profile, language and output domain.

Supported source families are DATE, TIME, DATETIME2, DATETIMEOFFSET, DATETIME,
SMALLDATETIME and UNIQUEIDENTIFIER. Modern temporal scales remain explicit.
Values use day counts, 100-nanosecond ticks, validated core temporal values,
legacy 1/300-second ticks or minutes, and mixed-endian SQL GUID bytes. Range,
scale alignment and declaration/value identity are checked without repairing
invalid values. DATETIME's displayed milliseconds are derived from stored ticks;
source construction and storage rounding remain adapter responsibilities.

Default CAST, explicit styles 0 and 121, CONCAT_WS and TRANSLATE are separate
profiles replayed against their independently labelled reference observations.
English is supported for all seven families. French and German are supported
for the two legacy temporal families, including exact month names and spacing.
Uncaptured modern/GUID locale contexts, other styles, unknown source families
and unknown output encodings return explicit errors. CP1252 output validates
representability; Unicode output retains UTF16 units. Output is bounded to
36 code units. NULL validates its declaration and context and remains NULL;
CONCAT_WS skipping and composition happen outside this formatter.

The tests replay every applicable observation from all four retained #835 runs
in `reference/temporal-guid-format.json` (SHA256
`2912bd9f81d269d0c0d81eac2dcfa056eb2f63305c460ff2b00fe3516357c0a6`).
They derive values from native SQL storage bytes and check DATEPART fields, or
from actual emitted prepared TDS payloads. Lossy JavaScript Date carriers are
not inputs. The complete captured rows remain the expected results, with each
style, function role and encoding compared separately. Prepared descriptors
must remain equal to their captured prepare descriptors.

Across the four runs this covers 20,920 formatting observations, 1,444 native
SQL source observations and 176 native prepared bindings. All 300 source
construction failures remain separate, with complete captured diagnostics and
zero rows checked. SQL Server can emit descriptors before those failures; they
are not formatter successes. Other tests cover invalid declarations, profiles,
contexts, NULL contracts, range and scale violations, GUID byte order and
bounded output. These finite captures establish the retained cases, not every
possible stored value, language or style.

The initial private Linux proof compiles the actual source and test with Rust
1.95 against compatible cached dependencies, passes all four tests and strict
Clippy, and removes its own binaries. Full exact-head workspace, client and
audit verification, independent review and Codex review are still pending at
this checkpoint. Root integration must compose conversion allocation, encoding,
declaration metadata, session language and source construction diagnostics
without changing original source kinds or evaluating operands again.
