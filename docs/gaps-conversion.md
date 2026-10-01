# Explicit COLLATE, styled CONVERT, FORMAT and server properties

Issue #725 (task `gaps-conversion-v1`). Before this work, msduck v0.2.4 failed
on these generic repros:

- expression `COLLATE Latin1_General_CI_AS` and `COLLATE Latin1_General_CS_AS`
  (208, unknown DuckDB collation);
- `CONVERT(nvarchar(40), <datetimeoffset>, 127)` and datetime style 126 (208);
- `CONVERT(varchar(6), 0x010203, 2)` (50000, backend parser error);
- `FORMAT(CAST('2024-01-01' AS date), 'yyyy-MM-dd')` (50000, binder error);
- `SERVERPROPERTY('Collation')`, `ROWCOUNT_BIG()` and `DATABASEPROPERTYEX`
  (208).

All of them now return SQL Server's values. The feature lives in the
extension hooks (docs/extension-hooks.md): `src/engine/ext/conversion.rs` and
its submodules. No shared dispatch file changed.

## Reference evidence

`reference/gaps-conversion.json` holds 1,326 programs captured by
`scripts/capture-gaps-conversion.mjs` from one fresh database of
`mcr.microsoft.com/mssql/server:2022-latest` (ProductVersion 16.0.4236.2,
server collation SQL_Latin1_General_CP1_CI_AS, `TZ=UTC`). Each program keeps
its result descriptors (type and length), rows and diagnostics (number,
state, class and message). They cover:

- every date/time style from 0 to 131 (and the invalid styles 15, 99, 128 and
  200) for datetime, smalldatetime, date, time(0/3/7), datetime2(0/3/7) and
  datetimeoffset(0/3/7), to varchar and nvarchar;
- round trips through each style, and 50 hand-written strings converted to
  each date/time type with CONVERT and TRY_CONVERT;
- binary styles in both directions, including truncation, padding and errors;
- explicit COLLATE comparisons, ORDER BY, COUNT(DISTINCT) and projection for
  13 collations, plus 447, 448 and 468;
- SERVERPROPERTY, DATABASEPROPERTYEX and ROWCOUNT_BIG with SQL_VARIANT_PROPERTY.

FORMAT uses the existing `reference/format.json` (docs/format.md). The script
refuses to overwrite an existing fixture and labels its container with the
task id.

## Explicit COLLATE

Names are checked before binding against SQL Server's grammar: the 73 SQL
collations and the Windows designators of `sys.fn_helpcollations()` with
`BIN`, `BIN2` (optionally `_UTF8`) or `CI|CS_AI|AS[_KS][_WS][_SC][_UTF8]`.
An unknown name raises 448 and a numeric literal 447, as in SQL Server. A
valid collation without an implementation (for example German_PhoneBook)
reports "unsupported collation".

After translation an explicit collation becomes a DuckDB collation chain:

| SQL Server | DuckDB |
| --- | --- |
| `*_CI_AS` | `nocase` + ICU locale |
| `*_CS_AS` | ICU locale |
| `*_CI_AI` | `nocase.noaccent` + ICU locale |
| `*_CS_AI` | `noaccent` + ICU locale |
| `*_BIN`, `*_BIN2` | code point order |

Latin1_General, SQL_Latin1_General and the other Latin collations use the ICU
`en_us` locale, which orders lowercase before uppercase and accented letters
after their base letter, like the captured Windows collations; about 30 other
designators map to their ICU language. Comparisons (`=`, `<>`, `<`, `<=`,
`>`, `>=`), IN and BETWEEN with an explicit collation ignore trailing spaces,
as SQL Server does. COUNT(DISTINCT) applies the collation's equality, and
LIKE, ORDER BY, GROUP BY and DISTINCT use the DuckDB collation. Unicode RPC
parameters and expression results are read through the UTF-16 carrier.

The default collation is unchanged: comparisons without COLLATE are still
binary (N'A' = N'a' is false).

## Styled CONVERT

The built-in conversions keep the styles they already handled (currency to
text, and binary to Unicode text when the binder knows the source). Every
other styled CONVERT or TRY_CONVERT is lowered after translation by the type
of the backend value:

- To character types, a native function formats date/time values with styles
  0 to 14, 20 to 25, 100 to 115, 120, 121, 126, 127, 130 and 131 (Hijri, with
  .NET's tabular calendar and SQL Server's Arabic month names), binary values
  with styles 0, 1 and 2, and float/real values with styles 0 to 3. The
  target's ordinary width, padding and code page rules then apply.
  Hexadecimal styles keep whole bytes when the target is too short ('0x' for
  `CONVERT(varchar(3), 0x0A0B, 1)`).
- To binary types, styles 1 and 2 read hexadecimal text (style 1 requires the
  `0x` prefix) and style 0 the text's bytes, then truncate or pad.
- To date/time types, the text is parsed with the style's field order (mdy,
  dmy or ymd), flexible separators, month names, two-digit years (cutoff
  2049), `hh:mi[:ss[.fffffff|:mmm]][AM|PM]` times and `Z` or `±hh:mm`
  offsets. A leading four-digit year reads the remaining fields in the style's
  order for datetime and smalldatetime only, as SQL Server does
  (`CONVERT(datetime, '2024-01-02', 103)` is February 1). The ISO 8601 result
  goes through the built-in conversion, so rounding and ranges are unchanged.
