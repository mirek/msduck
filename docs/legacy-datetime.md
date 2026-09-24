# Legacy datetime conversion evidence

`reference/legacy-datetime.json` contains 60 RPC observations captured twice
identically from the pinned SQL Server container. Reproduce them with
`node scripts/capture-legacy-datetime.mjs`. The generator compares fresh results
with the retained fixture and records raw repetitions separately. Each case
retains rows, descriptors, diagnostics, DONE tokens and a subsequent
`@@ROWCOUNT`/`@@ERROR` readback. This is reference evidence, not an msduck pass.

The cases distinguish nullable DATETIME and SMALLDATETIME parameters, string
and DATETIME2(7) conversion sources, CAST and TRY_CAST, precision boundaries,
range limits, rounding across midnight, expression consumers and single-row
versus mixed-source multirow assignments. They do not establish support for
all conversion styles, date formats, language settings or implicit assignments.

Observed rules include:

- Nullable legacy parameters use DateTimeN with byte lengths 8 and 4,
  respectively, for both NULL and non-NULL values.
- Seven-digit ISO string fractions are rejected: DATETIME returns 241/state1/
  class16; SMALLDATETIME returns 295/state3/class16. TRY_CAST returns NULL.
  The corresponding DATETIME2(7) values are valid conversion inputs.
- SMALLDATETIME rounding depends on source type. String `12:00:29.999` becomes
  `12:01:00`, but the same value supplied through DATETIME2(7) becomes
  `12:00:00`. Both convert `12:00:30.000` to `12:01:00`.
- DATETIME rounds DATETIME2 `29.9983333` down and `29.9983334` up at the
  1/300-second boundary. Tedious exposes the resulting fractions as .997 and
  .000; the raw descriptors and tokens remain in the fixture.
- DATETIME string `1752-12-31T23:59:59.999` is rejected with 242/state3/class16
  despite rounding potentially reaching the minimum day. Source validation
  cannot be replaced by range-checking a rounded wire payload.
- SMALLDATETIME accepts `1899-12-31T23:59:59.999` after rollover to 1900, but
  rejects `2079-06-06T23:59:29.999` when rounding exceeds its upper bound.
- DATETIME2 source `1752-12-31T23:59:59.9999999` converts to DATETIME at
  `1753-01-01T00:00:00`. The captured maximum DATETIME2 value converts to
  `9999-12-31T23:59:59.997`; it does not produce the string-source range error.
- DATEPART, conversion back to DATETIME2, and comparison with the original
  DATETIME2 value observe the rounded legacy value before wire encoding.
- Separate SMALLDATETIME inserts of string `12:00:29.999` and its DATETIME2
  counterpart store `12:01:00` and `12:00:00`, respectively. Combining them in
  one multirow VALUES expression stores `12:00:00` for both: common-source
  type conversion precedes assignment to the destination. DATETIME stores
  `12:00:30` for both source forms and both insert arrangements.
- NVARCHAR invalid and seven-digit fractional strings retain the captured
  241/295 diagnostics. Out-of-range DATETIME2-to-SMALLDATETIME conversion
  reports 242/state3/class16 and names `datetime2` as the source type.

The inspected mirek/mssqlite revision
`7f71f2081602f8e3051998f5c11f058e65fe24ec` supplies useful TDS representations in
`packages/tds/src/date-time.ts` and metadata mappings in
`packages/engine/src/metadata.ts`. Its `packages/transpile/src/type.ts` maps
legacy types to the backend datetime representation; it does not provide a
strict conversion parser covering these captured distinctions. No source code
is copied by this reference task.

At msduck checkpoint `15e1c3f`, legacy parameter descriptors and wire payloads
have focused coverage, but backend TIMESTAMP casts still accept over-precise
strings. SQL conversion must preserve source type, validate before backend
coercion, round the stored/computed value, and deliver the captured error at the
correct statement phase. Rounding only while encoding a result is insufficient
for comparisons, arithmetic, assignments and persisted values. A successor
implementation should consume explicit typed inputs in deterministic rules,
keep native vector and session effects in root adapters, and preserve every
raw difference until it is resolved.
