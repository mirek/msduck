# ISNUMERIC and ISDATE reference contract

The retained fixture in reference/isnumeric-isdate.json contains 160 SQL
Server programs, each captured in two fresh databases in each of two
independent containers. Both containers used the pinned SQL Server 2025 image
digest 86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a.
The four raw captures matched exactly. The programs comprise 122 ordinary
batches, 36 sp_executesql RPC calls and two prepared handles with ten
executions in total. They retain result rows, TDS column descriptors, error
number/state/class/text, information messages and DONE tokens.
scripts/capture-isnumeric-isdate.mjs regenerates an artifact and checks the
retained fixture. It refuses to overwrite an existing fixture before any
container starts. `--one-database` is a diagnostic mode that captures one
database and never writes or compares the fixture.

This is reference evidence only. msduck does not implement either function
on the basis of this task.

## Shared result contract

- Both functions return `int`, described as an IntN column of length 4 with
  flags 33 (nullable). SELECT INTO creates a nullable `int` column, and
  sys.dm_exec_describe_first_result_set reports `int` and nullable.
- Neither returns NULL: every NULL input (untyped, typed, column or bound
  parameter) yields 0.
- Zero or two arguments raise error 174 (class 15, state 1) "The isnumeric
  function requires 1 argument(s)." (or isdate), before any metadata.
- An unaliased call yields an empty column name.
- Rejected argument types raise error 8116 (class 16, state 1) "Argument data
  type T is invalid for argument 1 of F function." before metadata.
- ISNUMERIC is accepted in a PERSISTED computed column. ISDATE is rejected
  with error 4936 (non-deterministic), which aborts the batch. ISDATE works in
  a CHECK constraint; a failing insert gives error 547 plus information 3621.

## ISNUMERIC observed rules

Argument types: exact and approximate numerics, money, smallmoney and bit
return 1. Datetime, smalldatetime and uniqueidentifier return 0. Binary 0x31
returns 1 (implicitly read as character '1'). DATE, DATETIME2, TIME,
DATETIMEOFFSET, SQL_VARIANT, TEXT, NTEXT and XML raise 8116, including
typed NULL DATE.

Character strings return 1 when SQL Server's permissive numeric scanner
accepts them, which is broader than any single conversion target. The
fixture pairs every probe string with TRY_CONVERT to INT, BIGINT,
DECIMAL(38,10), FLOAT and MONEY. Observed rules:

