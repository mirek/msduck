# SOUNDEX and DIFFERENCE reference contract

The retained fixture in reference/soundex-difference.json holds 115 SQL Server
observations. Each was captured in two fresh databases in each of two
independent containers. Both containers used the pinned SQL Server 2025 image
digest 86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a
(ProductMajorVersion 17, server and database collation
SQL_Latin1_General_CP1_CI_AS, default compatibility level 170). The four raw
captures matched exactly. The observations are:

- 90 ordinary batches, including 5 setup statements and 8
  `ALTER DATABASE CURRENT SET COMPATIBILITY_LEVEL` batches;
- 24 sp_executesql RPC calls;
- one prepared handle, with its sp_prepare, six sp_execute calls and its
  sp_unprepare retained separately.

They retain result rows, TDS column descriptors (including collations),
error number/state/class/line/text, information messages and DONE tokens.
scripts/capture-soundex-difference.mjs regenerates an artifact and compares it
against the retained fixture with the shared bounded helpers. With
`--write-fixture` it refuses to run when the fixture already exists, and it
writes the fixture with an exclusive-create flag. `--one-database` is a
diagnostic mode that cannot write the fixture.

Every recorded statement ends with a `/* record name */` comment. As a result,
no two programs share a plan-cache entry, including sp_executesql calls, the
prepared statement and statements repeated at each compatibility level. An
earlier diagnostic capture without the comments produced identical results for
every record that exists in both captures. The per-level RPC probe was changed
between the two captures, so it could not be compared. The prepared-handle
helper records a tedious request error only for the execution that raised it;
tedious reuses the first error for later callbacks. No captured sp_prepare or
sp_execute raised an error.

msduck does not implement these functions. This document describes SQL Server
only. Rules stated below are the smallest descriptions consistent with every
captured row. They are observations, not vendor specifications.

## SOUNDEX encoding (compatibility level 110 through 170)

Levels 110, 120, 130, 140, 150, 160 and 170 produced identical results for every
level-sensitive probe. The rules for level 100 follow in the next section.

1. **Conversion.** The input is converted to VARCHAR in its collation's code
   page first. Every other rule operates on those bytes. NVARCHAR input
   therefore uses best-fit mapping. For example, `Łódź` became `Lodz` (L320),
   fullwidth `ＲＯＢＥＲＴ` became R163, Greek `α` became `a`, `Σ` became `S`,
   KELVIN SIGN became `K`, ANGSTROM SIGN became `Å`, dotless `ı` became `i`,
   `ĥ` became `h`, and `Ĥ`, `Ħ` and `Ŵ` became `H`, `H` and `W`. Characters
   without a mapping become `?`. This covers Cyrillic, Hebrew, Arabic, CJK,
   `Α` (capital alpha), `Ĳ`, `ﬁ`, U+1E9E, BOM, ZWSP, U+3000, U+2002, each
   surrogate half and U+FFFD. `CAST(N'Москва' AS VARCHAR(10))` is `??????`.
2. **Letters.** Under the default code page 1252, the letters are exactly the
   bytes 0x41-0x5A, 0x61-0x7A, 0xC0-0xD6, 0xD8-0xF6 and 0xF8-0xFF. Every other
   byte is a non-letter, including 0x80-0xBF. So `Š`, `Œ`, `Ž`, `š`, `œ`,
   `ž`, `Ÿ`, `ª`, `µ` and `º` are non-letters, as are `×` and `÷`. Every
   captured byte 0-255 was classified; see "soundex per character varchar".
3. **First letter.** If the first byte is not a letter, the result is `0000`.
   This applies to empty strings, spaces, tabs, line feeds, NBSP, digits,
   punctuation and every non-letter byte. Otherwise the result begins with
   that letter, uppercased by the code page. `a`-`z` map to `A`-`Z`, `à`-`þ`
   map to `À`-`Þ`, and `ÿ` maps to `Ÿ` (0x9F). `ß` stays `ß`. Accented and
   other letters above 0x7F are kept verbatim as the first character (`É540`,
   `Ñ530`, `Æ260`, `Ø230`, `Þ600`, `ß000`).
4. **Codes.** B F P V = 1. C G J K Q S X Z = 2. D T = 3. L = 4. M N = 5.
   R = 6. Case-insensitive.
5. **Separators.** A E I O U Y (either case), lowercase `h` and `w`, and every
   letter 0xC0-0xFF are uncoded. They end a run of equal codes, so `BAB` gives
   B100 and `Bhb` gives B100.
