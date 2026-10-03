# Text conversion reference for CONCAT_WS and TRANSLATE

This is reference evidence, not a server implementation or compatibility completion claim. `scripts/capture-concat-text-conversion.mjs` retains 908 programs in two fresh databases in each of two independently owned SQL Server 2025 containers. All four complete observations match without dropping, sorting or repairing fields. The immutable container image is the repository's pinned `referenceImage` (digest `86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`).

The fixture SHA-256 is `b8fe1d62168a501ce82716fe864cdfa475390cfeead13caf59ff3f6498061dba`; each complete program sequence hashes to `05f8f45d5fd21aab61688514713c6b4d1b400cc58c3042dd1d789fdf08345af4`. The script pins the latter independently of agreement between copies.

Each family has its own native source descriptor and explicit `CONVERT(VARCHAR(MAX), source)` baseline, plus separate TRANSLATE and CONCAT_WS observations. Typed NULL repeats the declared types. CONCAT_WS tests separator, first and last source positions and Unicode promotion. Decimal precision/scale and temporal scales are explicit inputs. Three prepared programs retain prepare, four changed bindings including NULL, and unprepare; their execution descriptors are identical across bindings. No inferred parameter length is used.

## Binary conversion and MAX

For the captured BINARY(10), VARBINARY(10) and VARBINARY(8000) first arguments, TRANSLATE returns VARCHAR with wire byte capacity 8000; Unicode mappings produce NVARCHAR with wire byte capacity 8000 (4000 code units). VARBINARY(MAX) instead returns VARCHAR(MAX) or NVARCHAR(MAX), represented by descriptor length 65535. All these expressions have flags 33 and the retained default collation LCID 1033, flags 13, version 0, sort ID 52, CP1252. Typed NULL keeps those descriptors and returns NULL.

ANSI mapping interprets `0x4142` as `AB` and TRANSLATE('A','Z') produces `ZB`. Unicode mapping interprets the same binary bytes as UTF-16LE U+4241, rather than decoding CP1252 and promoting `AB`; the ASCII mapping does not match it. Fixed BINARY(10) preserves its eight padding zero bytes, hence five Unicode code units. The 9001-byte MAX probe consists of repeated byte 0x41: ANSI translation produces 9001 `Z` characters and DATALENGTH 9001, while Unicode conversion forms 4500 U+4141 units followed by padded U+0041; translation then replaces that final unit with U+005A (`Z`). The retained translated result has DATALENGTH 9002. These exact strings, including NULs, remain in the fixture. The prepared MAX binary program retains short/NULL/9001-byte/repeated-short bindings with unchanged descriptors.

This evidence requires a conversion domain supplied explicitly by an adapter. A rule that truncates MAX to 8000, or always decodes binary as CP1252 before Unicode promotion, contradicts the captures.

## Individual observed families

The table directly observes the CONCAT_WS source allocation with `CONCAT_WS(NULL, source, NULL)` and its reversed source position. Literal NULL companions contribute no character capacity, and all typed NULL repetitions retain the same widths while returning the empty string (flags 32). These are **CONCAT_WS-specific declarations**, not generic CAST conversion widths. No capacity is inferred by subtracting empty-string companions or measuring current text. Native source descriptors and explicit conversion baselines remain separate in each `source` record. Length 65535 denotes MAX; Unicode descriptor lengths are bytes.

