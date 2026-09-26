# PARSE and TRY_PARSE reference contract

The retained fixture in reference/parse-try-parse.json contains 344 SQL
Server programs: 4 setup statements, 324 ordinary batches, 12 sp_executesql
RPC calls, 3 sp_prepare/sp_execute/sp_unprepare sequences (18 executions in
total) and a final connection reuse check. Each program was captured in two
fresh databases in each of two independent containers. Both containers used
the pinned SQL Server 2025 image digest
86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a
(ProductMajorVersion 17, server and database collation
SQL_Latin1_General_CP1_CI_AS, language us_english, DATEFIRST 7, container
TZ=UTC). The four raw captures matched exactly. They retain result rows, TDS
column descriptors, error number/state/class/line/text, information messages,
DONE tokens and return statuses.

scripts/capture-parse-try-parse.mjs regenerates an artifact, checks it
against the retained fixture, and refuses to overwrite an existing fixture
before starting a container. It follows docs/reference-captures.md from the
owner-authored prepared-helper work (PR #312):

- Whole captures are compared only with `assertSameCapture`; `node:assert` is
  used only for small values in `validate`.
- The prepared sequences use a local copy of that PR's `capturePrepared`
  semantics, because the helper is not on main yet. sp_prepare completion is
  awaited through the `prepared`/`error` events, `request.error` is cleared
  before each phase, only messages raised while a phase is outstanding are
  recorded, and rows and messages are bounded. Once the shared helper lands,
  the script should import it instead (the retained record shape is the same).
- Every batch, RPC and prepared statement ends in a unique `/*case name*/`
  comment, so no two programs share plan cache text.
- The one constraint is named (`parse_src_pk`).
- Containers come from `withReferenceContainer` (labelled
  `msduck.reference=1` and `msduck.owner=<cwd>:<pid>`). The script prints each
  container name, and each container is removed when its capture finishes.

Run it with a heap limit and an RSS watchdog that kills the process above
4 GB, and give it an output path under `artifacts/parse-try-parse-reference-v1/`:

```
node --max-old-space-size=2048 scripts/capture-parse-try-parse.mjs artifacts/parse-try-parse-reference-v1/check.json
```

The capture peaked at about 120 MB RSS. `--one-database` runs a diagnostic
capture in a single database and skips validation and the fixture comparison.
`--write-fixture` writes a new fixture only when none exists.

These rules describe only what the fixture shows. They say nothing about
inputs that were not tested, and msduck does not implement PARSE or
TRY_PARSE.

## Results and descriptors

| Aspect | Captured behavior |
| --- | --- |
| Result type | Exactly the target type: IntN 1/2/4/8, NumericN with the declared precision and scale (`DECIMAL` without arguments is (18,0)), MoneyN 8/4, FloatN 8 (FLOAT) or 4 (REAL), Date, Time/DateTime2/DateTimeOffset with the declared scale (default 7), DateTimeN 8 (DATETIME) or 4 (SMALLDATETIME). |
| Flags | 33 (nullable) for PARSE and TRY_PARSE alike, for literals, columns and parameters. |
| sp_describe_first_result_set | is_nullable 1 for every form. System type names: int, numeric(18,0), numeric(10,2), money, float, real, date, time(3), datetime, datetime2(4), datetimeoffset(7), smalldatetime. |
| SELECT INTO | Nullable int, datetime2 (max_length 7, precision 23, scale 3) and numeric(10,2) columns. |
| Typed NULL input | NULL for both functions. |
| PARSE and TRY_PARSE | Every successful input gives the same value from both functions. |

## Errors versus NULL

| Condition | PARSE | TRY_PARSE |
| --- | --- | --- |
| Text that cannot be converted, or a value outside the target range | Error 9819 state 1 class 16: "Error converting string value '<text>' into data type <type> using culture '<culture>'." The culture is '' when USING is absent, even when the session language supplies the culture. The column descriptor is sent, then no row and a DONE with no count. | NULL row |
| Culture '' or 'German' (a language name), 'Invariant Language (Invariant Country)', or a typed or untyped NULL culture | Error 9818 state 1 class 16: "The culture parameter '<culture>' provided in the function call is not supported." A NULL culture is shown as 'NULL'. It is raised after the column descriptor. | Same error 9818. TRY_PARSE does not suppress it. |
| Out-of-range SMALLDATETIME ('2079-06-07') | Error 6521 state 2 class 16. The message contains a .NET InvalidCastException stack trace (CXVariantBase.SqlDateTimeToSmallDate). | Same error 6521 |
| DATETIME2(0) whose rounding passes 9999-12-31 ('9999-12-31T23:59:59.99999999') | Error 6521 state 2 class 16. The message contains an ArgumentOutOfRangeException stack trace from DateTimeParse.ParseISO8601. | Same error 6521 |
| Target VARCHAR, NVARCHAR, BIT, UNIQUEIDENTIFIER, VARBINARY, XML, SQL_VARIANT or an alias type | Error 10761 state 2 class 15, raised before any metadata: "Invalid data type <type> in function PARSE." Alias types are named as `dbo.parse_alias`. | Same, with "TRY_PARSE" |
| INT, NUMERIC, DATETIME or VARBINARY input, or an untyped NULL input | Error 8116 state 1 class 16, raised before any metadata: "Argument data type <type> is invalid for argument 1 of parse function." The text says "parse" for both functions. | Same |
| INT culture (`USING 1033`) | Error 8116 for argument 2 | Same |
| NVARCHAR(MAX) input longer than 4000 characters | Error 8152 state 10: "String or binary data would be truncated." A short NVARCHAR(MAX) or CHAR(10) input is accepted. | Same error 8152 |
| No AS clause | Error 1035 state 10 class 15: "Incorrect syntax near 'PARSE', expected 'AS'." | Same, with 'TRY_PARSE' |
| Third, style argument | Error 102 near ',' | |

A 9819 error in a batch does not abort it: the next statement runs. With
XACT_ABORT ON the batch stops and the transaction is rolled back
(@@TRANCOUNT is 0 afterwards). In TRY/CATCH, ERROR_NUMBER() is 9819,
ERROR_SEVERITY() is 16 and ERROR_STATE() is 1. Over a column source, the rows
before the failing row are sent before the error. This holds for PARSE with
9819 and for TRY_PARSE with 9818 from a per-row NULL culture.

## Cultures

| Culture input | Captured behavior |
| --- | --- |
| Absent | Session language culture. us_english parses '1,234' as 1234 and dates month first. With SET LANGUAGE German, '1,5' is 1.5 and '02.01.2024' is 2024-01-02. With British, '13/12/2010' is a date. With French, '1 234,50' is money 1234.5. |
| SET DATEFORMAT dmy | Ignored by PARSE and TRY_PARSE ('01/02/2010' stays 2010-01-02). CONVERT in the same batch gives 2010-02-01. |
| 'en-US', 'en-GB', 'de-DE', 'fr-FR', 'ja-JP', 'ar-SA' | Used as listed below. |
| Neutral 'de', lowercase 'de-de', VARCHAR or an expression (`N'de-'+N'DE'`) | Accepted; behaves like de-DE for '1,5'. |
| 'xx-XX' (well-formed but unknown) | Accepted without error. '1' and '8' parse as integers (batch, column, RPC and prepared). |
| '', 'German', invariant display name, NULL | Error 9818 for both functions (see above). |

## Numeric targets

| Input | Captured behavior |
| --- | --- |
| Leading/trailing spaces, tab, line feed | Accepted ('  123  ', TAB+'123'+LF). |
| Leading U+00A0 | Rejected for INT (9819/NULL). |
| Sign | Leading '+'/'-' and trailing '-' ('123-' is -123) accepted. '- 123' rejected. '-0' gives TINYINT 0; '-1' TINYINT is out of range. |
| Parentheses | '(123)' rejected for INT. '($1.50)' is MONEY -1.5 under en-US. |
| Group separators | Accepted for integers, DECIMAL and MONEY, and not validated by position: '1,2,3,4' is INT 1234 under en-US, and '1234.5' is DECIMAL 12345 under de-DE. Rejected for FLOAT ('1,000.5' under en-US). fr-FR accepts U+0020 and U+00A0 as group separators but rejects U+202F. |
| Decimal separator | Culture specific. '.5' is 0.5 and '5.' is 5. '1,5' is 15 under en-US (grouping) and 1.5 under de-DE. |
| Fraction into an integer | '123.0' is 123. '123.5' and '-2.5' are rejected. |
| Exponent | Rejected for INT and DECIMAL ('1e3', '1.5e2'). Accepted for FLOAT ('-1.5E+300'). |
| Hex, full-width digits, Arabic-Indic digits (ar-SA) | Rejected. |
| Currency symbol | Rejected for INT ('$123'). Accepted for MONEY when it is the culture's symbol: '$' en-US, '£' en-GB, '€' prefix or suffix de-DE, '¥' (U+00A5) ja-JP. '£' under en-US is rejected. |
| Empty or spaces only | Rejected. |
| Integer ranges | INT, SMALLINT, TINYINT and BIGINT limits are exact. One past a limit is rejected. |
| DECIMAL scale | Rounds half away from zero in the captured cases: 1.235 is 1.24, -1.235 is -1.24 and 1.225 is 1.23. |
| DECIMAL precision | '12345.6' as DECIMAL(5,2) is rejected. A 39-digit fraction as DECIMAL(38,38) is rejected. |
| MONEY | Five decimals round to four (1.23456 is 1.2346). '922337203685478' is rejected. SMALLMONEY 214748.3647 is accepted and 214748.3648 is rejected. |
| FLOAT/REAL | '1e309', 'Infinity', 'NaN' and REAL '3.5E+38' are rejected. REAL '3.4028235E+38' is accepted. |

## Date and time targets

| Input | Captured behavior |
| --- | --- |
| ISO '2024-01-02', surrounding spaces | Accepted |
| 'Monday, 13 December 2010' USING 'en-US' | DATETIME2 2010-12-13 |
| Numeric order | en-US is month first: '13/12/2010' is rejected and '01/02/2010' is Jan 2. en-GB is day first: '01/02/2010' is Feb 1. de-DE '02.01.2024', ja-JP '2024/01/02' |
| Month names | 'Januar' (de-DE), 'janvier' (fr-FR) and English 'January' under de-DE are all accepted. |
| ar-SA | Hijri (Um al-Qura) calendar: '20/06/1445' is 2024-01-02. The Gregorian ISO '2024-01-02' is rejected. |
| Two-digit year | '1/2/49' is 2049 and '1/2/30' is 2030. |
| Compact '20240102' | Rejected. |
| Invalid day, empty string | Rejected ('2024-02-30'). |
| DATE with a time part, TIME with a date part | Accepted; the other part is dropped. |
| DATETIME | '.1234567' becomes .123 and '.998' becomes .997. '23:59:59.9999999' rolls over to the next day. '1752-12-31' and '9999-12-31 23:59:59.999' are rejected with 9819/NULL. |
| SMALLDATETIME | 29.998 seconds rounds down and 30 seconds rounds up. Out of range raises 6521 (see above). |
| DATETIME2(n) | Rounds to the scale: '.1235' at scale 3 is .124, '.5' at scale 0 moves to the next second, and eight fraction digits round to seven. |
| TIME(0) '23:59:59.6' | 23:59:59 (truncated rather than rolled over). Rounding at other times was not captured. |
| '25:00:00' | Rejected |
| Time-only string as DATETIME2 | The server's current date (the fixture keeps only the comparison with CAST(SYSUTCDATETIME() AS DATE), which was 1) plus the time. |
| Offset in DATETIME2 input | '2024-01-02T03:04:05+05:30' is converted to 2024-01-01 21:34:05. 'Z' is kept as 03:04:05. |
| DATETIMEOFFSET | '+05:30' is kept (text 2024-01-02 03:04:05.0000000 +05:30). 'Z' gives +00:00. A missing offset gives +00:00. '+15:00' is rejected. |
| 12-hour clock | '1/2/2024 3:04:05 PM' and '3:04 PM' under en-US |

The offset conversion and the offset assigned to input without one are
consistent with the container's UTC local time zone. They were not captured
under any other server time zone.

## RPC and prepared statements

| Case | Captured behavior |
| --- | --- |
| sp_executesql success | Rows, DONEINPROC with a count, DONEPROC, return status 0. |
| sp_executesql PARSE failure | Column descriptor, error, DONEINPROC without a count, and return status equal to the error number (9819 or 9818). |
| INT parameter as input | Error 8116 with no metadata, a DONEPROC only, and return status 8116. |
| VARCHAR text and culture parameters, NVARCHAR(MAX) text | Accepted. The culture may be a parameter. |
| sp_prepare | Returns the column descriptor and a DONEINPROC with count 0. Its return status was 8116 when the session's previous RPC had failed with 8116, and 0 otherwise. |
| sp_execute failure | Error, DONEINPROC without a count, return status -6. The next execution on the same handle succeeds with no error. |
| TRY_PARSE prepared | Unparseable text and DECIMAL overflow give NULL with return status 0. An unknown 'xx-XX' culture parses. |
| NULL culture parameter | Error 9818 in every protocol. |

## Uncaptured gaps

- Cultures other than en-US, en-GB, de-DE, fr-FR, ja-JP, ar-SA, de, de-de and
  xx-XX, and the full culture list. Session languages other than us_english,
  German, British and French.
- The rules that decide which unknown culture names are accepted. Only xx-XX
  was tried.
- Server time zones other than UTC, which affect offset conversion and the
  offset assigned to input without one.
- Exact DECIMAL values beyond double precision. tedious returns NumericN as a
  JavaScript number. Ties were captured only at 1.235, -1.235 and 1.225.
- Whitespace other than space, TAB, LF and U+00A0, other digit scripts,
  and surrogate pairs.
- Era and calendar variants other than ar-SA Um al-Qura, and TwoDigitYearMax
  boundaries other than 49 and 30.
- TIME(n) rounding away from the end of the day, and DATETIMEOFFSET overflow
  through rounding.
- sp_prepare of an invalid target. SQL Server returns a handle whose value
  appears in later messages, so it would not be deterministic; the batch and
  RPC forms are captured.
- PARSE in computed columns, CHECK constraints, indexed views or with
  SCHEMABINDING (determinism restrictions), parallel plans, and CLR-disabled or
  memory-pressure conditions.
- Collations and code pages other than the default, for VARCHAR input.

## Proposed successors

1. **Deterministic core** (for example `parse-try-parse-core-v1`, in
   `msduck-core` with binding rules in `msduck-sql`): a pure rule
   `parse(text, target, culture) -> value | failure` over an explicit, closed
   culture table built from the captured cultures, with the current date and
   the server time zone as explicit inputs. Failures would be classified as
   conversion (9819; TRY_PARSE gives NULL), unsupported culture (9818 in both),
   runtime overflow (6521 in both) and compile-time target/argument errors
   (10761, 8116, 1035). Binding would compute the target type as a nullable
   descriptor. Tests would reproduce the retained fixture's values without a
   database.
2. **Root adapter** (for example `parse-try-parse-root-v1`): lower PARSE and
   TRY_PARSE through the core rule in DuckDB execution. It would pass the
   session language as the default culture, supply the clock and time zone
   explicitly, and emit the exact error tokens, DONE tokens and RPC/prepared
   return statuses (error number for sp_executesql, -6 for sp_execute). It
   would compare batch, sp_executesql and prepared paths against
   reference/parse-try-parse.json. Unsupported cultures and inputs should stay
   explicit errors rather than guesses.
