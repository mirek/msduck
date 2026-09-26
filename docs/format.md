# FORMAT reference contract

The retained fixture in reference/format.json contains 192 SQL Server
programs:
- 7 setup statements, including the environment probe.
- 165 ordinary batches.
- 17 sp_executesql RPC calls.
- 2 sp_prepare/sp_execute/sp_unprepare sequences, with 14 executions in total.
- A final connection reuse check.

Each program was captured in two fresh databases in each of two independent
containers. Both containers used the pinned SQL Server 2025 image digest
86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a:
ProductMajorVersion 17, server and database collation
SQL_Latin1_General_CP1_CI_AS, login language us_english and container `TZ=UTC`.
The four raw captures matched exactly. They retain:
- Result rows and TDS column descriptors.
- Error number, state, class, line and text.
- Information messages and DONE tokens.
- RPC return statuses.

scripts/capture-format.mjs regenerates an artifact and checks it against the
retained fixture. It refuses to overwrite an existing fixture before any
container starts.

Every rule below is exactly what the fixture shows. None is a claim about
untested inputs, and msduck does not implement FORMAT yet. Non-ASCII
characters in the results are given as code points where they matter: U+00A0
no-break space, U+2019 right single quotation mark, U+200F right-to-left mark
and U+00A4 generic currency sign.

## Capture conventions

- Statements are unique per RPC and prepared case. Each has a trailing
  `/*case name*/` comment, so no two plan-dependent sequences share cached text.
