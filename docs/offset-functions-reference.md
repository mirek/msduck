# SWITCHOFFSET and TODATETIMEOFFSET reference observations

`reference/offset-functions.json` retains 134 SQL Server programs and two
prepared programs. `scripts/capture-offset-functions.mjs` captured each in two
fresh databases in each of two independent containers, then independently ran
the same four-database capture again against the retained fixture. All eight
observation sets matched exactly. The containers used the pinned SQL Server
2025 image digest
`86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`.
The fixture SHA-256 is
`01f05ca35c176f67ecb6f182bf4477871b8d9a0341d4b925f3db84ec23715269`.
The generator writes a separate capture artifact, checks an existing fixture,
and refuses to overwrite it with `--write-fixture`.

The fixture retains TDS descriptors, rows, exact error number/state/class/text,
information messages, DONE events and RPC return status. Tedious presents a
DATETIMEOFFSET wire value as a UTC JavaScript Date, so the capture also asks SQL
Server for UTC text (`CONVERT(..., 127)`) and `DATEPART(tzoffset, ...)`. Together
they preserve the original offset and fractional ticks; the `rendered` column
is **UTC**, even when the effective offset is nonzero. The SQL values were not
normalized to hide differences.

| Probe | Observed behavior |
| --- | --- |
| Scales 0–7 | Both functions return DATETIMEOFFSET at the input's declared scale, including empty results. Fractional ticks at scale 7 survive the switch or attachment. |
| SWITCHOFFSET | The UTC instant is retained while the reported offset changes. Source DATETIMEOFFSET columns and constant expressions agree in the captured cases. |
| TODATETIMEOFFSET | The input DATETIME2 local fields are retained and the UTC instant changes by the supplied offset. Source DATETIME2(3) columns produce DATETIMEOFFSET(3). |
| Signed integer minutes | `-840` through `840` are accepted; `±841` raise 9812. `TINYINT`, `SMALLINT`, `BIGINT` and `BIT` probes are retained. |
| Text offsets | Exact `±HH:MM` forms, including `±14:00`, are accepted. Short forms, surrounding spaces, numeric text, minute 60, hour 15 and empty text raise 9812. ANSI and Unicode text are represented. |
| Fractional numeric offsets | In these probes, DECIMAL/NUMERIC/FLOAT/REAL `±90.5` become `±90` minutes; MONEY `90.5` becomes `91` minutes. This is type-dependent coercion, not a universal rounding rule. |
| NULLs and error precedence | A NULL source suppresses an otherwise invalid offset and yields a typed NULL. A malformed source date with an invalid offset yields conversion error 241 before offset error 9812 in the captured scalar probes. |
| Range limits | Changing to an offset that would make local time leave the DATETIMEOFFSET range, or attaching one that makes UTC leave that range, raises 9813. Valid adjacent minimum/maximum cases are retained. |

Invalid offset errors are 9812, class 16, state 1 for SWITCHOFFSET and state 3
for TODATETIMEOFFSET. Range errors are 9813, class 16, state 0 and state 2
respectively. The scalar failure cases emitted typed result descriptors before
the diagnostic, then a DONE without a row. The prepared handle survived NULL
and 9812 executions and returned the original value on reuse; the failed
prepared `sp_execute` result had return status -6, while the `sp_executesql`
RPC capture retained its distinct return status. These completion distinctions
are in the fixture and should not be collapsed by an adapter or test.

The current runtime handles integer minutes and exact `±HH:MM` text for these
functions, but the first-party roadmap explicitly leaves broader numeric/text
coercion, exact error precedence and live SQL Server comparison open. This
reference task does not change the runtime or claim parity. A successor should
replay each typed row, descriptor, diagnostic and completion sequence against
an immutable server revision, then implement only the proven missing rules.
The fixture does not establish behavior for every numeric precision, locale,
binary/XML/user-defined offset type, volatile operand, or all possible source
date formats.