6. **Transparent H/W.** Only **uppercase** `H` (0x48) and `W` (0x57) are
   uncoded *and* transparent. The codes on either side of them merge: `BHB`,
   `BWB`, `BHWB`, `SHC` and `SWC` give B000, B000, B000, S000 and S000. The
   lowercase forms separate: `Ashcraft` and `Ashcroft` give A226, and
   `bwb` gives B100.
7. **Adjacent duplicates.** Consecutive letters with the same code (possibly
   across uppercase H/W) produce one digit (`Tymczak` T522, `Burroughs`
   B622).
8. **First-letter duplicates.** The first letter's own code suppresses an
   immediately following run with the same code. This includes runs across
   uppercase H/W, but not runs after a separator. Examples: `Pfister` P236,
   `Bb`, `Bf` and `Bpfv` B000, `Lloyd` L300, `Schmidt` S530, `Sch` S000, `Qqq`
   Q000, `Zz` Z000.
9. **Stop.** The first non-letter byte after the first letter ends encoding.
   The rest of the string is ignored. Examples: `Rob ert`, `Rob-ert` and
   `Rob<ZWSP>ert` give R100. `a1b2c3`, `a.b.c` and `a b c` give A000.
   `abc.` gives A120. `Mc Donald` gives M200. `O'Brien` gives O000.
   `R<NBSP>obert` gives R000. `Ro<U+0301>bert` gives R000, and `R😀bert`
   gives R000. Trailing spaces, tabs and emoji after a complete code have no
   effect.
10. **Length.** Truncate to 4 characters and pad with `0`. REPLICATE of
    `Robert` to 12000 characters as VARCHAR(MAX) or NVARCHAR(MAX) gives R163.
    8000 `b` characters give B000. `'Rob'+9000 spaces+'ert'` gives R100.

Other collations: the probes below were captured but not generalized.

- `Latin1_General_BIN2`, `Latin1_General_CS_AS` and `Latin1_General_CI_AI`
  gave the same codes as the default for the probes used.
- Under `Latin1_General_100_CI_AS_SC_UTF8`, VARCHAR `Émile` gives 0000,
  because the lead byte of the multibyte `É` is not a letter.
- Under `Cyrillic_General_CI_AS`, VARCHAR `Жук` gives 0000.
- Under `Turkish_CI_AS`, `N'ımre'` gives 0000, even though 0xFD is a letter
  in code page 1252. `N'imre'` gives I560, with `i` uppercased to `I`
  rather than `İ`.

## Compatibility level 100

`ALTER DATABASE ... SET COMPATIBILITY_LEVEL=100` restores the legacy encoding.
The level also applies to sp_executesql. At each level, the per-level RPC
probe `DIFFERENCE(@a,@b)` with `@a='BHB'` and `@b='BAB'` returned codes
B100/B100 at level 100 and B000/B100 at levels 110 through 170. It returned 4
at every level. The only differences from 110 and later are:

- `H`, `W`, `h` and `w` all act as separators (`BHB` B100, `SHC` S200).
- The first letter's code is not used for duplicate suppression (`Pfister`
  P123, `Bb` B100, `Bpfv` B100, `Lloyd` L430, `Schmidt` S253, `Qqq` Q200).

Across bytes 0-255, only B, F, P and V (both cases) and uppercase H and W
changed their per-character results. NVARCHAR code points that best-fit to those letters
changed in the same way. The source-word table, the code matrix and the
literals were otherwise identical.

