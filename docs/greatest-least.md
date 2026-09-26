# GREATEST and LEAST reference contract

The retained fixture in reference/greatest-least.json contains 221 SQL Server
programs. Each was captured in two fresh databases in each of two
independent containers. Both containers used the pinned SQL Server 2025 image
digest 86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a
(ProductMajorVersion 17, server and database collation
SQL_Latin1_General_CP1_CI_AS). The four raw captures matched exactly. They
retain result rows, TDS column descriptors, error number/state/class/text,
information messages and DONE tokens. Most projections are also described
through sys.dm_exec_describe_first_result_set, which records the declared
type name, precision, scale, nullability and collation.
scripts/capture-greatest-least.mjs regenerates an artifact and checks the
retained fixture. It refuses to overwrite an existing fixture.

msduck does not implement these rules yet. This document records SQL Server
behavior only.

## Values and NULLs

| Input | Captured behavior |
| --- | --- |
| Mix of NULL and non-NULL arguments | NULLs are ignored: `GREATEST(1,NULL,3)` is 3 and `LEAST` is 1. A bound NULL parameter is also ignored (`rpc int null` returns 7 and 7). |
| All arguments NULL | Returns NULL. Untyped `NULL,NULL` and a single `NULL` are typed `int`. |
| Ties | Both functions return the first tied argument as written. `'a','A'` returns `a` from both functions and `'A','a'` returns `A`. `'a ','a'` returns `a ` and `'a','a '` returns `a`. Equal DATETIMEOFFSET instants return the first argument with its own offset. |
| Evaluation errors | Every argument is evaluated. `1,1/0` and `NULL,1/0` both send the column descriptor, then error 8134 without a row, then a DONE with no row count. |
| Arity | 1 to 254 arguments succeed. Zero or 255 arguments raise error 189 (class 15, state 1): "The greatest function requires 1 to 254 arguments." (or "least"). The describe DMV reports this as a SYNTAX error. |

## Result type

The result type follows data type precedence across all arguments,
including NULL-literal arguments typed as int:

- **Integers:** tinyint with smallint returns smallint. int with bigint
  returns bigint. bit with int returns int. bit with bit returns bit, and
  tinyint with tinyint returns tinyint.
- **Exact numerics:** the result is decimal/numeric with integer digits
  max(p-s) and scale max(s). When that exceeds 38, precision is capped at
  38 and scale is reduced so the integer digits survive:
  - (38,0) with (38,10) returns decimal(38,0).
  - An integer literal with decimal(38,38) returns decimal(38,37).
  - Integer literals count their digits: `7` with decimal(5,2) returns
    decimal(5,2).
  - Typed int counts as 10 digits and bigint as 19: CAST(1 AS INT) or
    @a int with decimal(10,2) returns decimal(12,2), and bigint with
    decimal(5,2) returns decimal(21,2).
  - Decimal literals `1.5,2.25` return numeric(3,2), and numeric(4,1)
    with decimal(4,1) returns numeric(4,1).
  - Values are converted to the result scale by rounding half away from
    zero: 0.5 becomes 1 and -0.5 becomes -1 at scale 0. So
    `LEAST(CAST(99…9 AS DECIMAL(38,0)),CAST(0.5 AS DECIMAL(38,10)))` returns
    1, not 0.5.
- **Approximate numerics:** any float argument returns float. real with int
  returns real (FloatN length 4), and real with float returns float.
- **Money:** int with money returns money and smallmoney with money returns
  money. smallmoney with int returns smallmoney. money with decimal(10,2)
  returns decimal(19,4), and money with float returns float.
- **Character types:** the higher-precedence type wins, with a width equal
  to the maximum character length:
  - varchar(10) with nvarchar(3) returns nvarchar(10), and a bound
    varchar(10) with nvarchar(20) returns nvarchar(20).
  - char(3) with varchar(5) returns varchar(5), and its value keeps the
    CHAR padding (`ab `).
  - char(3) with char(5) returns char(5), and nchar(2) with char(6) returns
    nchar(6). Values are padded to the result width.
  - Literals `'abc','b'` return varchar(3). `'',NULL` returns varchar(1)
    with the empty string.
  - The comparison uses the result collation: case-insensitive under the
    default collation.
