# REGEXP functions reference contract

The retained fixture in reference/regexp-functions.json contains 250 SQL Server
programs:

- 211 ordinary batches: 5 setup statements, 192 cases, 13
  compatibility-level programs and a final connection reuse check.
- 34 sp_executesql RPC calls.
- 5 sp_prepare/sp_execute/sp_unprepare sequences, with 24 executions in total.

They cover REGEXP_LIKE, REGEXP_REPLACE, REGEXP_SUBSTR, REGEXP_INSTR,
REGEXP_COUNT and the table-valued REGEXP_MATCHES and REGEXP_SPLIT_TO_TABLE.
Each program was captured in two fresh databases in each of two independent
containers. Both containers used the pinned SQL Server 2025 image digest
86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a. The
environment was ProductMajorVersion 17, server and database collation
SQL_Latin1_General_CP1_CI_AS, and database compatibility level 170, the default
for a fresh database.

The four raw captures matched exactly. They retain result rows, TDS column
descriptors, error number/state/class/text, information messages and DONE
tokens. The script makes only two substitutions, both in message text: fresh
database names become `<fresh-database>` and `database ID n` becomes
`database ID <fresh-database-id>`. scripts/capture-regexp-functions.mjs
regenerates an artifact, compares all four captures and the retained fixture
using the bounded helpers in scripts/lib/reference.mjs, and refuses to
overwrite an existing fixture. `--one-database` makes a single diagnostic
capture that it does not compare.

The rules below are exactly what the fixture shows. They are not claims about
untested inputs. msduck does not implement any of these functions yet.

## Availability and compatibility level

| Form | Level 170 | Level 160 | Level 100 |
| --- | --- | --- | --- |
| REGEXP_REPLACE, REGEXP_SUBSTR, REGEXP_INSTR, REGEXP_COUNT | Available | All four available, same results | REGEXP_REPLACE and REGEXP_COUNT captured: available, same results |
| REGEXP_LIKE | Available | Error 195 state 10 class 15: "'REGEXP_LIKE' is not a recognized built-in function name." | Same error 195 |
| REGEXP_MATCHES, REGEXP_SPLIT_TO_TABLE | Available | Error 208 state 1: "Invalid object name ..." | Not captured |
| Existing CHECK constraint using REGEXP_LIKE, created at 170, used by an INSERT at 160 | Enforced: violation 547 | Error 195 ('regexp_like', lower case) followed by error 427 "Could not load the definition for constraint ID ..." | Not captured |

Returning to level 170 restores REGEXP_LIKE and the table functions. No
object named REGEXP_* appears in sys.all_objects with is_ms_shipped=1.

## Placement and arity

