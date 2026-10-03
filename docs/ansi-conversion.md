# Native CP1251 and CP1252 projections

`msduck_core::ansi_conversion::project` consumes an explicit source declaration,
an optional native `AnsiView`, a `ProjectionTarget` and `ProjectionLimits`.
The admitted sources are CP1251 and CP1252. Targets are native CP1251, CP1252, UTF8 and SQL
UTF16 units. Opaque numeric tags do not alias named encodings. Unsupported plans
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

General Unicode-to-codepage best-fit conversion, arbitrary encodings, malformed
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
