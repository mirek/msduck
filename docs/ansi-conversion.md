# Native codepage and valid UTF8 projections

`msduck_core::ansi_conversion::project` consumes an explicit source declaration,
an optional native `AnsiView`, a `ProjectionTarget` and `ProjectionLimits`.
CP1251 and CP1252 sources admit native CP1251, CP1252, UTF8 and SQL UTF16 targets.
Valid UTF8 scalar sources admit native CP1251, CP1252, UTF8 identity and SQL
UTF16 targets under the captured profiles described below.
Opaque numeric tags do not alias named encodings. Unsupported plans
fail before NULL handling; a non-NULL carrier must match its declaration.
NULL stays `None` and empty input produces an empty non-NULL projected value.

The two 256-cell tables come directly from all four runs of
`reference/bulk-character-conversion.json`, SHA256
`f55527a2b0969d7104510d9b76a3303c82b28eac05d4ad11bc2acaf472caab27`.
CP1251 identity preserves every original byte. CP1252 projection uses the
observed native bytes, including replacements and the observed best-fit cells.
SQL UTF16 uses the observed units. UTF8 encodes those SQL characters, with
direct mixed and fragmented MAX controls from the retained captures.
Byte `98` becomes SQL unit `0098`, native UTF8 `c298`, and CP1252 `3f`;
the client's replacement-character display is not conversion ground truth.
The earlier four-run `bulk-character-encoding.json` supplies additional mixed
and MAX controls for same-encoding, CP1252 and Unicode targets.

The CP1252 tables come from every source byte in all four unchanged runs of
`reference/bulk-character-cp1252.json`, SHA256
`d9d8baa3ce4530f1af077feda8c80c9394b110557748b1fb84df0db9201189c2`.
Same-CP1252 output preserves bytes; CP1251 output is the observed native
best-fit/replacement table. Each byte yields one captured BMP SQL unit.
Every direct native UTF8 cell matches standard encoding of those observed units,
independently of client display. Undefined bytes81/8D/8F/90/9D remain those native
CP1252 bytes and SQL units0081/008D/008F/0090/009D, become UTF8c281/c28d/c28f/c290/c29d,
and map to CP1251question marks. The client displays U+FFFD for the original
CP1252 cells. Tests compare every target in all48 retained observations,
including mixed and fragmented values, NULL/empty and checked resource limits.

Input limits count source bytes. Output limits count native bytes or twice the
UTF16 unit count. Checked size preflight precedes allocation and copying;
`try_reserve_exact` failures return a typed error. The source stays immutable.
These are active payload bounds, not an allocator-capacity guarantee. Debug
output retains only a bounded prefix. No database, transport, parser, clock,
environment or process-global state participates in conversion.

The API produces complete projections. It does not apply CHAR padding,
VARCHAR capacity rules, truncation, SQL errors, transaction atomicity or
collation admission. For example, CP1251 copyright needs two UTF8 bytes here;
an output resource limit of one byte fails even though the separately captured
BulkLoad into UTF8 VARCHAR(1) succeeds with an empty value. That family-specific
SQL behavior belongs to the capacity layer.

Unicode-source codepage conversion, arbitrary encodings, malformed
UTF8 source semantics and Unicode-source isolated surrogates are not admitted.
The [native storage prototype](ansi-carrier.md) is complete; Value binding,
BulkLoad/catalog/output integration remain pending under the
[adapter plan](bulk-character-adapter-plan.md). Exporting this deterministic
kernel does not enable additional runtime collation profiles by itself.

The Rust reference tests extract complete selected observations from the
original fixture without modifying it. This avoids asking `serde_json::Value`
to represent unrelated diagnostics containing lone UTF16 surrogates. Expected
conversion values come from exact native-byte/SQL-unit fields, never a lossy
client decoder; diagnostic differences remain in the original reference.

## Valid UTF8 scalar domain