| Typed source | Direct CONCAT_WS allocation bytes | TRANSLATE formatted value |
| --- | ---: | --- |
| `tinyint` | 4 | `"255"` |
| `smallint` | 6 | `"-32768"` |
| `int` | 12 | `"-2147483648"` |
| `bigint` | 24 | `"-9223372036854775808"` |
| `bit` | 1 | `"1"` |
| `money` | 40 | `"-123456.79"` |
| `smallmoney` | 40 | `"-123.45"` |
| `float` | 23 | `"1.23457e+020"` |
| `real` | 23 | `"1.23457"` |
| `date` | 40 | `"2024-01-02"` |
| `datetime` | 40 | `"Jan  2 2024  3:04AM"` |
| `smalldatetime` | 40 | `"Jan  2 2024  3:04AM"` |
| `guid` | 40 | `"6F9619FF-8B86-D011-B42D-00C04FC964FF"` |
| `binary2` | 2 | `"AB"` |
| `binary10` | 10 | `"AB\u0000\u0000\u0000\u0000\u0000\u0000\u0000\u0000"` |
| `varbinary2` | 2 | `"AB"` |
| `varbinary10` | 10 | `"AB"` |
| `varbinary8000` | 8000 | `"AB"` |
| `varbinarymax` | 65535 | `"AB"` |
| `varchar10` | 10 | `"a"` |
| `nvarchar10` | 20 | `"a"` |
| `char10` | 10 | `"a         "` |
| `nchar10` | 20 | `"a         "` |
| `varcharmax` | 65535 | `"a"` |
| `nvarcharmax` | 65535 | `"a"` |
| `xml` | error 257 | `"error 8116"` |
| `variant` | error 257 | `"error 8116"` |
| `text` | 65535 | `"error 8116"` |
| `ntext` | 65535 | `"error 8116"` |
| `image` | error 206 | `"error 8116"` |
| `decimal1_0` | 41 | `"-1"` |
| `decimal8_2` | 41 | `"-1.25"` |
| `decimal18_0` | 41 | `"-1"` |
| `decimal18_4` | 41 | `"-1.2500"` |
| `decimal38_0` | 41 | `"-1"` |
| `decimal38_18` | 41 | `"-1.250000000000000000"` |
| `decimal38_38` | 41 | `"0.12345000000000000000000000000000000000"` |
| `time0` | 40 | `"03:04:05"` |
| `datetime2_0` | 40 | `"2024-01-02 03:04:05"` |
| `datetimeoffset0` | 40 | `"2024-01-02 03:04:05 +01:30"` |
| `time2` | 40 | `"03:04:05.12"` |
| `datetime2_2` | 40 | `"2024-01-02 03:04:05.12"` |
| `datetimeoffset2` | 40 | `"2024-01-02 03:04:05.12 +01:30"` |
| `time3` | 40 | `"03:04:05.123"` |
| `datetime2_3` | 40 | `"2024-01-02 03:04:05.123"` |
| `datetimeoffset3` | 40 | `"2024-01-02 03:04:05.123 +01:30"` |
| `time7` | 40 | `"03:04:05.1234567"` |
| `datetime2_7` | 40 | `"2024-01-02 03:04:05.1234567"` |
| `datetimeoffset7` | 40 | `"2024-01-02 03:04:05.1234567 +01:30"` |

Bounded scalar inputs in these TRANSLATE probes return VARCHAR(8000), or NVARCHAR(4000) when promoted. MAX character/binary inputs remain MAX. Decimal scale is retained in formatted text; money is rounded to two fractional digits; FLOAT uses six significant digits and scientific notation for the chosen large value. DATETIME/SMALLDATETIME use the captured English month/12-hour format; DATE, TIME, DATETIME2 and DATETIMEOFFSET preserve the shown family/scale formatting. These examples do not establish uncaptured rounding, exponent boundaries, locale behavior, conversion widths or collation/codepage domains.

TRANSLATE rejects XML, SQL_VARIANT, TEXT, NTEXT and IMAGE with 8116 in the captured first-source position. CONCAT_WS accepts TEXT/NTEXT as MAX, rejects XML/SQL_VARIANT with 257 and IMAGE with 206; separator and last-position diagnostics are retained independently. An explicit conversion baseline is a separate expression and must not be substituted for the function's own implicit conversion, especially for binary and Unicode. No mapping of unsupported families is invented.

## Reproduction and validation

Run from an isolated reference directory with the repository's cached Node dependencies and Docker:

```sh
node scripts/capture-concat-text-conversion.mjs .tmp/reproduction.json
node scripts/capture-concat-text-conversion.mjs --check-fixture
node scripts/capture-concat-text-conversion.mjs --self-test
```

The default reproduction captures all four databases, compares complete observations to the retained fixture, and preserves raw output before validation. `--one-database` is diagnostic only; `--write-fixture` exclusively creates a new fixture and refuses an existing one. Output collisions, symlinks/hardlinks to the fixture, conflicting flags and unknown options fail before Docker. Fresh random loopback ports/container identities are owned and removed by the shared lifecycle helper. The existing native workspace is never synchronized or built.

The local observer reuses the approved recovered CONCAT_WS/TRANSLATE capture design: complete request-handler token order, ORDER, raw DONE status/command/64-bit count, and bounded fragmentation-safe RETURNVALUE name/status/metadata/payload. Batch and prepared result descriptors retain `userType` for every source, baseline and function column. Ordered COLMETADATA tokens also retain full descriptors and are reconciled field-for-field against public events; missing userType is rejected. The self-test checks every split and bytewise fragmentation, rejects truncation, and rejects eight coherent four-copy mutations covering rows, descriptors, token omission, DONE fields, handle payload, diagnostics and missing/changed userType. Required CI and review evidence belongs to the PR's exact head.

Future deterministic conversion adapters still need general conversion declarations outside these CONCAT_WS allocations, exhaustive numeric/temporal formatting boundaries, arbitrary codepages/collations and runtime integration. This fixture supplies the listed per-type observations and explicit unknowns; it does not prove those remaining rules.
