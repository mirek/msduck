# Numeric formatting reference for CONCAT_WS and TRANSLATE

This fixture records SQL Server 2025 ground truth, not an msduck runtime pass. The generator retains 2,238 programs in two fresh databases in each of two independently owned containers. All four complete runs agree, and a second complete four-database reproduction agrees with the retained fixture. The pinned image is `mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`; the observed product version is `17.0.4065.4`, with server and database collation `SQL_Latin1_General_CP1_CI_AS`.

Fixture SHA-256: `313ce8e6cd3254493f17aed4bfef54a4bdeb990f887e7c8f7178d184a0b8ad1c`. Each complete `JSON.stringify(run)` independently hashes to `1f3e1608b5ae476a80b31d1969ef9e2d0d35632d4302b691c6662b81bdb811b9`. The script pins that digest without projecting, sorting, or repairing observations. It detects even identical corruption in all four copies; it is an integrity contract, not independent proof of every SQL semantic rule.

## Inputs and separate observations

The 114 typed scalar inputs cover BIT, TINYINT, SMALLINT, INT, BIGINT, MONEY, SMALLMONEY and six explicit DECIMAL precision/scale pairs: `(1,0)`, `(8,2)`, `(18,4)`, `(38,0)`, `(38,18)`, `(38,38)`. Integer endpoints, zero and signs are retained as original scalar text. Decimal input strings include signed zero, trailing fractional zeros, scale/precision extremes and exact 38-digit coefficients. Money probes include extrema and both signs of half-cent ties, adjacent ten-thousandths and carries at 99.9950 and 100.0050. There is one typed NULL per declaration.

Another 80 inputs use typed REAL/FLOAT RPC parameters. Each family retains positive and negative zero, minimum/maximum subnormal, minimum normal, maximum finite values, negative extremes, the adjacent IEEE values around 1.234565, 1.234575, 9.999995, 999999.5, 0.00009999995, 0.0001, 0.00001 and 1000000, plus other signs/magnitudes and typed NULL. `ieeeLittleEndian` is the original input bit pattern, not a conversion from returned text. Every non-NULL payload is checked against the actual tedious parameter generator before capture; TYPE_INFO, length prefix and payload bytes are retained separately. Native source requests retain SQL VARBINARY storage and verify that it matches the requested IEEE bits (the observed SQL float storage is big-endian).

Tedious 20.0.0 normally passes numeric values through `parseFloat`, which loses numeric negative zero. A process-local wrapper preserves only that admitted input in validation and parameter generation so the signed-zero probes actually send the negative-zero bytes. It changes no reference output. A reversible `{kind:"number",value:"-0"}` carrier retains observed negative zero because JSON numbers cannot preserve it. All other existing missing/binary/BIGINT/nonfinite carriers remain unchanged. Nonfinite REAL/FLOAT RPC inputs are not admitted or established by this fixture.

Each input has eleven separate requests: native source plus VARBINARY storage, explicit VARCHAR(MAX)/NVARCHAR(MAX) style-0 controls, ANSI/Unicode TRANSLATE, CONCAT_WS first/last/separator positions in both ANSI and Unicode domains, and CONCAT_WS with literal NULL separator/companion. Explicit conversion never substitutes for the function's implicit conversion. Seven additional out-of-range typed scalar inputs repeat all eleven contexts; their diagnostics and empty descriptors remain retained. Thus there are 1,333 batch programs and 880 ordinary RPC programs.

The remaining 25 prepared programs retain prepare, changed bindings, replay and unprepare. Twenty-two REAL/FLOAT programs bind positive zero, negative zero, minimum subnormal, maximum finite, NULL and repeated positive zero on each fixed declaration. Three programs rebind DECIMAL(18,4), MONEY and BIGINT. All 144 executions preserve complete prepare descriptors. Scalar RPC binding values are also retained with their actual TYPE_INFO/length/payload bytes; the Money/Decimal driver inputs are JS numbers and must not be confused with the separately captured exact scalar-text boundaries.

Native DECIMAL/BIGINT/MONEY observations keep their full typed descriptors and SQL VARBINARY storage. Tedious's decoded numeric values alone can lose decimal precision; the original scalar text, exact native storage and independently captured formatted strings remain available rather than treating a JS number as an exact 38-digit input.

## Observed formatting and diagnostics

The examples below are observations for the retained inputs and declarations. They do not establish uncaptured rounding modes or every finite input.

| Input | Observed ANSI TRANSLATE text |
| --- | --- |
| MONEY/SMALLMONEY -0.0049 | `-0.00` |
| MONEY/SMALLMONEY -0.0050 / +0.0050 | `-0.01` / `0.01` |
| MONEY/SMALLMONEY -1.2350 / +1.2350 | `-1.24` / `1.24` |
| MONEY/SMALLMONEY -99.9950 / +99.9950 | `-100.00` / `100.00` |
| MONEY maximum 922337203685477.5807 | `922337203685477.58` |
| REAL negative zero (LE `00000080`) | `-0` |
| FLOAT negative zero (LE `0000000000000080`) | `-0` |
| REAL minimum subnormal (LE `01000000`) | `1.4013e-045` |
| FLOAT minimum subnormal (LE `0100000000000000`) | `4.94066e-324` |
| REAL maximum finite | `3.40282e+038` |
| FLOAT maximum finite | `1.79769e+308` |
| REAL/FLOAT input nearest 999999.5 | `1e+006` |
| REAL/FLOAT input nearest 0.0001 | `0.0001` |