For an explicitly declared UTF8 source, strict scalar validation follows source
identity and the active input-byte limit. Native UTF8 output copies the original
valid bytes; SQL UTF16 output encodes scalar values into one/two units. Checked
unit counting and multiplication establish the final output-byte budget before
fallible allocation. The borrowed source and UTF16 iterator introduce no expanded
temporary. An empty value remains non-NULL and NULL performs no allocation.

Malformed/truncated input returns `ProjectionError::InvalidUtf8` with the original
byte offset (`valid_up_to`) and an optional error length (`None` for incomplete
input). This is an explicit boundary of the valid-scalar API, not SQL error 7339,
4896, 9833 or a SQL Server malformed-decoding algorithm. Native SQL UTF8 can retain
malformed bytes while SQL UTF16 and client display differ; those original fields
remain in the references. The carrier can still preserve raw bytes, but this
projection API never repairs them or produces U+FFFD to hide a difference.

Public tests replay all applicable variable-storage observations from these
unchanged owner captures:

| Reference | SHA256 |
| --- | --- |
| Conversion895 | `f55527a2b0969d7104510d9b76a3303c82b28eac05d4ad11bc2acaf472caab27` |
| UTF8 boundary913 | `fa1e3ae36794cfc4b197d2ff122c5ab5effb99f3b449d0ad2eafea354c18be6a` |
| UTF8 bounded target919 | `6c951a06d19c60c2a71e4656b570976baf726982cffcdef207d8e96bb5b92455` |

Selection requires matching declared/wire UTF8, variable source/target families
and native UTF8/Unicode targets. It uses declaration facts before comparing
candidate output. Fixed CHAR/NCHAR storage observations retain their original
padding/capacity results outside this complete-projection contract. Failed
declaration/admission/decoding loads have no stored-value oracle and are counted
explicitly rather than turned into successful rows.

All four runs of every selected case are retained: 716 observations include 236
failed loads; 920 valid rows provide 1396 native-byte/SQL-unit comparisons. Valid
NULL/ASCII controls inside mixed-malformed successful cases still participate;
184 malformed rows in those cases remain outside scalar projection. A separate
test checks every retained malformed input, including unsuccessful loads, for
explicit byte errors. Source/output type and the observed UTF8 profile (LCID 1033,
flags 96, version 2, sort 0) are checked; none enables an operational collation gate.
Every successful value checks exact and one-under active byte limits and source
immutability. Supplementary and scalar-boundary controls use captured SQL units.

The capacity layer handles captured UTF8 source families separately. Arbitrary
codepages, isolated UTF16 source surrogates and complete root storage/BulkLoad/
output adoption remain separate work. The valid domain is a
Unicode scalar contract supported by finite SQL observations, not exhaustive
proof of SQL Server behavior across contexts or collations.

## Complete valid UTF8 to CP1251/CP1252

Complete projections now share the measured unit maps used by the capacity
layer. The CP1251 map comes from all 63,488 valid BMP scalars in each of four
original task942 runs; CP1252 agrees with the existing full captured BMP map.
The existing capacity test compares this API against every one of those eight
original native outputs. No capacity truncation, source window or padding is
applied here. The retained acquisition and independent reproduction hashes are
in [ansi-capacity.md](ansi-capacity.md).

A separate test preserves all 72 applicable observations and 360 rows from
`reference/bulk-character-collation-precedence.json`. Explicit source COLLATE
and the captured database default select the interpretation independently of
the wire collation. UTF8 `c3a9` yields native CP1251 `65` and CP1252 `e9`; the
retained supplementary duck scalar yields `3f3f` in both. Conversion proceeds
by UTF16 units using the captured non-SC codepage profiles. Supplementary
replacement follows those retained controls; no SC codepage profile is admitted.

Strict validation and source/input checks precede output counting. Output uses
one byte per UTF16 unit, including both units of a supplementary scalar. Checked
counting and output limits precede fallible allocation; no expanded Unicode
temporary is allocated. NULL and empty remain distinct, and native output
retains its explicit encoding. This does not enable root Value, catalog,
storage, assignment, BulkLoad or result-writer integration.
