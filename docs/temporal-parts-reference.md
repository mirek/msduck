# Temporal constructor SQL Server reference

`reference/temporal-parts.json` retains 221 ordered TDS observations from each of
two fresh databases in each of two independent containers. The image is pinned
to `mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`;
the captured server reports ProductVersion `17.0.4065.4` and database collation
`SQL_Latin1_General_CP1_CI_AS`. A separate invocation repeated the four fresh
databases and matched the fixture exactly. The raw fixture SHA-256 is
`942bd9c31bc98b96befdc1a5a28ac79ab834fb5b7ba32f860d3d158f72b5e482`. Each observation retains the SQL, typed column descriptors,
rows, errors, information, DONE events, request row count and RPC return status.
It does not normalize error states, metadata flags, or completion differences.

`scripts/capture-temporal-parts.mjs` starts and removes its own pinned container
with a random name and an ephemeral loopback port. It creates and removes two
fresh databases per container, keeps credentials out of the fixture and command
line, and compares both databases and containers before writing a fixture.
Run from an owner-approved checkout on a Docker-capable Node.js 24+ host with
dependencies installed:

```sh
node scripts/capture-temporal-parts.mjs
```

That command writes an ignored diagnostic capture and compares it with the
retained fixture. A first-time `--write-fixture` invocation writes only if the
fixture does not exist; a second invocation rejects the overwrite before
starting Docker. `--one-database` is a quick diagnostic mode and cannot write
the retained fixture. The full capture and independent recapture ran on
`linux.local` in an isolated scratch directory, so no shared build workspace or
reference container was modified.

## Observations

- All three constructors expose their requested `Time`, `DateTime2`, or
  `DateTimeOffset` scale from 0 through 7, including on an empty result. At
  scale 7, fraction `9999999` survives as seven fractional digits; fraction
  `10000000` fails. The lower scales use `10^scale` as the invalid boundary.
- Invalid fields report error **289** with constructor-specific states:
  `TIMEFROMPARTS` state **2**, `DATETIME2FROMPARTS` state **5**, and
  `DATETIMEOFFSETFROMPARTS` state **6**. This includes invalid Gregorian dates,
  invalid offsets, and UTC results outside years 1–9999. A NULL runtime field
  yields a typed NULL even when another runtime field is out of range. A bad
  varchar-to-int conversion still raises **245** in the presence of a NULL
  runtime field, so conversion and component validation are distinct steps.
- Integer constant precision expressions such as `1+2`, `8/2`, `7&3`,
  parentheses, and `CAST(3 AS INT)` are accepted. NULL, decimal, float, string,
  variable, parameter, divide-by-zero and overflowing expressions produce
  **10760** with the relevant type name. Literal scales `-1` and `8` produce
  **1002**, state 2. An invalid scale is reported before an invalid runtime
  component or runtime conversion. Wrong arity produces **174**, class 15.
- Numeric strings and decimal/float component expressions convert to integers;
  the captured positive decimal examples discard the fractional part. Invalid strings report
  **245**, and an oversized BIGINT component reports **8115**.
- Offset `+14:00` and `-14:00` are accepted at valid UTC instants; adding a
  minute at those extremes fails. Signed subhour zones `-00:30` and `+00:30`
  survive `DATENAME(tz, ...)`, while mixed-sign hours/minutes and out-of-range
  fields fail with 289. Both local and derived UTC bounds matter. Tedious
  exposes a `DateTimeOffset` value as a UTC JavaScript `Date`, so separate
  `DATENAME(tz, ...)` observations preserve the signed zone and style-127 text
  observations preserve exact seven-digit UTC rendering. The client-visible
  typed rows and the separately rendered values should be read together.
- Repeated `sp_executesql` RPC calls and a reused `sp_prepare` handle both
  return the same typed metadata through valid, NULL, invalid and recovered
  executions. Successful RPC executions have `doneInProc` then `doneProc` and
  return status 0. Invalid runtime fields retain the typed descriptor, report
  error 289 and end with `doneProc`; their return-status field is missing in
  the client capture. A precision parameter fails with 10760 through RPC.

The fixture is selected ground truth, not a compatibility pass for msduck.
Current implementation notes in [TIMEFROMPARTS](timefromparts.md),
[DATETIME2FROMPARTS](datetime2fromparts.md), and
[DATETIMEOFFSETFROMPARTS](datetimeoffsetfromparts.md) predate this live
comparison. The next runtime comparison should replay this fixture and preserve
raw differences in precision diagnostics, expression forms, coercion order,
NULL/error precedence, typed flags, offset representation, and RPC completion
tokens. This capture does not cover every SQL expression form, all implicit
conversion source types, SET-option interactions, or storage assignment.