- Styles of numeric, bit and other targets do not change SQL Server's result
  and are dropped.

Errors follow SQL Server: 281 for a style that does not exist for the source
type, 8114 for a date style on a time value (and the reverse), 9809 for other
binary styles, 241 (295 for smalldatetime) for text that does not parse, 242
for an out-of-range field and 8114 for invalid hexadecimal. TRY_CONVERT
returns NULL instead.

Unstyled CAST and CONVERT of bit values to character types give `1` and
`0` (DuckDB writes BOOLEAN as `true` and `false`).

## FORMAT

FORMAT becomes a native function typed nvarchar(4000), implementing .NET
Framework formatting for the en-US and invariant cultures (and well-formed
tags of unknown languages, which .NET formats like the invariant culture):

- numeric standard formats C, D, E, F, G, N, P, R and X with precision up to
  99, and custom formats with `0 # . , % ‰ E0 e+0 ;` sections, quotes and
  escapes, rounding half away from zero;
- date standard formats d D f F g G M O R s t T u U Y and custom formats,
  including F/f fractions, `zzz` and `K`;
- TIME as a .NET TimeSpan: c, t, T, g, G and custom formats.

A format .NET rejects returns NULL. The culture defaults to en-US (the
session language is us_english). 189, 8116 (literal, CAST/CONVERT and
variable argument types) and 9818 (constant culture names) are raised when the
statement compiles. Other languages (de-DE, fr-FR, ja-JP and so on) report
"unsupported FORMAT culture".

The en-US and invariant FORMAT calls of reference/format.json (over 500) all
match, except smalldatetime values (see below) and a DECIMAL(38,38) literal
that msduck already reads with fewer digits.

## SERVERPROPERTY, DATABASEPROPERTYEX and ROWCOUNT_BIG

Calls with constant or variable arguments become typed constants before
binding. A call projected directly by the outermost SELECT keeps the
sql_variant type (nvarchar(128), int or tinyint base types, as
SQL_VARIANT_PROPERTY reports them). Elsewhere, for example inside CAST,
CONVERT or a comparison, the call has its base type, which is what SQL
Server's implicit conversion from sql_variant produces. Row-dependent
arguments (`DATABASEPROPERTYEX(name, 'Status') FROM sys.databases`) choose
among the values with a CASE. Unknown names and missing databases give NULL.

Collation is `SQL_Latin1_General_CP1_CI_AS` for the server and every
database (CollationID 872468488, ComparisonStyle 196609, SqlSortOrder 52,
SqlSortOrderName `nocase_iso`), consistent with `sys.databases` and the
login collation. Note that msduck's default comparisons are currently
case and accent sensitive (binary): `N'A' = N'a'` is false without an
explicit case-insensitive COLLATE, unlike a real SQL_Latin1_General_CP1_CI_AS
server. ProductVersion is `16.0.0.0`, matching the
version in PRELOGIN and LOGINACK (ProductMajorVersion 16, ProductLevel RTM,
Edition `Developer Edition (64-bit)`, EngineEdition 3). ServerName,
MachineName and ComputerNamePhysicalNetBIOS are the host name and
InstanceName is NULL (a default instance). `IsCaseSensitive` is not a
SERVERPROPERTY name, so SQL Server and msduck return NULL; use the Collation
or ComparisonStyle properties. Status, Recovery, Updateability and UserAccess
come from `sys.databases`, so ALTER DATABASE ... SET READ_ONLY shows.

ROWCOUNT_BIG() is the session's row count as bigint. Stored definitions
(views, tables) reject ROWCOUNT_BIG and DATABASEPROPERTYEX, which would
otherwise be frozen at definition time.

## Remaining differences

`tests/compat/conversion.test.mjs` replays the whole capture and fails on any
difference not listed here, and on a listed difference that disappears.

- Errors for constant arguments are raised when the statement compiles, so
  they come without the column metadata SQL Server sends first. Errors that
  depend on row values come from the native functions with SQL Server's text
  but number 50000 (241 keeps its number), because runtime messages can only
  map to numbers through the shared engine.
- smalldatetime values keep their seconds in msduck storage, so styles and
  FORMAT show seconds SQL Server rounds away.
- The fractional digits of a time value follow its declared scale only when
  the scale is evident (a CAST, CONVERT or variable); a time column formats
  with 7 digits.
- Wire descriptors keep the default collation for explicit collations other
  than the six names `msduck_tds::collation` knows (for example
  Latin1_General_CI_AS reports sort ID 52 instead of version 0, sort ID 0).
- varchar values under SQL_* case-sensitive collations use Windows order
  (`'a' < 'A'`); SQL Server's SQL sort orders put uppercase first.
- GROUP BY and DISTINCT under an explicit collation do not ignore trailing
  spaces, and duplicate ORDER BY items are not rejected with 169.
- ROWCOUNT_BIG() is described as nullable bigint (IntN, length 8).
- The reported collation is case insensitive while default comparisons are
  binary (see above).
- User databases report the SIMPLE recovery model, and IsXTPSupported, IsFulltextEnabled, ProductBuild and
  ProductUpdateLevel differ from the reference server.
- SQL_VARIANT_PROPERTY of a non-constant property name and CONVERT of a
  sysname sql_variant produced elsewhere use the built-in sql_variant support,
  which handles integer base types only.