- The script replicates `capturePrepared` from the owner's prepared-capture
  helper (PR #312, docs/reference-captures.md), because that helper is not yet
  on main:
  - Preparation completes on the `prepared`/`error` events.
  - `request.error` is cleared before each phase.
  - An error object already reported by an earlier phase is never attributed
    again.
  - Only tokens raised while a phase is outstanding are recorded.
- Rows and messages are bounded per phase.
- Whole captures are compared only with the bounded `assertSameCapture`.
- The table constraints are named explicitly: `fmt_src_pk` and `fmt_bad_pk`.
- tedious derives a DATETIMEOFFSET parameter's offset from the client host's
  time zone. With the host in Europe/Warsaw, an RPC value captured as
  `+01:00`. The script therefore sets `process.env.TZ = 'UTC'`, and every
  retained DATETIMEOFFSET parameter carries `+00:00`.

## Result declarations

| Form | Captured descriptor |
| --- | --- |
| Every FORMAT result: literal, column, RPC and prepared, with any accepted input type or culture | NVarChar, length 8000 (NVARCHAR(4000)), flags 33, database collation (lcid 1033, sortId 52, CP1252) |
| Expressions over NOT NULL columns | Same: nullable. sys.dm_exec_describe_first_result_set reports nvarchar(4000), max_length 8000, is_nullable 1, collation SQL_Latin1_General_CP1_CI_AS |
| SQL_VARIANT_PROPERTY | BaseType nvarchar, MaxLength 8000, Collation SQL_Latin1_General_CP1_CI_AS |
| SELECT INTO | nvarchar, max_length 8000, nullable, database collation |
| sp_prepare response | Same descriptor with no rows and DONEINPROC count 0 |

FORMAT is nondeterministic:
- A PERSISTED computed column fails with error 4936 state 1.
- A non-persisted computed column is accepted.
- OBJECTPROPERTY IsDeterministic and COLUMNPROPERTY IsDeterministic are 0 for a
  schema-bound view column.

## Arguments and NULL

| Input | Captured behavior |
| --- | --- |
| Value types accepted | TINYINT, SMALLINT, INT, BIGINT, DECIMAL/NUMERIC, MONEY, SMALLMONEY, FLOAT, REAL, DATE, TIME, DATETIME, SMALLDATETIME, DATETIME2, DATETIMEOFFSET and integer/decimal/float literals |
| Value types rejected | BIT, VARCHAR, NVARCHAR, UNIQUEIDENTIFIER, VARBINARY, SQL_VARIANT, XML, and an untyped NULL. Error 8116 state 1 class 16, "Argument data type X is invalid for argument 1 of format function.", before any metadata, with a DONE without count. An NVARCHAR RPC parameter is rejected the same way. |
| Format argument | VARCHAR, NVARCHAR and NVARCHAR(MAX) are accepted. INT and untyped NULL give 8116 for argument 2. |
| Culture argument | INT gives 8116 for argument 3. |
| Two or four arguments | Only one and four arguments were captured. Both give error 189 class 15, "The format function requires 2 to 3 arguments." |
| Typed NULL value | NULL, including with the unknown culture 'xx-XX'. The culture is still validated: a typed NULL value with 'Klingon' or a typed NULL culture raises 9818. |
| Typed NULL format (literal, RPC, prepared or column) | Formats as if no format were given. An INT gives '1', and DECIMAL(10,2) 1234.5 gives '1234,50' with de-DE. A prepared DECIMAL(19,4) gives '1234.5000' because the scale is kept. A rejected culture still raises 9818. |
| Empty format '' | Same as the general format: 1234.5 gives '1234.5'. |
| NULL or empty culture | Error 9818 state 1 class 16, "The culture parameter 'NULL' provided in the function call is not supported." (the text contains `''` for the empty string). The error is raised at run time after the column metadata, and no row follows. |
| Invalid format string | NULL, with no error. For example 'Q', 'B', 'R' on integers, 'D'/'X' on non-integers, 'U' on DATETIMEOFFSET, and single-character custom date strings such as 'z', 'K' and 'H'. |

## Cultures

Accepted names in the fixture:
- The six requested cultures: en-US, de-DE, fr-FR, ja-JP and ar-SA, plus iv
  (invariant).
- Neutral names: de, fr, ja and ar.
- de-CH, en-IN, hi-IN, zh-Hans, sv-SE, sr-Latn-RS and zh-TW.
- en-US-x-test, en-US-POSIX and qps-ploc (pseudo-locale).
- Case-insensitive variants: de-de, DE-DE and EN-us.
- Underscore forms: de_DE formats as German, and en_US is accepted.
- Unknown but well-formed language tags: xx-XX, zz, tlh and abc. These format
  like the invariant culture. For example, xx-XX 'D' gives
  'Tuesday, 05 March 2024'.

Names rejected with error 9818:
- The empty string and NULL.
- x, abcdefgh, abcdefghi, Klingo, klingon and Klingon.
- en-USA, C and POSIX.
- A trailing or leading space: 'en-US ' and ' de-DE'.
- The LCID string '1031'.
- 'Invariant Language (Invariant Country)'.

The error is statement-level. In `SELECT 1; SELECT FORMAT(1,'N','Klingon');
SELECT 2` the first and third results are returned. The middle statement sends
its metadata, no row and a DONE with more set.

With no culture argument, the session language selects the culture:
- us_english behaves as en-US.
- SET LANGUAGE German gives '1.234,50' and 'Dienstag, 5. März 2024'.
- Japanese gives '¥1,235' and '2024年3月5日'.
- British gives '05/03/2024' and '£1,234.50'.

An explicit culture overrides the language: en-US under German gives
'1,234.50'. Each SET LANGUAGE emits information message 5703 in the new
language.

## Numeric culture matrix

The value is DECIMAL(19,4) -1234567.8951 unless noted. The default column
(us_english) always equals en-US.

| Format | iv | en-US | de-DE | fr-FR | ja-JP | ar-SA |
| --- | --- | --- | --- | --- | --- | --- |
| N | -1,234,567.90 | -1,234,567.90 | -1.234.567,90 | -1 234 567,90 (U+00A0) | -1,234,567.90 | -1,234,567.90 |
| N0 | -1,234,568 | -1,234,568 | -1.234.568 | -1 234 568 | -1,234,568 | -1,234,568 |
| C | (¤1,234,567.90) | ($1,234,567.90) | -1.234.567,90 € | -1 234 567,90 € | -¥1,234,568 | -1,234,567.90 ر.س.‏ (ends U+200F) |
| P | -123,456,789.51 % | -123,456,789.51% | -123.456.789,51 % | -123 456 789,51 % | -123,456,789.51% | -123,456,789.51 % |
| E | -1.234568E+006 | same | -1,234568E+006 | -1,234568E+006 | -1.234568E+006 | -1.234568E+006 |
| G, F2, 0.### | -1234567.8951, -1234567.90, -1234567.895 | same | decimal comma | decimal comma | same as en-US | same as en-US |

Other captured values:
- `#,##0.00` follows each culture's separators, like N.
- INT -1234567 'D8' gives '-01234567' in every culture.
- MONEY 1234567.8951 'C' gives '$1,234,567.90', or '¥1,234,568' with ja-JP.
- FLOAT 1234567.891 'N' gives '1,234,567.89'. 'G' and 'R' give
  '1234567.891', and 'E3' gives '1.235E+006'. The decimal separator changes for
  de-DE and fr-FR.
- de-CH gives '1’234.50' (U+2019), and hi-IN groups as '12,34,567.50'.

Rounding is away from zero:
- 1234.5 with 'N0' gives '1,235'.
- ja-JP 'C' uses zero decimals.

## Numeric standard specifiers (en-US)

| Specifier | INT 1234 | INT -1234 | DECIMAL -1234.5678 | FLOAT 0.1 |
| --- | --- | --- | --- | --- |
| C / C0 | $1,234.00 / $1,234 | ($1,234.00) / ($1,234) | ($1,234.57) / ($1,235) | $0.10 / $0 |
| D / D10 | 1234 / 0000001234 | -1234 / -0000001234 | NULL | NULL |
| E / e2 | 1.234000E+003 / 1.23e+003 | negated | -1.234568E+003 / -1.23e+003 | 1.000000E-001 / 1.00e-001 |
| F / F3 | 1234.00 / 1234.000 | negated | -1234.57 / -1234.568 | 0.10 / 0.100 |
| G / G5 | 1234 / 1234 | negated | -1234.5678 / -1234.6 | 0.1 / 0.1 |
| N / N2 | 1,234.00 | -1,234.00 | -1,234.57 | 0.10 |
| P / P1 | 123,400.00% / 123,400.0% | negated | -123,456.78% / -123,456.8% | 10.00% / 10.0% |
| R | NULL | NULL | NULL | 0.1 |
| X / x8 | 4D2 / 000004d2 | FFFFFB2E / fffffb2e | NULL | NULL |
| B, Q | NULL | NULL | NULL | NULL |
| N99 | 99 decimals | 99 decimals | 99 decimals | 99 decimals |
| N100 | N11234 | -N11234 | -N11235 | N100 |
| '' / ' ' | 1234 / ' ' | -1234 / '- ' | -1234.5678 / '- ' | 0.1 / ' ' |

N100 is read as a custom format: the literals `N` and `1`, then two digit
placeholders. This is the .NET Framework precision limit of 99, not the
.NET 5+ limit.

X uses the width of the SQL type: SMALLINT -32768 gives '8000', INT
-2147483648 gives '80000000', BIGINT MIN gives '8000000000000000', and TINYINT
255 gives 'FF'.

## Numeric custom formats (en-US)

| Format | 1234567.891 | -1234567.891 | 0 | FLOAT 0.000123 |
| --- | --- | --- | --- | --- |
| #,##0.00 | 1,234,567.89 | -1,234,567.89 | 0.00 | 0.00 |
| 0000000000 | 0001234568 | -0001234568 | 0000000000 | 0000000000 |
| #.## | 1234567.89 | -1234567.89 | '' | '' |
| #,##0, / 0,,.0 | 1,235 / 1.2 | -1,235 / -1.2 | 0 / 0.0 | 0 / 0.0 |
| 0.00;(0.00);zero | 1234567.89 | (1234567.89) | zero | zero |
| 0.0;neg | 1234567.9 | neg | 0.0 | 0.0 |
| 0.0e+00 / 0.00E0 | 1.2e+06 / 1.23E6 | -1.2e+06 / -1.23E6 | 0.0e+00 / 0.00E0 | 1.2e-04 / 1.23E-4 |
| 0.0% / 0.0‰ | 123456789.1% / 1234567891.0‰ | negated | 0.0% / 0.0‰ | 0.0% / 0.1‰ |
| '#'0'!' / \#0 / "n="0 | #0! / #1234568 | -#0! / -#1234568 | #0! / #0 | #0! / #0 |
| abc / # | abc / 1234568 | -abc / -1234568 | abc / '' | abc / '' |

The `'#'0'!'` and `"n="0` formats give '#0!' and 'n=0' for every non-negative
value, and '-#0!' and '-n=0' for negative ones. The captured value never
replaces the `0`.

## Value types (en-US; formats G, N2, 0.00, X, d)

- Integers format their full range, including BIGINT MIN/MAX.
- DECIMAL keeps its declared scale with G: '1.50000', '42.50' and '-0.50'.
  Precision beyond the .NET decimal range formats without error:
  - 79228162514264337593543950336 gives G
    '79228162514264337593543950336'.
  - DECIMAL(38,38) keeps all 38 digits with G.
  - N2 rounds to 2 decimals.
- MONEY G gives four decimals: '-922337203685477.5808'. SMALLMONEY gives
  '214748.3647'.
- FLOAT G uses 15 significant digits: '1.23456789012346E+20', '1.5E-07', and
  '0.100000001490116' for a REAL widened to FLOAT. N2 of 1.2345678901234567E+20
  gives '123,456,789,012,346,000,000.00'.
- REAL 0.1 gives '0.1'. FLOAT -0.0 gives '0'.
- Date types use the date/time rules. Numeric-looking formats become custom
  date formats: 'N2' gives the literal 'N2', '0.00' gives '0.00', and 'X' gives
  NULL.
- TIME uses TimeSpan rules: 'G' gives '0:23:59:59.9999999', and N2, 0.00, X and
  d give NULL.

## Date and time standard specifiers (en-US)

For DATETIME2(7) '2024-03-05 14:07:09.1234567':

| Specifier | Result |
| --- | --- |
| d | 3/5/2024 |
| D | Tuesday, March 5, 2024 |
| f | Tuesday, March 5, 2024 2:07 PM |
| F | Tuesday, March 5, 2024 2:07:09 PM |
| g | 3/5/2024 2:07 PM |
| G | 3/5/2024 2:07:09 PM |
| M | March 5 |
| O | 2024-03-05T14:07:09.1234567 |
| R | Tue, 05 Mar 2024 14:07:09 GMT |
| s | 2024-03-05T14:07:09 |
| t | 2:07 PM |
| T | 2:07:09 PM |
| u | 2024-03-05 14:07:09Z |
| U | Tuesday, March 5, 2024 2:07:09 PM |
| Y | March 2024 |

Differences for the other date types:
- DATETIME '...09.997' gives O '2024-03-05T14:07:09.9970000'.
- DATE gives midnight: O '2024-03-05T00:00:00.0000000', G '3/5/2024 12:00:00 AM'.
- SMALLDATETIME '14:07:29' rounds to 14:07:00.
- DATE MIN and DATETIME2 MAX give '1/1/0001 12:00:00 AM' and
  '12/31/9999 11:59:59 PM'.

For DATETIMEOFFSET '2024-03-05 14:07:09.1234567 -05:30':
- O keeps the offset: '2024-03-05T14:07:09.1234567-05:30'.
- R and u convert to UTC: 'Tue, 05 Mar 2024 19:37:09 GMT' and
  '2024-03-05 19:37:09Z'.
- s keeps the local time.
- U gives NULL in every culture.
- The other specifiers format the local time.
- O, R, u, zzz and K results are the same in every culture.

## Date culture matrix

| Format (DATETIME2) | iv | de-DE | fr-FR | ja-JP | ar-SA |
| --- | --- | --- | --- | --- | --- |
| d | 03/05/2024 | 05.03.2024 | 05/03/2024 | 2024/03/05 | 24/08/45 |
| D | Tuesday, 05 March 2024 | Dienstag, 5. März 2024 | mardi 5 mars 2024 | 2024年3月5日 | 24/شعبان/1445 |
| G | 03/05/2024 14:07:09 | 05.03.2024 14:07:09 | 05/03/2024 14:07:09 | 2024/03/05 14:07:09 | 24/08/45 02:07:09 م |
| M / Y | March 05 / 2024 March | 5. März / März 2024 | 5 mars / mars 2024 | 3月5日 / 2024年3月 | 24 شعبان / شعبان, 1445 |
| t | 14:07 | 14:07 | 14:07 | 14:07 | 02:07 م |
| MMM tt | Mar PM | 'Mrz ' | 'mars ' | 3 午後 | شعبان م |
| dddd, MMMM d | Tuesday, March 5 | Dienstag, März 5 | mardi, mars 5 | 火曜日, 3月 5 | الثلاثاء, شعبان 24 |
| yyyy-MM-dd | 2024-03-05 | 2024-03-05 | 2024-03-05 | 2024-03-05 | 1445-08-24 |

ar-SA uses the Um Al-Qura (Hijri) calendar, including for custom yyyy/MM/dd.
de-DE abbreviates March as 'Mrz', and de-DE and fr-FR have an empty AM/PM
designator. DATE gives the same d and D results as DATETIME2.

## Date and time custom formats (en-US)

- 'yyyy-MM-dd HH:mm:ss.fffffff' gives the full 7 digits. DATETIME gives
  '.9970000'.
- 'ss.FFFFFFF' trims trailing zeros: DATETIME gives '09.997'.
- 'dddd, MMMM d, yyyy', 'ddd MMM', 'h:mm:ss tt', 'yy', "'at' HH'h'",
  'HH\:mm', 'yyyy/MM/dd HH:mm' and 'mm' behave as in .NET.
- '%d' gives '5', and the one-character 'd' is the short date '3/5/2024'.
- 'gg yyyy' gives 'A.D. 2024'.
- 'zzz' gives '-05:30' for DATETIMEOFFSET and '+00:00' for DATETIME2 and
  DATETIME. The latter follows the server's local zone, so the value is tied to
  the container's `TZ=UTC`.
- One-character custom strings that are not standard specifiers give NULL:
  'z', 'K' and 'H'.

TIME(7) '14:07:09.1234567' uses TimeSpan rules:
- 'c' and 't' give '14:07:09.1234567'.
- 'g' gives '14:07:09.1234567', with a decimal comma for de-DE and fr-FR.
- 'G' gives '0:14:07:09.1234567'.
- 'hh\:mm\:ss' gives '14:07:09', "hh':'mm" gives '14:07', and 'fffffff' gives
  '1234567'.
- An unescaped colon ('hh:mm'), 'HH\:mm' and 'D' give NULL.

TIME(0) midnight gives 'c' '00:00:00', 'g' '0:00:00' and 'G'
'0:00:00:00.0000000'.

## Result length

- A 4000-character format of zeros gives a 4000-character result.
- A format given as NVARCHAR(MAX) longer than 4000 characters fails with error
  8152 state 10 ("String or binary data would be truncated.") at run time,
  after the metadata.
- An expanding format of 800 × 'dddd ' (4000 characters) would produce 6400
  characters. The result is silently truncated to 4000 characters: DATALENGTH
  8000, LEN 3999 because of a trailing space. The NVARCHAR(MAX) form of the
  same format behaves the same.
- The 'result truncated format concatenation' case shows that a non-MAX `+`
  concatenation truncates the format to 4000 characters before FORMAT sees it.

## Rows and protocol shape

Culture validation is per row and during execution:
- With rows (1 en-US N0), (2 de-DE N2), (3 NULL value), (4 NULL format ja-JP)
  and (5 NULL culture), rows 1–4 are streamed: '1,235', '1.234,50', NULL and
  '1234.50'. Then error 9818 ends the statement with a DONE without a count.
- A DATETIME2 column with format 'N0' gives the literal 'N0'. With a NULL format
  and ja-JP it gives '2024/03/05 14:07:09'.
- The unknown culture 'xx-XX' in a row gives invariant-style '2.00' and does
  not stop the scan.

sp_executesql results:
- A successful call gives DONEINPROC with count 1 and DONEPROC with return
  status 0.
- A 9818 failure sends the metadata, no row and DONEINPROC without count. The
  DONEPROC carries return status 9818.
- An 8116 compile failure (NVARCHAR value) sends no metadata and only a
  DONEPROC with return status 8116.

Prepared sequences:
- sp_prepare returns the descriptor and DONEINPROC(0). Its return status was
  8116 for the first sequence and 0 for the second: the status of the previous
  RPC call on the session. That preceding call was the failed
  `rpc nvarchar value` and a successful sp_unprepare respectively.
- A failing execution (9818 for a NULL culture or 'Klingon') sends the
  metadata, no row and DONEINPROC without count, with return status -6.
- The next execution on the same handle succeeds with no error. No stale error
  is recorded.
- sp_unprepare returns DONEPROC with status 0.

The RPC values for FLOAT 0.1 'R' ar-SA, MONEY 'C' ja-JP, BIGINT MAX 'N0' fr-FR,
DATE 'D' ja-JP, DATETIME2 'D' ar-SA and TIME 'hh\:mm\:ss\.fff' match the
literal forms. DATETIMEOFFSET 'O' gives '2024-03-05T14:07:09.1230000+00:00'.

## Not captured

- Other cultures, and the full 900+ CultureInfo list. Culture data may change
  with server builds or OS/ICU data; only this image was observed.
- The acceptance grammar for culture names is not isolated. The fixture shows
  examples of accepted and rejected names only.
- Other session languages beyond German, Japanese and British, and SET
  DATEFORMAT/DATEFIRST interaction.
- Custom numeric edge cases: more than three sections, escaped section
  separators, very long digit runs, rounding ties at every precision, and
  binary floating-point ties.
- Negative-pattern variations from culture NumberFormat data, such as
  parentheses versus sign for C in other cultures.
- Custom date specifiers beyond those listed: fractional F/f widths 1–6,
  'hh'/'h' at midnight and noon, era names for other calendars, 'MMMM' genitive
  forms, and other non-Gregorian calendars (ja-JP-u-ca-japanese, th-TH).
- DATETIME2 and TIME with other scales in custom 'f' formats, and DATETIME
  rounding interaction beyond one value.
- Server `TZ` other than UTC, which affects 'zzz' on types without an offset.
- Output types other than the default: FORMAT inside CAST/CONVERT, string
  concatenation typing, and collation precedence. Only COLLATE on the result
  was captured.
- Parallel plans, very large row counts, and FORMAT in WHERE with index use.
- CLR availability or disabled-CLR errors, and memory or AppDomain failures.
- Non-us_english login defaults.

## Proposed successors

1. **format-core-v1 (msduck-core).** A deterministic `format` rule:
   - Takes an explicit SQL value family (integer width, decimal
     precision/scale, money, float/real, date/time family with scale and
     offset), a format string and an explicit culture-data snapshot.
   - Returns `Result<Option<String>, FormatError>`.
   - Implements the .NET Framework numeric and DateTime/TimeSpan
     standard/custom format engines exactly as captured here, including the
     NULL-on-invalid-format rule, N99 versus N100, width-based X, and
     15/7-digit float G.
   - Covers the Gregorian and Um Al-Qura calendars for ar-SA.
   - Truncates the output to 4000 UTF-16 code units.
   - Uses culture data from a checked-in table limited to the fixture's
     cultures: iv, en-US, de-DE, fr-FR, ja-JP, ar-SA and their neutral forms,
     plus the languages' default cultures.
   - Keeps a culture-name validator that reproduces the accepted and rejected
     names above.
   - Is tested only against reference/format.json values.
2. **format-metadata-v1 (msduck-sql).** Result typing and binding:
   - NVARCHAR(4000), nullable, database collation.
   - Argument-type rejection 8116 for arguments 1–3 before metadata, and arity
     error 189.
   - Nondeterminism for computed-column and view properties (4936).
3. **format-root-v1 (root).** DuckDB lowering through a scalar function that
   calls the core rule:
   - Passes the session language's default culture explicitly.
   - Raises 9818 at run time per row, after metadata, and 8152 for an
     over-length format.
   - Preserves RPC and prepared return statuses (9818 and -6), including
     sp_prepare's previous-status value.
   - Adds a tedious comparison against this fixture.
4. **format-culture-reference-v2.** Further captures for the gaps above,
   especially the culture-name grammar, additional calendars and non-UTC
   server time zones, before the core table is widened.