Setting level 100 on a database that has a persisted computed column
`SOUNDEX(v)` disabled the column's index and the table's clustered primary
key. Each emitted information message 3817, state 1, class 0 ("Warning: The
index "..." on "dbo"."soundex_computed" was disabled because the
implementation of "soundex" have changed."). The ALTER batch ended with
DONE(null, more true) and DONE(1). Moving from 100 to 110 and later emitted no
further messages. The captured sequence did not re-enable those indexes.

## SOUNDEX typing and errors

| Case | Captured behavior |
| --- | --- |
| Result descriptor | VarChar, TDS length **5** (not 4), flags 33, for VARCHAR, NVARCHAR, CHAR, NCHAR, both MAX types, typed and untyped NULL, numeric, date, binary and GUID inputs, and RPC or prepared parameters. `sys.dm_exec_describe_first_result_set` reports `varchar(5)`. SELECT INTO creates a nullable `varchar` with max_length 5. The unnamed column name is empty. |
| Result collation | The input collation, or the database default for NVARCHAR literals. A `Latin1_General_BIN2` or `CS_AS` input produced flags 35 and that collation. `SQL_VARIANT_PROPERTY` reports `varchar`, MaxLength 5 and `Latin1_General_BIN2`. Comparing a BIN2 result with a CS_AS result raises 468, state 9, class 16. |
| NULL | Untyped `NULL`, typed NULL and NULL parameters return NULL without error. |
| Implicit conversion | INT 123, DECIMAL 1.5, DATE and UNIQUEIDENTIFIER convert to their string forms, which start with a digit, so the result is `0000`. VARBINARY 0x526F62657274 converts byte-wise to `Robert` (R163). An INT RPC parameter returned `0000`. |
| Rejected types | TEXT, NTEXT, XML and SQL_VARIANT: error 8116, state 1, class 16 ("Argument data type text is invalid for argument 1 of soundex function."), before metadata. The batch then ends with DONE(null). |
| Argument count | Zero or two arguments: error 174, state 1, class 15 ("The soundex function requires 1 argument(s)."). |
| Determinism | `COLUMNPROPERTY` IsDeterministic=1 and IsPrecise=1 for persisted `SOUNDEX(v)` and `DIFFERENCE(v,'Robert')` computed columns. Both persisted columns were created and populated without error. `CREATE INDEX` on the SOUNDEX column `s` succeeded. No index was attempted on the DIFFERENCE column `d`, so index support for it is not captured. |

## DIFFERENCE

| Case | Captured behavior |
| --- | --- |
| Result descriptor | IntN length 4, flags 33. SELECT INTO creates a nullable `int`. `describe_first_result_set` reports `int`. |
| Values | Range 0-4. Every pair in the word matrix with equal level-170 SOUNDEX codes returned 4, for example (`Green`, `Greene`), (`Robert`, `Rupert`) and (`''`, `'1abc'`). That does not hold for every input: `DIFFERENCE('Pfister','Pister')` is 3 (see the next row). A result of 4 does **not** imply equal codes. `DIFFERENCE('BABAB','BABAC')` (B110 against B120) is 4. |
| Compatibility level | Every DIFFERENCE value in the per-level probes was identical at levels 100 through 170. The full code matrix, captured at levels 100 and 170, was also identical. At level 170, `DIFFERENCE('BHB','BAB')`, `('SHC','SAC')` and `('Bb','Bab')` all return 4, even though the SOUNDEX codes differ (B000/B100, S000/S200, B000/B100). `DIFFERENCE('Pfister','Pister')` returns **3** at every level. At levels 110-170, `Pfister` encodes to P236. `SOUNDEX('Pister')` was not captured; the rules above give P236 at every level. So the level-170 codes appear equal, but the score is below 4. At level 100, `Pfister` is P123. DIFFERENCE is therefore not a function of the level-170 SOUNDEX output. The results are consistent with the level-100 (legacy) codes. Across the 172 source words compared against `Robert`, DIFFERENCE was 4 exactly when the legacy code was R163. |
| Asymmetry | 76 of the 256 captured ordered word pairs differ from the reversed pair. For example, `DIFFERENCE('A','')` is 0 but `DIFFERENCE('','A')` is 3. `('BABAB','BABABAB')` is 3 and the reverse is 4. |
| Letterless codes | When the second argument has code `0000` and the first has a letter, the result was 0 in every capture (`('A','1')`, `('Lee','')`, `('Robert',' ')`). With `0000` first, the result depends on the zeros in the second code: 3 for `L000` and `A000`, 2 for `R150`, `S530` and `G650`, and 0 for `R163`, `A226` and `T522`. Two letterless codes return 4 (`('','')`, `('',' ')` and `('1','2')`). |
| NULL | NULL if either argument is NULL, whether typed, untyped or an RPC or prepared parameter. Untyped `NULL` is accepted in either position. |
| Types | Unlike SOUNDEX, TEXT is accepted: `DIFFERENCE(CAST('a' AS TEXT),'a')` is 4. `DIFFERENCE(1,2)` is 4, because both convert to digit strings. VARCHAR/NVARCHAR and MAX mixes are accepted. |
| Collation | Explicit conflicting collations raise 468, state 9, class 16 ("... in the difference operation."). |
| Argument count | One or three arguments: error 174, state 1, class 15 ("The difference function requires 2 argument(s)."). |

The fixture retains a 34x34 code matrix (1,156 rows). Its 34 words have
first letter B or P, with 17 digit patterns built from vowel-separated
consonants, and it was captured
at levels 170 and 100. The fixture also retains a 16x16 word matrix with
both SOUNDEX codes. After removing the first-letter contribution, the matrix
values were identical for B/B and B/P pairs, except for six rotations such as
123/231. There, same-letter pairs scored one less.

The exact DIFFERENCE algorithm was **not** derived. The commonly published
reconstruction does not fit. That reconstruction is: 1 for matching first
letters, then +3 for a 3-digit substring, +2 for a 2-digit substring, or +1
per contained digit. It fails whichever argument supplies the needle. LCS,
greedy-subsequence and positional-window variants also fail. The
retained matrix is the acceptance data for any future model.

## Completion tokens and RPC shape

- A successful batch SELECT ends with DONE(rowCount n, more false).
- A compile-time error (8116, 174, 468) produces no metadata and one
  DONE(null).
- An sp_executesql call ends with DONEINPROC(n, more true) and
  DONEPROC(null).
- The SELECT INTO batch carries DONE(1, more true), DONE(6, more true) and
  DONE(null) around its descriptor query.
- sp_prepare returned the two-column descriptor (VarChar 5 and IntN 4) with
  DONEINPROC(0) and no rows. Each of the six sp_execute calls returned that
  descriptor and one row. sp_unprepare returned a single DONEPROC.
- Parameter values, including NULL, empty and accented values and a replay
  of the first value set, did not change the descriptors.

## Not captured

- Non-default server or database collations. Only expression and column
  collations varied. Only single probes were captured for code pages other
  than 1252: 1251 (`Cyrillic_General_CI_AS`), 1254 (`Turkish_CI_AS`) and
  UTF-8 (`Latin1_General_100_CI_AS_SC_UTF8`). Their letter sets were not
  enumerated. No other code page, including 1250 and 932, was captured.
- Supplementary-character (`_SC`) and UTF-8 behavior beyond the single probes
  above.
- SOUNDEX and DIFFERENCE over a CHAR column with more than 40 characters, and
  over CLR, spatial or hierarchyid inputs.
- Output parameters, TVPs and bulk copy, and use inside indexed views,
  constraints and partition functions.
- An index on a persisted DIFFERENCE computed column.
- Recovery after a level change (rebuilding the disabled indexes), and the
  plan-cache behavior across level changes.
- The exact DIFFERENCE algorithm (see above), and whether DIFFERENCE uses
  legacy SOUNDEX codes in every case or only in the captured ones.
- Levels below 100, which this image does not support.

## Proposed successors

**Deterministic core (msduck-core, with msduck-sql binding rules).** Scope:

- A byte-level SOUNDEX over a code-page buffer, parameterized by a
  compatibility-level mode. The legacy mode (level 100) and the current mode
  (level 110 and above) follow the numbered rules above: letter-set table,
  code-page uppercasing of the first letter, uppercase-only transparent H/W,
  first-letter duplicate suppression, stop at a non-letter, and truncate/pad.
- An explicit caller-supplied NVARCHAR-to-code-page conversion, with the
  best-fit table as an input rather than a hidden global.
- Result typing: `varchar(5)` carrying the input collation, and nullable
  `int` for DIFFERENCE.
- Argument binding: SOUNDEX rejects TEXT, NTEXT, XML and SQL_VARIANT with
  8116, while DIFFERENCE accepts TEXT. Other scalars convert to string.
  The wrong arity raises 174. Conflicting collations raise 468.
- Unit tests generated from the fixture's per-character tables (bytes 0-255
  and code points 0-591 plus extras) and the source-word table, at both levels.
- DIFFERENCE stays unsupported (an explicit error), not approximated, until a
  model reproduces the entire retained code matrix, word matrix and
  `difference columns` result.

**Root adapter (msduck).** Scope:

- Bind SOUNDEX/DIFFERENCE to the core, with the session's database
  compatibility level passed explicitly.
- Lower the functions for DuckDB through a deterministic scalar UDF. Do not
  use DuckDB's own `soundex`, whose rules differ.
- Emit the captured descriptors and DONE tokens for batch, RPC and prepared
  paths.
- Record the 3817 information messages if msduck ever models compatibility
  level changes for persisted computed columns.
- Add a differential client test that replays this fixture's batches against
  msduck. This test must not normalize the length-5 descriptor or the
  collation flags.