REAL and FLOAT carry different input bits near six-significant-digit boundaries. The nearest REAL to 1.234565 formats `1.23457`; the nearest FLOAT formats `1.23456`. Adjacent bit patterns on either side are retained. Around the lower fixed/scientific transition, REAL nearest 0.00009999995 remains `9.99999e-005` and its next higher bit pattern becomes `0.0001`; FLOAT at the nearest input is already `0.0001`, while its preceding bit pattern remains scientific. Scientific exponents have signs and at least three digits; trailing significand zeros disappear in these style-0 observations. Decimal strings retain the declared fractional scale, including 38-place fractions. Typed NULL yields NULL for TRANSLATE and an empty non-null string for the isolated CONCAT_WS form.

The direct CONCAT_WS form with NULL companions retains source allocation widths independently of current text: BIT 1, TINYINT 4, SMALLINT 6, INT 12, BIGINT 24, MONEY/SMALLMONEY 40, DECIMAL 41, REAL/FLOAT 23 bytes. Its flags are 32. TRANSLATE's bounded numeric first arguments retain VARCHAR(8000), or NVARCHAR(4000) with Unicode mappings (wire length 8000 bytes), flags 33. The explicit MAX conversion controls remain MAX (wire length 65535). All descriptors, including Unicode byte capacities and the widths of separator/first/last forms, remain raw in the fixture.

Out-of-range VARCHAR source casts produce one class-16 diagnostic and no row, after result metadata is emitted. TINYINT `256` yields 244/state 1; SMALLINT `-32769` yields 244/state 2; INT `2147483648` yields 248/state 1; BIGINT `9223372036854775808` and MONEY `922337203685477.5808` yield 8115/state 2; DECIMAL(1,0) `10` yields 8115/state 8; SMALLMONEY `214748.3648` yields 294/state 0. Exact messages, line numbers, server/procedure identities and DONE status/count fields are preserved for every context. These are source-conversion failures, not fabricated unsupported-function errors.

## Capture fidelity and reproduction

The local observer follows the approved text-conversion capture: ordered COLMETADATA with every decoded property (including userType/schema/udtInfo/tableName), row markers, ORDER, ERROR/INFO serverName/procName, raw DONE status/command/64-bit count, RETURNSTATUS and fragmentation-safe RETURNVALUE bytes/name/status/metadata/payload. Public metadata/row counts/messages/completions are reconciled against the ordered observer. Prepare handle bytes are checked against the decoded IntN4 handle, every RPC success status is zero, prepare/unprepare succeed and each execution's full descriptors match prepare. The explicit hostname `msduck-numeric-reference` is independent of random owned container names, loopback ports and databases; diagnostic identity is not normalized.

```sh
node --max-old-space-size=768 scripts/capture-concat-numeric-format.mjs .tmp/reproduction.json
node --max-old-space-size=768 scripts/capture-concat-numeric-format.mjs --check-fixture
node --max-old-space-size=768 scripts/capture-concat-numeric-format.mjs --self-test
```

The full reproduction retains raw output before validation and compares complete observations with the fixture. `--one-database` is diagnostic only. `--write-fixture` exclusively creates a new fixture and refuses existing fixtures before Docker; output writes also use exclusive creation. Existing outputs, symlink/hardlink aliases, conflicting flags and unknown flags are refused before containers start. Nine CLI negative probes confirm no Docker invocation, no changed output and unchanged fixture bytes. Every RETURNVALUE split and bytewise fragmentation is checked; truncation is rejected. Fifteen coherent four-copy corruptions cover rows, full descriptors/userType, omitted token/raw DONE fields, prepare bytes, input and outgoing wire provenance, RPC status, prepared width drift and diagnostic state/server identity.

Capture input is fixed and bounded below 2,500 programs per run. Each phase is validated at no more than 1,024 tokens/1 MiB serialized data; retained/captured envelopes are bounded at 128 MiB. RETURNVALUE buffering is capped at 1 MiB. A 30-minute watchdog is checked between requests/container operations, requests time out at 120 seconds and Docker commands at 60 seconds. Captures use a 768 MiB Node heap invocation and one owned container at a time with 4 GiB memory, two CPUs and 512 PIDs. These are bounds for this fixed scalar probe suite, not a general adversarial TDS streaming proof. Only owned lifecycle resources are removed. The private reference directory uses copied cached Node dependencies and approved helpers; no shared native workspace, source synchronization or build is required.

## Existing candidates and remaining scope

The core exact money formatter's coefficient-based half-away rounding agrees with the retained tie/neighbour/carry examples, including `-0.00`. Existing decimal display rules are candidates for preserving declared scale, not ground truth. The root `float_text` style-0 branch currently turns both zero signs into `"0"`; that differs from the captured negative-zero `"-0"`. This reference task changes no formatter and asserts no msduck runtime pass. No runtime comparison or native verification was run for these new reference files.

Future formatting integration must select by original source kind and declaration, preserve exact coefficient/IEEE input provenance, establish general numeric conversion/allocation rules, preserve descriptors for NULL/empty/prepared results, and carry exact errors/tokens through adapters. Exhaustive finite IEEE rounding, nonfinite RPC behavior, uncaptured DECIMAL scales, styles beyond 0, arbitrary collation/codepage/locales and surrounding session/wire/runtime integration remain unproved here. Core candidates and copied language notes must not replace the retained SQL Server observations.
