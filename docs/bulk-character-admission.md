# BulkLoad character admission

Task #947 separates character metadata admission, declared source-row length,
and target conversion/storage capacity. The immutable source evidence is
`reference/bulk-character-metadata.json`: 160 cases in each of four fresh SQL
Server databases. Its collector and acquisition evidence are documented in
`docs/bulk-character-metadata-reference.md`.

The deterministic `msduck_core::bulk_character_admission` module receives a
validated source declaration and explicit decoded wire family, byte length or
PLP framing, nullability and target MAX facts. It has no parser, TDS, database,
session or environmental dependency. Unknown wire shapes remain distinguishable
from measured diagnostics. Bounded wire widths need not equal declared widths.
CHAR and VARCHAR are distinct source/wire families, as are NCHAR and NVARCHAR.
Either a source MAX declaration or a target MAX column requires PLP framing.
Those metadata checks apply before NULL rows, independently of payload contents.

An admitted bounded declaration limits original source bytes, before conversion:
one byte per ANSI declaration unit and two bytes per Unicode unit. Even trailing
spaces count. Overflow is SQL4815/state1/severity17; family/nullability and MAX
framing mismatches are SQL4816/state1 and state2, respectively. Target conversion
and capacity remain separate operations; this module does not classify SQL2628.

The original-packet Session regression established 14 predecessor mismatches
among 24 CP1252/Unicode rejected cases: the old server successfully stored rows
that SQL Server rejected. Full predecessor output is retained privately in
`.tmp/bulk-character-admission-predecessor-test.log`. The pure replay covers all
640 observations without modifying their original inputs or expected outcomes.
After the runtime changes, all 24 supported Session failure cases pass on Linux,
including original diagnostic tuples, failure DONE, zero counters, empty target
and subsequent connection recovery. This focused result is retained in
`.tmp/bulk-character-admission-first-test.log`; it is not full revision evidence.

This is work in progress. CP1251 and UTF8 column declarations remain rejected by
the endpoint before BulkLoad; the Session regression deliberately retains the
original collations and explicitly limits its current scope to CP1252. Pure
admission agreement does not prove those endpoint domains. Fixed-source padding,
all successful readbacks, older capacity controls, late/staged failure and caller
transaction coverage, complete revision verification and reviews remain to be
completed before merge. The existing generic decoder4804 paths and legacy wire
types have not been reclassified by this change.
