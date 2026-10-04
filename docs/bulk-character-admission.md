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

Successful original Session loads exposed two additional predecessor errors:
CHAR(8) and NCHAR(8) sources with narrower wire values lost source padding in
variable targets. Padding now follows source admission, preserves isolated
Unicode units and NULLs, and precedes target storage conversion. All 37 supported
success controls match original native bytes and exact bulk completion bytes.
The pure replay additionally covers all 384 older capacity observations without
parsing or rewriting isolated surrogates in unrelated target diagnostics.

Companion task #949 preserves modern-character bounded wire overflow as typed
decoder facts before reading/allocating the oversized payload. The codec does
not classify SQL diagnostics. The root checks metadata first and emits4815 only
when the original incoming byte count also exceeds the declared source limit.
Wire-only overflow remains the existing4804 path. Numeric/binary, odd-Unicode,
PLP and framing error categories retain their existing behavior. Legacy wire
families remain outside the new measured rules.

Converted payload sizes include source padding, with checked arithmetic. Rows
are staged as they are converted, so a small wire chunk cannot first expand a
large batch of fixed sources before staging. A staged late-failure regression preserves the
caller's prior row and transaction, drops the load's staging table, leaves no
bulk rows and permits a later insert/commit. The initial original-row stress
shape took90seconds; a wide-row boundary shape exercises the same staging path
in the focused Session suite's roughly12second run. These are preliminary
working-tree measurements, not frozen final-head verification.

This is work in progress. CP1251 and UTF8 column declarations remain rejected by
the endpoint before BulkLoad; the Session regression deliberately retains the
original collations and explicitly limits its current scope to CP1252. Pure
admission agreement does not prove those endpoint domains. SQL Server's container
server name and msduck's server identity also differ; diagnostic tests explicitly
compare number/state/severity/message/procedure/line, not full identity-equal
error tokens. Complete frozen-revision verification, audit comparison and reviews
remain required before merge. Native ANSI encoding/catalog/wire gates and full
endpoint compatibility remain separate work.