- REGEXP_LIKE is a predicate. It works in WHERE, in CASE WHEN, with NOT, and
  in a CHECK constraint. In a select list it gives syntax error 156 ("Incorrect
  syntax near the keyword 'REGEXP_LIKE'"). One argument or four arguments give
  syntax error 102.
- Scalar arity errors are 189 class 15, "The regexp_X function requires 2 to N
  arguments." N is 6 for REGEXP_REPLACE, 6 for REGEXP_SUBSTR, 7 for
  REGEXP_INSTR and 4 for REGEXP_COUNT.
- For the table functions, too few arguments give 313 state 3 and too many
  give 8144 state 3. Using REGEXP_MATCHES as a scalar gives 195 state 10.

These signatures are the ones the fixture exercised:

- REGEXP_LIKE(source, pattern [, flags])
- REGEXP_REPLACE(source, pattern [, replacement [, start [, occurrence [, flags]]]])
- REGEXP_SUBSTR(source, pattern [, start [, occurrence [, flags [, group]]]])
- REGEXP_INSTR(source, pattern [, start [, occurrence [, return_option [, flags [, group]]]]])
- REGEXP_COUNT(source, pattern [, start [, flags]])
- REGEXP_MATCHES(source, pattern [, flags])
- REGEXP_SPLIT_TO_TABLE(source, pattern [, flags])

## Argument types

| Argument | Captured behavior |
| --- | --- |
| Source | CHAR, VARCHAR, NCHAR, NVARCHAR and their MAX forms are accepted. INT, TEXT, NTEXT, VARBINARY, XML, UNIQUEIDENTIFIER and SQL_VARIANT give 8116 state 1 for argument 1. The table functions use state 25. |
| Pattern | VARCHAR/NVARCHAR are accepted. INT gives 8116. VARCHAR(MAX)/NVARCHAR(MAX), including an NVARCHAR parameter longer than 4000 characters, give 8116: "Argument data type VARCHAR(MAX)/NVARCHAR(MAX) is invalid for argument 2 ...". The state depends on the function: LIKE 15, COUNT 13, REPLACE 17, MATCHES 23, SPLIT 26. REPLICATE('a',9000) is a non-MAX VARCHAR and is accepted. |
| Replacement | VARCHAR and NVARCHAR, including MAX forms, are accepted. INT gives 8116 for argument 3. |
| Flags | Only CHAR/VARCHAR are accepted, including VARCHAR(8000). NVARCHAR gives 8116, "Argument data type nvarchar is invalid for argument k". This applies to literals, sp_executesql parameters and sp_prepare, where it fails the prepare with 8116 plus 8180. A VARCHAR(MAX) flags argument gives 8116 state 18. |
| Start, occurrence, return_option, group | INT, BIGINT and a numeric string such as '3' are accepted. DECIMAL (1.5, 1.9) gives 8116 "numeric". The string 'i' gives conversion error 245 after the column descriptor. BIGINT 3000000000 gives 8115 state 2 after the descriptor. |

## Flags

- The only valid flags are lower-case c, i, s and m. Any other character,
  including upper-case I, 'x' or a CHAR(3) value padded with spaces, gives
  error 19303 state 0: "Invalid flag provided. '<whole flags string>' are not
  valid flags. Only {c,i,s,m} flags are valid."
- More than 30 characters gives 19302: "Maximum of '30' flags can be
  provided. But you provided '31' flags." Thirty repeated 'i' characters are
  accepted, and an empty string is accepted.
- Matching is case-sensitive by default, whatever the collation of the source
  or pattern. CI_AS, CS_AS and BIN2 all give REGEXP_COUNT('ABC','a') = 0.
- When both c and i appear, the last one wins: 'ic' is case-sensitive and 'ci'
  is case-insensitive.
- m makes ^ and $ match at line breaks, and s makes . match \n (and each of
  CR and LF).
- Inline (?i) works. With flags 'i', inline (?-i) turns case-insensitivity off
  again.
- With i, É matches é, and [a-z] matches A but not É.

## Pattern syntax and errors

The engine behaves like RE2. The fixture captured these supported forms:
\p{L}, \p{So}, [[:alpha:]], \b, \z, \Q...\E, (?P<name>...), \x{1F600}, and
repetition bounds up to 1000 (a{0,1000}).

| Pattern | Error |
| --- | --- |
| Lookahead a(?=b) | 19300 state 1: "An invalid Pattern 'a(?=b)' was provided. Error 'invalid perl operator: (?=' occurred during evaluation of the Pattern." |
| Backreference (a)\1 | 19300, 'invalid escape sequence: \1' |
| a{2,1}, a{1001} | 19300, 'invalid repetition size: ...' |
| Leading * | 19300, 'no argument for repetition operator: *' |
| Unclosed ( | 19308 "Missing ')' in the Pattern (." The state depends on the function: REPLACE 1, MATCHES 3. |
| Unclosed [ | 19308 "Missing ']' in the Pattern [." The state depends on the function: LIKE 2, SPLIT 4. |
| Unmatched ) | 19307 "Encountered an unexpected ')' in the Pattern )." |
| Trailing backslash | 19309 "Invalid trailing backslash (\) provided at the end of the Pattern a\." |
| Supplementary character cast to VARCHAR | The pattern becomes '??' and gives 19300 'no argument for repetition operator: ??'. |

A pattern is compiled only when a row with non-NULL arguments is evaluated:

- An invalid literal pattern with a NULL source returns NULL.
- An invalid pattern over an empty table returns an empty result without an
  error.
- When a column-derived pattern is invalid only on row 2, row 1 is returned
  and then the error follows.

## Result descriptors

| Function | Captured descriptor |
| --- | --- |
| REGEXP_REPLACE | For a non-MAX source, VARCHAR(8000) or NVARCHAR(4000) (TDS length 8000) whatever the declared width. A CHAR(5) or NCHAR(5) source gives CHAR(8000) or NCHAR(8000). The type family follows the source: a VARCHAR source with an NVARCHAR replacement stays VARCHAR. A MAX source or MAX replacement gives the MAX type of the source's family. |
| REGEXP_SUBSTR | Same family and declared length as the source: literal 'abc123' is VARCHAR(6), NVARCHAR(10) is NVARCHAR(10) (length 20), MAX stays MAX, and CHAR(5) becomes VARCHAR(5). A NULL literal source gives VARCHAR(1). |
| REGEXP_INSTR, REGEXP_COUNT | INT (IntN length 4), including MAX sources and positions past 8000. There is no BIGINT selection, unlike CHARINDEX. |
| REGEXP_LIKE in CASE | INT, not nullable (flags 32). |
| REGEXP_MATCHES | match_id BIGINT, start_position INT, end_position INT, match_value, and substring_matches VARCHAR(MAX) with collation Latin1_General_100_BIN2_UTF8. match_value has the family and length of the source; a NULL literal source gives VARCHAR(8000). |
| REGEXP_SPLIT_TO_TABLE | value, with the family and length of the source (CHAR(5) stays CHAR(5), NVARCHAR MAX stays MAX), and ordinal BIGINT. |

- Scalar results carry TDS flags 33 (nullable). A source with a
  case-sensitive or BIN2 collation gives flags 35.
- The result collation is the source collation: Latin1_General_CS_AS, BIN2 and
  Japanese_CI_AS (CP932) were retained.
- Table function columns carry flags 0 (NOT NULL). match_value and value carry
  flags 1 when the source is an expression, variable, parameter or MAX value.
- Under OUTER APPLY, the column descriptors become nullable (IntN, flags 1).
- sys.dm_exec_describe_first_result_set reports:
  - is_nullable 0 for every table-function column. For example it gives
    varchar(3) for match_value over 'abc' and nvarchar(3) for value over
    N'a,b'.
  - is_nullable 1 for the scalars.
- sp_prepare responses carry the same descriptors with a DONEINPROC count 0.

## Values

### NULLs and ranges

- A NULL in any argument gives NULL, and REGEXP_LIKE is then UNKNOWN (neither
  WHEN nor WHEN NOT). The table functions then return zero rows.
- The one exception is REGEXP_INSTR with a NULL return_option, which returned
  2, not NULL.
- START must be at least 1. Otherwise the error is 19301 "'START' value should
  be greater than or equal to 1 but 'n' is provided in 'REGEXP_X' function."
  The state depends on the function: COUNT 0, REPLACE 1, INSTR 3, SUBSTR 7.
- When START is beyond the length, REPLACE returns the source unchanged,
  SUBSTR returns NULL, and INSTR and COUNT return 0.
- For REGEXP_REPLACE, OCCURRENCE 0 means every match (0 is also the default),
  n replaces only the nth match, and a negative value gives 19301 state 2
  ("greater than or equal to 0").
- For SUBSTR and INSTR, OCCURRENCE must be at least 1 (19301 state 8 or
  state 4). An occurrence beyond the number of matches gives NULL or 0.
- REGEXP_INSTR RETURN_OPTION 0 gives the match start and 1 gives the position
  after the match. Any other value gives 19301 state 6. The message says
  "greater than or equal to 0" even for 2.
- GROUP:
  - 0 means the whole match, and a group number beyond the pattern's groups
    gives NULL or 0.
  - An unmatched optional group also gives NULL or 0.
  - A negative group gives 19301: REGEXP_SUBSTR state 9 says "greater than or
    equal to 0", and REGEXP_INSTR state 5 says "greater than or equal to 1",
    although 0 is accepted.

### Replacement strings

- \1 to \9 insert groups. A missing group inserts nothing.
- \\ inserts one backslash.
- \0, $1 and & are literal.
- A trailing single backslash is kept literally.
- If the replacement is omitted, matches are deleted.

### Empty matches

| Case | Result |
| --- | --- |
| REGEXP_REPLACE('abc','','-') | 'abc' (unchanged) |
| REGEXP_REPLACE('abc','x*','-') | '-a-b-c-' |
| REGEXP_REPLACE with '^' / '$' | '-abc' / 'abc-' |
| REGEXP_REPLACE('','x*','-') / ('','','-') | '-' / '' |
| REGEXP_COUNT('abc','') / 'x*' / ('','') / ('abc','',2) | 4 / 4 / 1 / 3 |
| REGEXP_SUBSTR('abc','x*') | '' (not NULL) |
| REGEXP_INSTR('abc','x*') / return option 1 / '$' | 1 / 1 / 4 |
| REGEXP_MATCHES('abc','') | 4 rows, start/end 1,2,3,3 (the last row repeats 3), with length 0 in substring_matches |
| REGEXP_MATCHES('','x*') | 1 row, start 0 and end 0 |
| REGEXP_SPLIT_TO_TABLE('abc','') and 'x*' | One row per character: a, b, c |

### Other value rules

- Trailing spaces are significant. CHAR padding is visible: REGEXP_REPLACE on
  CHAR(5) 'abc' with ' ' gives 'abc__'. 'b$' does not match 'ab  '.
- REGEXP_REPLACE output is silently truncated to the declared result width,
  with no error or warning. REPLICATE('a',8000) with 'a'→'bb' has LEN 8000.
  NVARCHAR with 4000 characters has LEN 4000. A MAX source gives 20000
  characters.
- In MAX sources, REGEXP_INSTR returns 10001 and REGEXP_COUNT returns 10000.
- REGEXP_MATCHES:
  - end_position is inclusive (start + length - 1).
  - match_id numbers matches from 1.
  - substring_matches is a JSON array of {value,start,length} for each
    capturing group, or for the whole match when the pattern has no groups.
  - An unmatched group has null in all three fields. Named groups appear
    without their names.
  - JSON escapes ", \ and tab.
- REGEXP_SPLIT_TO_TABLE:
  - Empty fields are kept, and a leading or trailing delimiter gives '' rows.
  - An empty source gives one '' row, and no match gives one row with the
    whole source.
  - Ordinals start at 1.
- sp_executesql and prepared parameters follow the same rules. An NVARCHAR(20)
  parameter gives NVARCHAR(40 bytes) SUBSTR and match_value descriptors.

## Characters and collations

- Collation does not change matching. It affects only the result collation
  and collation conflict checks. Explicit conflicting collations give 468
  state 9, "... in the regexp_count operation."
- VARCHAR code-page characters (CHAR(233), CHAR(200), CHAR(128) matched by
  NCHAR(0x20AC)) are single characters. Positions count characters.
- With Latin1_General_100_CI_AS_SC_UTF8, 'éb' (DATALENGTH 3) gives INSTR 2,
  COUNT('.') 2 and SUBSTR('^.') 'é'. Positions are characters, not bytes.
- N'é' does not match 'e', and e plus combining U+0301 counts as two '.'
  matches.

### Supplementary characters

U+1F600 was built from NCHAR(0xD83D)+NCHAR(0xDE00), or passed as a JavaScript
string for RPC and prepared parameters. The default and
Latin1_General_100_CI_AS_SC collations behave the same.

| Case | Result |
| --- | --- |
| REGEXP_COUNT(emoji+N'b', N'.') | 2: '.' matches the whole pair |
| REGEXP_SUBSTR(emoji+N'b', N'^.') | The whole emoji (LEN 2) |
| REGEXP_INSTR(emoji+N'b', N'b') | 3: result positions are UTF-16 code units (also RPC and prepared) |
| REGEXP_INSTR(emoji+N'b', N'.', 1, 1, 1) | 3 |
| REGEXP_INSTR(N'a'+emoji+N'b', N'b', 3) / start 4 | 4 / 0: START counts code points, the result counts code units |
| REGEXP_SUBSTR(N'a'+emoji+N'b', N'.', 3) | 'b' |
| REGEXP_REPLACE(N'a'+emoji+N'b', N'.', N'x') | 'xxx' |
| REGEXP_MATCHES(N'a'+emoji+N'b', N'.') | start_position 1, 2, 3: code points, unlike REGEXP_INSTR. JSON length 1 for the emoji. |
| REGEXP_SPLIT_TO_TABLE(N'a'+emoji+N'b', emoji) | a, b |
| \x{1F600}, [emoji], literal emoji, \p{So} | Each matches once |
| Lone high surrogate NCHAR(0xD83D)+N'b' | COUNT('.') 2, INSTR('b') 2. REPLACE of 'b' returns U+FFFD followed by 'x': the lone surrogate is replaced by U+FFFD. |

An implementation must copy the mixed position units. They cannot be derived
from one counting rule.

## A NULL argument suppresses later table-function rows

This is the most important behavior the fixture records, and it is
deterministic across all four captures. After an instance of REGEXP_MATCHES
or REGEXP_SPLIT_TO_TABLE receives a NULL source, pattern or flags argument,
it returns no rows for later non-NULL arguments.

- Within one query, CROSS APPLY over rows (NULL, 'a1', 'b2') returns no rows,
  while the order ('a1', 'b2', NULL) returns both matches. A NULL pattern in
  the first row, or a NULL first row for REGEXP_SPLIT_TO_TABLE, also empties
  the result.
- A first row with no match ('zz', 'a1') does not trigger this.
- Scalar functions over the same rows are unaffected.
- The state is kept with the cached plan across executions:
  - sp_executesql with the same text: after a non-NULL execution and then a
    NULL execution, later non-NULL executions return no rows.
  - The same happens when the first execution has NULL flags, and for
    REGEXP_SPLIT_TO_TABLE when the first execution is NULL.
  - An execution that raises an error (invalid pattern) resets it, and the
    next execution returns rows again.
  - With OPTION(RECOMPILE), a NULL execution does not affect the next one.
  - The prepared handles show the same sequence: value → rows, NULL → none,
    value → none, invalid pattern → 19308, value → rows. The
    REGEXP_SPLIT_TO_TABLE handle returns no rows after a NULL execution.
- Separate statements in one batch using a variable (NULL, then a value)
  are unaffected.
- REGEXP_COUNT with the same cached NULL-first sequence is unaffected.

The fixture does not explain the mechanism. A compatible implementation must
decide whether to copy this state or report it as an explicit difference. It
must not claim equivalence silently.

## Protocol shape

The shape of a successful call depends on the statement and the protocol. The
fixture retains these shapes, and nothing else should be inferred from them:

| Successful program | Captured completion tokens |
| --- | --- |
| Batch with one SELECT, including one that returns no rows | One DONE whose count is the number of rows returned (0 for an empty or NULL-argument table function) |
| CREATE TABLE (create source, create check constraint) | One DONE without a count |
| INSERT (insert source, check constraint accepts) | One DONE whose count is the number of rows inserted (4, 2) |
| Batch of DECLARE with an initializer, SELECT, SET, SELECT (matches variable null then value) | Four DONEs with the more bit set on all but the last: count 1, 0 (the empty result), 1, 1 |
| DECLARE plus EXEC(@sql) running ALTER DATABASE ... SET COMPATIBILITY_LEVEL | DONE count 1 with the more bit set, then DONEINPROC without a count, then DONEPROC without a count; return status 0 |
| sp_executesql, and sp_execute on a prepared handle | DONEINPROC whose count is the number of rows returned, then DONEPROC without a count |
| sp_prepare | DONEINPROC count 0 after the column descriptor, then DONEPROC |
| sp_unprepare | One DONEPROC without a count |

An implementation must not add row counts to DDL, or replace the DONEPROC
sequence of an EXEC or RPC call with a batch DONE.

Failure shapes:

- Type, arity, flag and range errors on literal arguments (8116, 189, 156,
  102, 313, 8144, 19300-19309, 19301, 19302, 19303) occur before any column
  metadata. After them, a batch has a DONE without a count. The CHECK
  violation 547 and the level-160 constraint load failure (195 then 427) end
  the same way.
- Errors 245 and 8115 occur after the column descriptor, followed by a DONE
  without a count. This also happens for a column-derived pattern that is
  invalid only on row 2: that call returns row 1 before the error.
- A table function with an invalid pattern or flag, even as a literal, sends
  its column descriptor before the error.
- With sp_executesql or prepared parameters, range and pattern errors occur at
  execution. The call then returns the column descriptor, no rows, and the
  error, followed by a DONEINPROC without a count and then a DONEPROC.
- Parameter type errors (NVARCHAR flags, a MAX pattern) occur before
  metadata, with only a DONEPROC.
- A prepared handle survives execution errors: the next sp_execute returns
  rows, and sp_unprepare succeeds.
- When the prepare fails (NVARCHAR flags):
  - The retained sp_execute gives 8179 "Could not find prepared statement with
    handle 1929396226". The fixture keeps the handle value, which was the
    same in all four captures.
  - sp_unprepare gives 8179 state 8 with handle 0.
  - All three calls end with a single DONEPROC without a count.

After all programs, the session remains reusable (@@TRANCOUNT 0,
XACT_STATE 0).

## Not captured

- Other server or database default collations, and other database
  compatibility levels between 100 and 160.
- Patterns or sources with catastrophic size, the flags 'x' of other RE2
  front ends, and the RE2 program size limit.
- Many RE2 constructs beyond those listed: Unicode classes other than \p{L}
  and \p{So}, \pN, \C, possessive and lazy quantifiers, and
  (?flags:...) groups.
- REGEXP_REPLACE on MAX values larger than 20000 characters, and truncation of
  REGEXP_SUBSTR on CHAR sources beyond one case.
- Why REGEXP_INSTR returns 2 for a NULL return_option. The table-function
  NULL state beyond the sequences above: other APPLY join orders, parallel
  plans, and plan eviction.
- REGEXP_LIKE in computed columns, filtered indexes and JOIN conditions;
  SARGability and plan shapes.
- sp_describe_first_result_set and sp_describe_undeclared_parameters for
  parameterized forms.
- SET options (ANSI_NULLS, ANSI_PADDING), attention/cancel during long
  matches, and performance.
- Whether the Rust regex crate or DuckDB's RE2 produces the same error text
  and positions. Only SQL Server was observed.

## Proposed successors

1. **regexp-functions-core-v1** (deterministic core). Scope: a new msduck-core
   module, for example crates/msduck-core/src/regexp.rs, and its unit tests.
   It should hold:
   - Flag parsing: the c/i/s/m set, the last-of-c/i rule, the 30-character
     limit, and errors 19302/19303 with the whole flag string quoted.
   - Validation of start, occurrence, return_option and group, with the exact
     19301 messages and the per-function states.
   - NULL propagation, including the INSTR NULL-return_option result of 2.
   - The replacement expansion rules and empty-match iteration.
   - Position mapping: code points for START and REGEXP_MATCHES, and UTF-16
     code units for REGEXP_INSTR results.
   - Mapping from pattern errors to 19300/19307/19308/19309.

   Unit vectors can come from this fixture. RE2-equivalent matching is needed.
   Constructs that the chosen engine cannot reproduce exactly should be
   explicit unsupported results.
2. **regexp-functions-sql-v1** (msduck-sql binding). Scope: expression
   metadata, projection inference and table-function column shapes over
   explicit catalog snapshots. It should cover:
   - The result family/width rules above (REPLACE 8000/MAX, SUBSTR source
     width, INT for INSTR/COUNT) and the table-function columns and
     nullability.
   - Argument type admissibility (8116 with per-function states, VARCHAR-only
     flags, MAX pattern rejection) and arity errors (189, 102, 313, 8144,
     195, 156).
   - Collation derivation and conflict 468.
   - Level-170 gating: REGEXP_LIKE 195 and table functions 208 below 170.
3. **regexp-functions-root-v1** (root integration). Scope: backend lowering in
   the root crate, and root-side wire tests against this fixture for batch,
   sp_executesql and sp_prepare/sp_execute. It should:
   - Evaluate through the core module once per row, not through DuckDB
     regexp_* functions directly, because DuckDB's defaults for flags,
     positions and errors differ from the rules above.
   - Emit the captured descriptors and error placement (before or after
     metadata).
   - Decide whether to reproduce the table-function NULL state across rows
     and cached plans, or to report it as an explicit difference.
   - Model compatibility-level gating for existing constraints (195 + 427).

Parser, engine, catalog, metadata and client-test files were outside this
reference task's scope and were not changed.
