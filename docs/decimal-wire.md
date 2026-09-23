# DECIMAL result wire encoding

SQL Server advertises maximum result capacity 17 for DECIMAL, independently of
precision. Each non-null row uses sign plus the smallest number of whole 32-bit
magnitude groups: 5, 9, 13 or 17 bytes including sign. Zero uses a positive sign.
The logical precision/scale bound remains enforced before serialization; NULL
still has a zero payload length.

The codec now separates the result metadata maximum, the actual coefficient
payload length, and compact precision-based client RPC declarations. Result
encoding uses the magnitude helper after range validation. Existing RPC width
validation remains unchanged. A metadata change alone would miss the compact
result rows observed at precision 38.

The [MS-TDS decimal value specification](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tds/5e02042c-a741-4b5a-b91d-af5e236c5252)
defines sign and 4/8/12/16-byte integer groups and permits shorter values than
the precision-based maximum. Live SQL Server captures establish its advertised
result maximum and the unsigned magnitude thresholds used here.

Evidence:

- `sql-server-decimal-wire.json` and `decimal-wire-after.json`: eight precision
  boundaries, all event captures and complete TDS message payloads match.
- `sql-server-decimal-magnitude.json` and `decimal-magnitude-after.json`: twenty
  signed/zero/magnitude-boundary captures, all event captures and complete TDS
  message payloads match. Packet headers are retained, but are not compared;
  server-assigned session IDs differ.
- `sql-server-decimal-width.json` and `decimal-width-after.json`: eleven broader
  captures covering NULLs, empty results, variables and aggregates. All widths
  of actual DECIMAL results now match. NUMERIC retains a separate unresolved
  wire-type identity gap, and AVG over DECIMAL is still a FLOAT result locally.
  The raw mismatches remain; only two of these broader cases match completely.

Pure codec and native tests cover all magnitude boundaries, both signs, retained
precision/scale rejection and exact payload bytes. The focused Linux client tests
passed for decimal widths, money/decimal composition and BIT rejection. Formatting
and strict Clippy passed. Full Linux verification and the 296-case audit are
running. The earlier frozen test snapshot asserted the existing DecimalN spelling
for NUMERIC; that incorrect assertion was removed locally, and the new reference
mismatch explicitly records the outstanding type-identity work.