- **MAX types:** varchar(MAX) returns varchar(8000), nvarchar(MAX) returns
  nvarchar(4000) (TDS length 8000), and varbinary(MAX) returns
  varbinary(8000). A 9000-byte varchar(MAX) or 10000-byte nvarchar(MAX)
  argument raises error 8152 state 10 ("String or binary data would be
  truncated."). The error follows the column descriptor, even inside
  DATALENGTH.
- **Character against other families:** character arguments are converted
  to the other type:
  - `5,'10'` returns int 10 and 5.
  - `5,'abc'` sends the int descriptor, then error 245.
  - Strings with decimal(3,1) return decimal(3,1), and with float return
    float.
  - Date strings with date return date, in either argument order.
  - A uniqueidentifier string with uniqueidentifier returns
    uniqueidentifier.
  - In a prepared call, varchar 'x' against int/decimal parameters raises
    error 8114 state 5.
- **Date and time:**
  - date with datetime returns datetime, and smalldatetime with datetime
    returns datetime.
  - date with datetime2(3) returns datetime2(3), and datetime2(3) with
    datetime2(7) returns datetime2(7).
  - datetime with datetime2(7) returns datetime2(7), with the datetime
    value widened (`00:00:00.0033333`).
  - datetimeoffset(3) with datetimeoffset(0) or datetime2(0) returns
    datetimeoffset(3). The datetime2 argument is treated as +00:00 and
    comparison is by instant.
  - time(1) with time(7) returns time(7).
  - datetime with int returns datetime (int 1 becomes 1900-01-02), and time
    with datetime returns datetime.
  - date with int and time with date raise error 206 state 2 ("Operand type
    clash").
- **Accepted despite the task's expectation:** binary, varbinary,
  uniqueidentifier and sql_variant arguments are accepted.
  - binary(2) returns binary(2), and varbinary(4) returns varbinary(4) with
    byte-wise ordering. varbinary with int returns int.
  - uniqueidentifier uses SQL Server GUID ordering:
    `00000000-0000-0000-0000-000000000002` sorts after
    `01000000-0000-0000-0000-000000000000`.
  - sql_variant returns sql_variant (describe max_length 8016, TDS Variant
    length 8009). Mixed base types compare by family: int 1, varchar 'a'
    and numeric 2.5 give greatest 2.5 and least 'a'.
- **Rejected types:** xml, text, ntext and image raise error 8116 state 4
  class 16: "Argument data type xml is invalid for argument N of greatest
  function." The message names the function (`least` for LEAST) and the
  1-based argument position (argument 2 for `1,CAST('<a/>' AS XML)`).

## Nullability and descriptors

The result is nullable (describe `is_nullable` true, TDS flags 33 or 1)
whenever any argument is nullable. Every CAST expression and every nullable
column or parameter counts. Literal-only argument lists and NOT NULL
columns, including a NOT NULL column with a literal, are non-nullable. For
these, TDS sends a fixed-width type such as Int or Float with flags 32.
Nullable integer results use IntN with the family byte width. Computed
projections from columns report flags 1 rather than 33. The select into
case confirms the stored declarations: decimal(12,2) and nvarchar(3), both
nullable.

## Collation

- An explicit COLLATE argument wins over literals and implicit column
  collations. `'a' COLLATE Latin1_General_BIN,'B'` returns greatest `a`,
  and `Latin1_General_CS_AS` returns greatest `B`.
- A column's implicit collation wins over a coercible-default literal.
- Two different explicit collations, or two columns with different implicit
  collations, raise error 468 state 9: 'Cannot resolve the collation
  conflict between "Latin1_General_BIN" and "Latin1_General_CS_AS" in the
  GREATEST/LEAST operation.'
- The TDS collation carries the winning collation. The case-sensitive
  descriptor flag is set for BIN and CS results (flags 34/35).

## Contexts, RPC and prepared execution

- **Other clauses:** the functions work in WHERE, ORDER BY, over aggregates
  and nested.
- **sp_executesql RPC:** calls return doneInProc followed by doneProc. The
  type rules match ordinary batches for the int, decimal, nvarchar/int,
  varchar/nvarchar, money/float, date/datetime2 and varbinary parameter
  pairs. A replay of the int/int call is byte-identical to the first call.
- **sp_prepare/sp_execute:** a prepared `@a int,@b decimal(6,2),@c
  varchar(8)` statement returns decimal(12,2) on each execution.
  - An execution with a conversion failure sends the descriptor, then error
    8114, then only doneProc.
  - The next execution of the same handle succeeds.

## Not captured

- Non-default session settings (ANSI_WARNINGS OFF, ARITHABORT, DATEFIRST)
  and non-default database collations.
- Unicode supplementary-character or UTF-8 collation ordering, and Windows
  versus SQL collation differences beyond the BIN and CS_AS pairs.
- Spatial types, hierarchyid, CLR UDTs, rowversion, cursors and table
  types.
- Behavior over 8000 bytes except the MAX truncation error. Exact decimal
  values beyond JavaScript number precision are only exact in the text
  projections, since tedious decodes decimals to numbers.
- Execution-plan, constant-folding or short-circuit guarantees beyond the
  captured divide-by-zero cases.
- Computed columns, indexed views and check constraints using these
  functions.

## Proposed successors

Both successors are separate tasks. This capture edited no parser, engine,
catalog, metadata or client-test files.

1. **greatest-least-core (deterministic core):** add a pure
   msduck-core/msduck-sql rule that takes the declared argument types
   (family, precision, scale, length, nullability, collation and
   derivation) and returns either the result declaration or the exact
   captured error. The rule covers:
   - arity 1 to 254 (189)
   - precedence and width selection, including the decimal cap and scale
     reduction
   - demotion of MAX to 8000 bytes
   - nullability
   - collation resolution (468)
   - rejected types (8116 with argument position)
   - operand clashes (206)

   Add a value comparator that returns the first tied argument, ignores
   NULLs, and uses the result collation for character comparison. Use
   GUID byte ordering and sql_variant family ordering. Test it with
   fixture-derived tables only.
2. **greatest-least-root (root integration):** in the root crate:
   - Lower GREATEST/LEAST to DuckDB expressions that convert each argument
     to the core-selected type before comparison, evaluate every argument
     once, and preserve first-tie selection.
   - Emit the captured TDS descriptors, including fixed-width non-nullable
     columns, nullable N-types, collation and flags.
   - Raise 8152, 8134, 245 and 8114 after the descriptor with the captured
     DONE ordering.
   - Replay this fixture's batch, sp_executesql and sp_prepare/sp_execute
     programs against msduck, preserving raw differences.

   DuckDB's own greatest/least ignore NULLs, but they do not follow SQL
   Server precedence, collation or tie rules. The spelling alone is not
   evidence of compatibility.
