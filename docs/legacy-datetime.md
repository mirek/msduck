# Legacy datetime conversion evidence

`reference/legacy-datetime.json` contains 38 RPC observations captured twice
identically from the pinned SQL Server container. Reproduce them with
`node scripts/capture-legacy-datetime.mjs`. The generator compares fresh results
with the retained fixture and records raw repetitions separately. Each case
retains rows, descriptors, diagnostics, DONE tokens and a subsequent
`@@ROWCOUNT`/`@@ERROR` readback. This is reference evidence, not an msduck pass.

The cases distinguish nullable DATETIME and SMALLDATETIME parameters, string
and DATETIME2(7) conversion sources, CAST and TRY_CAST, precision boundaries,
range limits and rounding across midnight. They do not establish support for
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