| Input | ISNUMERIC |
| --- | --- |
| `0`, `123`, `-123`, `+123`, `1.5`, `.5`, `5.` | 1 |
| Lone `.`, `+`, `-`, `+.`, `-.`, `$`, `,`, `,.`, `.,`, `\` | 1 (only MONEY converts, to 0) |
| `''`, `' '`, `'   '` | 0, although INT, BIGINT, FLOAT and MONEY convert them to 0 |
| Exponent `1e5`, `1E5`, `1e+5`, `1e-5`, `1.e1`, `1d5`, `1D5` | 1 (only FLOAT converts) |
| `1e`, `e5`, `1e1.5`, `.e1`, `1d`, `1e5e5`, `1ee5` | 0 |
| Commas anywhere in the digits: `1,000`, `1,,2`, `1,2,3`, `,1`, `1,`, `1.000,5` | 1 (only MONEY converts, dropping commas) |
| Double or trailing signs `--1`, `+-1`, `-+1`, `1-`, `1+`; `1.2.3` | 0 |
| Currency prefix `$1`, `$-1`, `-$1`, `+$1`, `$+1`, `$ 1`, `$1,000`, `\1` | 1 (only MONEY converts) |
| Currency suffix `1$`, doubled `$$1`, currency with exponent `$1e5` | 0 |
| Space after sign `- 1`, `+ 1`; leading/trailing/both spaces | 1; embedded space `1 000` is 0 |
| Hex `0x1A`, `0x`, `0x0`, `&H1A`, letters, `%`, `/`, `:` | 0 |
| `Inf`, `-Inf`, `Infinity`, `NaN` | 0 |
| Integers beyond INT or BIGINT, beyond MONEY, 38 or 39 nines | 1 |
| Float overflow: `1e309`, `-1e309`, `1.7976931348623159e308`, 309 nines, `1` followed by 309 zeros, `$` + 309 nines | 0 |
| `1e308`, `1.7976931348623157e308`, `1` followed by 308 zeros, `4.9e-324`, `1e-400`, a 400-zero fraction | 1 |

Control characters and positions differ, so the fixture keeps separate
single-character, prefix (`X1`), suffix (`1X`) and infix (`1X2`) scans:

- VARCHAR, bytes 0-255 under the default collation: alone 9-13, `$ + , - .`,
  digits, `\`, 128 (euro), 160 (NBSP), 162-165. Prefix: 9, 10, 13, 32, the
  same symbols and digits, 128, 162-165 (not 11, 12 or 160). Suffix: 0, 11,
  12, 32, `,`, `.`, digits and 160 (not tab, LF or CR). So `CHAR(9)+'1'` is 1
  and `'1'+CHAR(9)` is 0. `'1'+CHAR(0)` is 1 and `CHAR(0)+'1'` is 0.
- NCHAR over U+0000-U+FFFF: 101 single characters, 108 prefixes, 58 suffixes
  and 68 infixes return 1; the exact code point lists are retained. Besides
  the ASCII set they include fullwidth digits and `＄ ＋ ， － ． ＼`,
  superscripts U+2074-U+2078, subscripts U+2080-U+2089 (but not U+00B2,
  U+00B3 or U+00B9), U+2212 minus, U+2010-U+2012 and U+2015 dashes (not en
  or em dash), U+20A1, U+20A4, U+20AC currency, U+221E infinity, U+2216,
  U+263C, U+2007, U+202F and a subset of box-drawing characters
  U+2500-U+256C. U+2000-U+200A spaces (except U+2007 as a prefix) and U+3000
  work as prefixes and suffixes. The infix scan adds exponent letters D, E, d, e, their
  fullwidth forms, Latin letters U+010E, U+010F and U+0111-U+011B, Greek δ and ε, and
  U+2107, U+212E-U+2130.
- Unicode digits in other scripts (Arabic-Indic, Devanagari), `²`, `½`,
  `Ⅻ`, `₹1`, `₩1`, `฿1`, `₪1` and `1٫5` return 0. `€1`, `£1`, `¥1`, `¢1`,
  `¤1`, `＄1`, fullwidth `１２３`, `＋1`, `－1`, `−1` and `1．5` return 1 while
  every captured TRY_CONVERT except MONEY for the currencies returns NULL.
  Several currencies (`₩`, `฿`, `₪`) convert to MONEY but give ISNUMERIC 0.
- NVARCHAR NBSP before a digit returns 0; U+3000 before a digit returns 1.
- Inputs cast to VARCHAR follow the code page: unmappable characters become
  `?` and return 0, while best-fit mappings (`＄`, `＋`, `－`, `−`, `１２３`,
  U+3000) become ASCII and return 1.

Length: a VARCHAR(MAX) or NVARCHAR(MAX) argument longer than 8000 characters
raises error 8152 (state 10, class 16) after the result descriptor. In a
multi-row probe this occurs after four earlier rows were sent. Lengths 4000,
4001 and 8000 are accepted, including a 4001-character NVARCHAR RPC
parameter. Collation clauses, a UTF-8 collated column and CHAR/NCHAR
padding do not change the observed results; `CAST('' AS CHAR(3))` is 0.

## ISDATE observed rules

ISDATE returns 1 exactly when the string would convert to DATETIME under the
session's DATEFORMAT and LANGUAGE. Strings that only DATE, DATETIME2 or
SMALLDATETIME accept are not enough:

| Input (us_english, mdy) | ISDATE |
| --- | --- |
| `2024-01-02`, `20240102`, `240102`, `2024-01-02T03:04:05`, space separator, `.123`, `Z` suffix | 1 |
| More than three fractional digits (`.1234`, `.1234567`) | 0, although DATETIME2 converts |
| Offsets `+02:00`, with or without a space | 0 |
| `1753-01-01` through `9999-12-31 23:59:59.998` (which rounds to .997) | 1 |
| `1752-12-31`, `0001-01-01`, `9999-12-31 23:59:59.999`, `10000-01-01` | 0 |
| Beyond SMALLDATETIME (`1899-12-31`, `2079-06-07`) | 1 |
| Invalid calendar days or months, including `2023-02-29` | 0 |
| `01/02/2024`, `01-02-2024`, `01.02.2024`, `2024/01/02`, `2024.01.02`, `2024-1-2`, `1/2/24` | 1 |
| `13/01/2024` | 0 |
| `2024` alone, `Jan 2024`, `Jan 2 2024`, `January 2, 2024`, `2 Jan 2024`, `2024 Jan 2` | 1 |
| `24`, `1`, `0`, `202401`, `2024-01`, `January`, `Jan`, `Janu 2 2024`, weekday prefix, ISO week forms | 0 |
| Time only `12:00`, `12:00 PM`, `12 PM`, `3PM`, `23:59:59.999`; `2024-01-02 13:00 PM`; `03:04:05:123` | 1 |
| `24:00`, `25:00`, `12:60`, `03.04.05` time, trailing `T`, `T03:04:05` | 0 |
| Leading/trailing ordinary spaces | 1 |
| `''`, `' '` | 0, although every conversion gives 1900-01-01 |
| Tab, LF, CRLF, NBSP, NUL or U+3000 around the date | 0 (TRY_CONVERT to DATE/DATETIME2 accepts tab) |
| Fullwidth or Arabic-Indic digits, fullwidth hyphen, ODBC `{d ...}` text | 0 |

Argument types: DATETIME, SMALLDATETIME, CHAR, NCHAR and VARCHAR(MAX) return 1
for a valid value; INT 20240102 (literal or RPC) returns 1. DECIMAL, FLOAT,
MONEY, BIT, binary (including RPC VARBINARY text) and uniqueidentifier
return 0. DATE, DATETIME2, DATETIMEOFFSET, TIME, SQL_VARIANT, TEXT, NTEXT and
XML raise 8116, from literals, columns and RPC parameters alike.

Length: a VARCHAR(MAX) argument of 4001-8000 characters raises 8152 state 4.
More than 8000 characters (VARCHAR) or 4000 characters (NVARCHAR, including a
4001-character RPC parameter) raises 8152 state 10. Padded 4000-character
inputs return 1.

DATEFORMAT and LANGUAGE:

- DATEFORMAT dmy, ydm and dym change how DATETIME reads `yyyy-mm-dd`:
  `2024-01-02` becomes 1 February and `2024-01-13` gives ISDATE 0, while
  `2024-13-01` gives 1. DATE and DATETIME2 still read these as ISO. Unseparated
  `yyyymmdd` is unaffected. `2024-13-01T03:04:05` is 0 under ydm while the
  space-separated form is 1.
- ymd reads `01.02.03` as 2001-02-03. myd and dym parse `mm/yyyy/dd` and
  `dd/yyyy/mm`. The two-digit year window observed is 49 -> 2049,
  50 -> 1950.
- SET LANGUAGE implies a DATEFORMAT. sys.dm_exec_sessions reported dmy for
  Deutsch; results imply dmy for British and Français and ymd for Japanese. It also
  selects month names: Deutsch accepts `Januar`, `Mai`, `Mär`, `Dezember`
  and rejects `May` and `December`; Français accepts `janvier`, `février`
  and rejects `fevrier` and `January`. A later SET DATEFORMAT overrides the
  language's order. Each SET LANGUAGE emits information 5703 in the new
  language's text.
- A SET earlier in the same batch governs the later statement. Separate-batch
  session SETs produce the same results.
- Inside sp_executesql, SET DATEFORMAT and SET LANGUAGE apply to the dynamic
  batch and revert afterwards (the next RPC saw mdy and us_english). A
  session DATEFORMAT dmy applies to sp_executesql and to a handle prepared
  and executed under it.

## RPC and prepared completions

sp_executesql results end with DONEINPROC (row count 1, more) and DONEPROC.
Errors 8116 in an RPC produce only DONEPROC; error 8152 produces metadata,
DONEINPROC without a count, then DONEPROC. sp_prepare returns the two-column
descriptor with DONEINPROC(0) and DONEPROC. Each sp_execute replays the same
descriptor; sp_unprepare returns only DONEPROC. Prepared results equal the
matching ad hoc and sp_executesql values.

## Gaps

Not captured: other collations and code pages for VARCHAR scans (only the
server default, one BIN2 and one UTF-8 case); supplementary characters;
languages other than us_english, British, Deutsch, Français and Japanese;
TWO DIGIT YEAR CUTOFF other than the default; compatibility levels; SET
DATEFIRST (not expected to matter); ISNUMERIC or ISDATE over
table-valued parameters, bulk loads, or in parallel plans; and the exact
scanner state machine. The code point scans establish which single
characters are accepted in four positions, not a complete grammar for longer
mixed strings. INT 20240102 returning 1 from ISDATE is recorded but its
conversion path was not isolated. No msduck or DuckDB behavior was compared.

## Proposed successors

1. Deterministic core (suggested `isnumeric-isdate-core-v1`, scope a new
   `crates/msduck-core/src/isnumeric.rs` and `crates/msduck-core/src/isdate.rs`
   plus their `lib.rs` exports and unit tests). Implement a pure ISNUMERIC
   classifier over UTF-16 code units, with explicit VARCHAR input given as
   code-page bytes and best-fit mapping supplied by the caller. Reproduce the
   retained positional code point sets, float overflow boundary and
   8000-character limit. Implement a pure legacy DATETIME literal recognizer
   parameterized by DATEFORMAT, language month names and the two-digit year
   window, returning validity and the 8152 length outcomes. Add the argument
   type admissibility table (1, 0 or 8116) and `int` nullable result
   descriptor. Drive tests from reference/isnumeric-isdate.json.
2. Root adapter (suggested `isnumeric-isdate-root-v1`, depending on the core
   task; scope the root function lowering, session-option and client-test
   files named by the queue publisher). Bind ISNUMERIC and ISDATE to the core
   rules with the session's current DATEFORMAT and LANGUAGE, including SET
   scoping inside sp_executesql and prepared handles. Emit 174, 8116 and 8152
   and the captured DONE ordering. Mark ISDATE non-deterministic for
   persisted computed columns (4936). Add a comparison test against this
   fixture. DuckDB has no equivalent function, so its casts are not evidence.
