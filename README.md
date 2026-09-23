# msduck

A Microsoft SQL Server compatible server written in Rust, backed by DuckDB.
**Under active development; full SQL Server compatibility is not implemented.**

## Run

Requires Rust/Cargo and a C++ compiler (DuckDB is built from bundled source).

```sh
cargo run -- --listen 127.0.0.1:1433 --database example.duckdb
```

Omit `--database` for in-memory storage. The development listener refuses
non-loopback addresses. Without `--admin-credentials`, it accepts any SQL
username/password. A [bootstrap administrator](docs/authentication.md) can be
configured over required TLS; integrated authentication is unsupported. It uses plaintext by default; configure clients
with encryption off, or supply `--tls-cert chain.pem --tls-key key.pem` to require
TLS 1.2. See [TLS transport](docs/tls.md) for certificate configuration and limits.

The SQL batch path supports basic SELECT/INSERT/UPDATE/DELETE/CREATE TABLE,
constraints, joins, nested SELECTs, multiple result sets, bracket identifiers,
Unicode strings, TOP, CROSS/OUTER APPLY, and SQL Server NULL ordering. `sp_executesql` accepts integer, bit, float, date, time, datetime2, decimal/numeric, money/smallmoney,
varchar, nvarchar and varbinary parameters, including large values and NULL. Each
client uses an independent DuckDB connection to the same database.
`ORIGINAL_LOGIN()` retains the connection’s original authenticated name across
transactions, prepared execution and credential-file rotation.

```sql
CREATE TABLE dbo.items (id INT PRIMARY KEY, name NVARCHAR(100));
INSERT INTO dbo.items VALUES (1, N'DuckDB');
SELECT TOP (10) id, name FROM dbo.items ORDER BY id;
```

## Workspace

`msduck-core` holds deterministic SQL values and rules; `msduck-tds` holds protocol
codecs. `msduck-sql` holds batch parsing and preflight, logical parameter/type adapters and
standalone AST transformations, declaration metadata rules and lexical column
lookup. Catalog type metadata uses backend-independent named fields; projection
inference and operand binding consume explicit catalog snapshots inside `msduck-sql`.
Shared expression metadata rules also live there. The root `msduck` crate owns catalog acquisition,
DuckDB, sessions and transport I/O. Run `cargo test -p msduck-core -p msduck-tds`
or `cargo test -p msduck-sql` for isolated test loops without DuckDB. See [architecture](docs/architecture.md) for
boundaries, build observations and the remaining compiler extraction work.

## Verify

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
npm ci
npm test
```

Rust integration tests use the independent tiberius SQL Server driver. Node
integration tests use tedious, following the upstream mssqlite client-testing
skill. They start ephemeral local listeners. The initial test suite is a
foundation, not proof of full SQL Server equivalence.

## Collaboration

Changes are developed through pull requests with fast deterministic-crate checks
and a separate full Rust/client/audit job. Raw CI captures are retained as
artifacts. See [the GitHub workflow](docs/github-workflow.md) and
[open issues](https://github.com/mirek/msduck/issues) for review and progress.

## Compatibility work

DML OUTPUT supports INSERT/UPDATE inserted images and DELETE deleted images,
including typed OUTPUT INTO and bound parameters in projections. Autocommit
sink failures roll back both writes. UPDATE can return paired old/new and joined
source images, including key changes, outer joins and prepared parameters.
Joined DELETE, writable derived targets, generated-column images,
partial failure streams and statement undo inside explicit transactions remain unfinished; see
[OUTPUT coverage and verification](docs/output.md).

[ROADMAP.md](ROADMAP.md) tracks the full remaining objective. Current gaps
include SQL-managed logins and permissions, advanced TLS modes, catalogs, stored procedures, savepoints, distributed
transactions, bulk load, MARS, cancellation, and substantial T-SQL semantic details.
Result metadata currently derives from DuckDB types (strings use nvarchar(max),
all columns are nullable); widths, collation, nullability, integer arithmetic,
error numbers, and statement completion behavior need SQL Server differential
validation. Character declarations currently lower to DuckDB VARCHAR without
SQL Server width/padding enforcement. Responses are buffered and capped at
16 MiB. The threaded server does not yet have connection admission limits or
a graceful shutdown API.

[Reference review](docs/reference-review.md) documents inspected mssqlite
architecture and reusable protocol/testing material. Copied skills preserve
upstream implementation notes; those notes describe mssqlite, not msduck.
`reference/mssqlite/corpus.ts` preserves upstream differential cases for the
local diagnostic harness. Run `npm run audit:local` to capture its 17 cases plus
the current focused probes in `artifacts/compatibility/local.json`. Successful
execution is not a compatibility pass. `npm run audit:compare` can compare the
same captures against an explicitly configured SQL Server endpoint using
isolated databases. `npm run audit:docker` provisions an isolated, pinned SQL
Server Developer container for the same comparison; see [reference comparison setup and limits](docs/reference-comparison.md).

Driver transaction-manager requests support begin, commit, rollback, nested
transaction counts, named rollback, and commit/rollback followed by a new
transaction. SQL batches and driver calls share state and descriptor
notifications. Stale descriptors and malformed requests are rejected before
execution. Current/read-committed/snapshot requests use DuckDB snapshot
isolation; SQL Server's locking/read-committed semantics remain to be emulated.
Other isolation levels and savepoints are explicitly rejected.

Prepared RPCs support `sp_prepare`, `sp_execute`, `sp_prepexec`, and
`sp_unprepare` by name or TDS procedure ID. Handles are connection-local;
preparation validates SELECT/DML and scalar DECLARE/SET without executing them, and executions bind
fresh values using the stored declarations. Ordinary and compound SELECT
assignments can update input bindings within an execution. Released or foreign handles return
8179. The current implementation retains SQL and declarations, recompiling at
execution rather than retaining native DuckDB plans. Prepare-time result
metadata (`sp_prepare` option 1), application OUTPUT parameters, and prepared
DDL batches remain unsupported. Supported session settings are validated without
applying them during preparation. Supported control flow is
validated without executing branches or loops. Each connection is limited to 1024
handles and 16 MiB of retained SQL/declaration text.

GENERATE_SERIES exposes `value`, with ascending/descending defaults, explicit
steps, known integer widths, exact decimal increments and correlated APPLY.
Tests cover prepared calls, empty results, view metadata, bigint/decimal
endpoints and single evaluation of inputs. Full argument coercion/diagnostics,
compatibility-level gating and decimal-path performance remain unfinished.

Decimal/numeric RPC values retain up to 38 digits through signed-magnitude TDS
decoding, DuckDB fixed-point storage and precision-sized result encoding.
Money/smallmoney inputs decode their signed scaled integers exactly. Backend
storage uses decimal(19,4)/decimal(10,4); known declared currency results retain
their four/eight-byte MONEYNTYPE metadata, including NULL and empty results.
Currency casts, style-free CONVERT and assignment paths check rounded bounds.
Character currency inputs accept documented symbols and comma separators with exact
rounding; invalid text reports 235 and MONEY text overflow reports 236.
Known currency-to-character conversions support default two-place formatting and
CONVERT styles 0/1/2 (126 aliases 2 for all character families), with currency
right-aligned in CHAR/NCHAR and
character-width validation.
Complete character syntax/diagnostic parity, full arithmetic semantics, broader
expression provenance and non-nullable fixed-type metadata remain incomplete.

Varchar RPC inputs support the advertised Windows-1252 collation, including
CP1252 punctuation, NULL, empty values and PLP large values. Other varchar
collations are rejected explicitly. Result strings still use nvarchar(max)
metadata; this does not yet implement SQL Server character widths, padding,
collation comparisons, or fixed CHAR/NCHAR inputs.

DATE RPC inputs preserve calendar days from 0001-01-01 through 9999-12-31,
including NULL, table writes and prepared execution. Malformed lengths and
out-of-range wire dates are rejected. TIME RPC inputs accept scales 0–7 and
preserve 100-nanosecond precision through TIME_NS storage, including prepared
execution. TIME casts and RPC declarations round to the requested scale.
Results currently advertise scale 7; column scale enforcement remains incomplete. Other temporal RPC inputs remain
unsupported; this does not establish full SQL Server temporal semantics.

Scalar `DECLARE` and `SET @variable = expression` support typed, batch-local
variables, including scalar subqueries and use in queries/DML. RPC inputs share
the batch scope; local values do not leak into later requests. Duplicate names
are rejected before DML execution. SELECT assignment retains the last row,
preserves the variable on an empty result, and sends no result set. All eight
compound SELECT assignment operators are accepted. Table/cursor variables remain unsupported. See
[local variable behavior](docs/local-variables.md) for validation and limits.

`IF/ELSE` and plain `BEGIN/END` blocks execute selected statements with shared
batch variables. Blocks do not start transactions. Declarations and variable
references are checked across branches before execution. See
[conditional execution](docs/conditional-execution.md) for behavior and limits.
Search conditions reject bare BIT/numeric values before execution; see
[predicate coverage](docs/predicates.md) for checked contexts and limits.
CREATE TABLE CHECK constraints validate predicate syntax and report violations
as error 547; see [CHECK coverage](docs/check-constraints.md).
IIF lowers to CASE with predicate, NULL-constant and nesting validation; see
[IIF coverage](docs/iif.md) for type-system limits.
CASE/IIF known integer and character branches use integer type precedence;
see [conditional result types](docs/case-types.md).
CHOOSE supports one-based selection and NULL bounds; see [CHOOSE coverage](docs/choose.md).
COALESCE handles typed NULLs and known integer/character precedence; see
[COALESCE coverage](docs/coalesce.md).
ISNULL preserves the first bound type, including integer replacement conversion
and width limits for known Unicode results;
see [ISNULL coverage and limits](docs/isnull.md).
NULLIF separates known integer/character and DATETIME2 comparison conversion from the first
argument's result type; see [NULLIF coverage and limits](docs/nullif.md).
Ordinary comparisons, IN lists and BETWEEN share known integer/character
precedence across predicate contexts; see [comparison conversion coverage](docs/comparison-conversion.md).
Known mixed integer/character arithmetic also uses integer precedence; see
[arithmetic conversion coverage](docs/arithmetic-conversion.md).
ABS applies SQL Server result types to known integer, BIT, REAL, DECIMAL and
money operands; see [ABS coverage and limits](docs/abs.md).
CEILING and FLOOR also preserve known numeric result types and exact BIGINT
values; see [numeric rounding coverage](docs/numeric-rounding.md).
SIGN retains known numeric result types; see [SIGN coverage](docs/sign.md).
Large integer and decimal-point literals use explicit DECIMAL types; see
[numeric literal coverage](docs/numeric-literals.md).
Known DECIMAL AVG and division inputs use exact coefficient arithmetic with
declared result scales. Nested aggregates, conditional AVG inputs and mixed
decimal/integer division have focused SQL Server comparisons; see
[decimal AVG](docs/decimal-avg.md) and [decimal division](docs/decimal-division.md)
for expression-typing and diagnostic limits.
NCHAR produces BMP characters with fixed NCHAR(1) metadata, integer conversion
and NULL/range handling. Explicit NCHAR casts truncate and pad to declared UTF-16
widths; see [coverage and limits](docs/nchar.md).
UNICODE returns the first UTF-16 unit with INT metadata under current non-SC
semantics; see [UNICODE coverage](docs/unicode.md).
SPACE handles count conversion, negative NULLs and the 8,000-space cap; see
[SPACE coverage and metadata limits](docs/space.md).
CHAR uses Windows-1252 byte codes and direct CHAR(1) result metadata; see
[CHAR coverage](docs/char.md).
Known CHAR/SPACE descriptors propagate through logical expressions; see
[logical text metadata](docs/logical-text-metadata.md).
Bitwise NOT preserves BIT and integer widths for bound values and columns; see
[bitwise NOT coverage](docs/bitwise-not.md).
AND/OR/XOR also support BIT and mixed integer widths through native overloads;
see [binary bitwise coverage](docs/bitwise-binary.md).
SELECT INTO creates persistent tables from bound query types with separate
creation/insertion semantics; see [SELECT INTO coverage](docs/select-into.md).
TOP limits apply independently within set-operation branches; see
[TOP coverage](docs/top-set-operations.md).
OFFSET/FETCH supports bound and expression counts with integer range checks;
see [paging coverage](docs/paging.md).

`WHILE`, `BREAK`, and `CONTINUE` support nested loops and ordinary single-statement
bodies. Batches currently have a 10,000-step interpreter limit and a shared
16 MiB response limit; limit errors leave the connection reusable. Active
cancellation remains unfinished. See [loop behavior](docs/loops.md).

`RETURN` exits the active batch, including nested loops. RPC calls expose signed
INT return status; NULL emits informational message 282 and returns zero.
Early return preserves transaction state. See [return behavior](docs/return.md)
for coverage and remaining compatibility checks.

Explicit `THROW number, message, state` preserves application error numbers and
states and sends severity 16 when unhandled. TRY/CATCH handles runtime errors,
supports nested error contexts and bare rethrow, and exposes ERROR_* functions.
Error line attribution is currently fixed at 1; see [TRY/CATCH limits](docs/try-catch.md). See [THROW behavior](docs/throw.md) for validation and limits.

`@@ERROR` exposes the previous batch statement's error number, including custom
THROW numbers. Successful statements reset it to zero; informational warnings
do not set it. See [session error state](docs/error-state.md) for remaining limits.

Compound `SET` supports arithmetic and bitwise operators plus string `+=`.
Known integer operands divide without floating-point conversion, and zero
divisors report 8134. See [compound assignment](docs/compound-assignment.md)
for coverage and remaining type-inference limits.

Legacy datetime and smalldatetime RPC inputs now decode with wire-range checks,
including prepared execution and NULLs. Values currently use microsecond
storage and datetime2 output metadata; [temporal compatibility limits](docs/legacy-datetime-rpc.md)
remain, including legacy rounding.

DATETIME2 casts and RPC parameters preserve year 1–9999, seven fractional digits,
and declared scale metadata, including NULLs and prepared execution. Time-only
text supplies the 1900-01-01 base date. Table columns,
INSERT/UPDATE assignments, defaults and ALTER COLUMN retain that precision and
round to the declared scale. DATE/TIME casts, YEAR/MONTH/DAY and EOMONTH accept these
exact values. Comparisons (including IS [NOT] DISTINCT FROM), BETWEEN, IN lists/subqueries and simple CASE normalize known DATETIME2 operands across
scales, including source columns and prepared predicates. CASE/COALESCE/IIF/CHOOSE
retain the highest known result scale; ISNULL and NULLIF retain their first argument's scale.
Set operations and VALUES combine known DATETIME2 result scales before comparison.
DATEPART extracts calendar and clock fields from exact values, including nanoseconds
and ISO weeks. SET DATEFIRST and @@DATEFIRST control session week numbering;
DATEPART integer inputs use checked day offsets from 1900-01-01.
Missing DATEPART fields on typed DATE/TIME inputs report 9810;
see [coverage and limits](docs/datepart.md).
DATENAME returns English calendar names and exact fractional text, with bounded
NVARCHAR(30) metadata for known projections; see
[DATENAME coverage](docs/datename.md).
Explicit NVARCHAR casts enforce bounded UTF-16 widths and retain known result
metadata, including NULL and empty results. See [cast coverage](docs/nvarchar.md).
Broader temporal operations remain open; see
[DATETIME2 support](docs/datetime2.md).

Uniqueidentifier RPC parameters and results retain their GUID wire type through
storage, local assignment, prepared execution, and NULL/empty results. `NEWID()`
generates GUIDs in expressions and persisted column defaults. See
[GUID support and remaining semantics](docs/guid-rpc.md).

`PRINT` emits informational messages without result rows, including variables
and catch diagnostics. See [PRINT behavior and limits](docs/print.md).

`REPLICATE` supports typed character, binary and numeric inputs, bounded whole-copy
limits, MAX results within configured memory budgets, and constant-aware result
widths. Floating-point text formatting and some companion metadata/diagnostics
remain incomplete; see [REPLICATE evidence and limits](docs/replicate.md).

`RAISERROR` delivers formatted ad-hoc INFO/ERROR messages with explicit severity
and state, SETERROR counters, nonfatal batch continuation and TRY/CATCH recovery.
Application errors preserve explicit transaction writes, and bare rethrow retains
formatted UTF-16 units. LOG, NOWAIT, fatal delivery and message catalogs remain
unfinished; see [RAISERROR behavior and evidence](docs/raiserror.md).

`CREATE VIEW`, `CREATE OR ALTER VIEW`, and `ALTER VIEW` persist translated queries, with
transactional creation and `DROP VIEW`. See [view coverage and remaining limits](docs/views.md).

`ALTER TABLE` supports adding/dropping multiple columns and changing types/nullability, including nullable and
NOT NULL defaults and `WITH VALUES` population with transactional execution.
DROP accepts T-SQL comma-separated names; mixed ADD/DROP actions are rejected. See [column alteration limits](docs/alter-table.md).

Standalone `CREATE SCHEMA` and empty-schema `DROP SCHEMA` support user namespaces
with transactional DDL and persistence. `sys.schemas`, `SCHEMA_ID` and
`SCHEMA_NAME` expose persistent schema IDs. [Ownership and catalog limits](docs/schemas.md) remain.

`TRUNCATE TABLE` supports whole-table deletion with rollback and incoming
foreign-key checks, including transactional integer IDENTITY reset.
[Self-reference, partition and other gaps](docs/truncate.md) remain.

`LEN` excludes trailing spaces, counts UTF-16 units, and retains INT/BIGINT
metadata for known bounded/MAX arguments. See [string-length limits](docs/len.md).

`LTRIM`, `RTRIM`, and `TRIM` preserve tabs and nonbreaking spaces by default,
with explicit character sets supported. See [trimming limits](docs/trim.md).

`DATEFROMPARTS` constructs native DATE values, validates calendar bounds and
supports stored defaults. See [conversion limits and evidence](docs/datefromparts.md).

Integer CAST/TRY_CAST and style-free CONVERT/TRY_CONVERT truncate numeric
fractions and preserve known money rounding. See [integer conversion](docs/integer-conversion.md).
Integer INSERT targets also convert VALUES/SELECT sources and defaults through
this path; see [INSERT conversion coverage](docs/insert-conversion.md).
Integer UPDATE assignments use it as well. Compound UPDATE operators support
typed arithmetic, bitwise operations and known-string concatenation;
see [UPDATE coverage](docs/update-conversion.md) for limits.
DELETE supports optional/two-FROM syntax, joined target aliases and CTE
completion counts; see [DELETE coverage](docs/delete.md).

FLOAT defaults to double precision; FLOAT(1–24) and REAL use single precision.
See [FLOAT precision coverage and limits](docs/float.md).

Integer IDENTITY columns now allocate persistent seeds/increments across connections,
including rollback and reopen behavior. ALTER TABLE ADD populates existing rows
transactionally. Explicit inserts and updates are checked;
see [identity coverage and remaining work](docs/identity.md).

`sys.columns` combines persistent declared table types with live nullability and
identity metadata. Views and SELECT INTO retain direct source and cast types.
See [column catalog limits](docs/columns.md).
`sys.types`, `TYPE_ID` and `TYPE_NAME` expose built-in type identities and
metadata. See [type catalog limits](docs/types.md).
`sys.tables` and `sys.views` expose user table/view objects; tables report their
persistent highest assigned column ID. See [table/view catalog limits](docs/table-catalog.md).
`sys.objects`, `OBJECT_ID`, `OBJECT_NAME` and `OBJECT_SCHEMA_NAME` expose persistent
user table/view IDs with transactional DDL. See [object catalog limits](docs/objects.md).
`COL_NAME` and core `COLUMNPROPERTY` lookups use persistent column IDs that retain
gaps after DROP COLUMN. COLUMNPROPERTY also reports declared precision, scale,
character lengths and core feature flags. See [column lookup limits](docs/columns.md).

Explicit VARCHAR/CHAR CAST/CONVERT supports bounded byte lengths, default length 30,
fixed CHAR padding and VARCHAR MAX result encoding. Known CHAR conditional and
set results use common widths before returning or deduplicating values. See [conversion limits](docs/varchar.md).

`sys.identity_columns` exposes live seed/increment/last values using integer
SQL_VARIANT payloads. `SQL_VARIANT_PROPERTY` exposes their base type, precision,
scale and lengths. Explicit integer casts unwrap integer variants with overflow
checks and TRY conversion. Integer-to-variant casts preserve base types through
SELECT INTO storage and views. See [identity catalog limits](docs/identity.md).

Declared `sql_variant` columns support integer INSERT/UPDATE assignments, defaults
and transactional ALTER conversion. Payloads other than BIT and integers remain unfinished.

Integer variant predicates, including IS [NOT] DISTINCT FROM, and ORDER BY source columns, output aliases and positions compare numeric values
across base types, including joins, resolved subqueries and DML filters.

Known integer variant conditionals preserve payload types through NULLIF, CASE,
COALESCE, ISNULL, IIF and CHOOSE, with lazy conversion of selected branches.

BIT variants retain Boolean wire values and support properties, storage, numeric
comparison and explicit conversion to BIT or integer targets.

Known integer/BIT variant results also propagate through UNION ALL and multi-row
VALUES, preserving base types and NULLs for predicates, ordering and assignments.
Integer/BIT variant INTERSECT and EXCEPT use numeric tuple comparison and NULL equality.

SELECT DISTINCT deduplicates known integer/BIT variant projections by numeric
value, retaining NULL and multi-column tuple semantics before TOP/paging.

Variant DISTINCT ordering resolves qualified wildcard and mixed projections,
including joined source identities, output alias precedence and quoted names.

UNION deduplicates integer/BIT variants numerically, including repeated NULLs,
while preserving tuple distinctions and nested UNION ALL boundaries.

Variant INTERSECT/EXCEPT retain distinct left-side representatives and names,
with compatible integer/BIT branch conversion, prepared inputs and outer paging.

COUNT/COUNT_BIG DISTINCT use numeric equality for integer/BIT variants, excluding
NULLs. APPROX_COUNT_DISTINCT rejects known variant inputs with error 8117.

Numeric aggregates SUM/AVG/STDEV/STDEVP/VAR/VARP reject known SQL_VARIANT arguments
with 8117; explicitly casting their inputs to numeric types enables aggregation.

MIN/MAX over known integer/BIT variants compare numeric values and retain the
selected payload type, including grouped queries, window frames and typed NULLs.

Window PARTITION BY uses numeric equality for known integer/BIT variants, including
mixed base types, NULL partitions, multiple keys, named windows and prepared
conditional expressions. Payloads returned by the projection retain their base types.

GROUP BY uses numeric equality for known integer/BIT variant keys and preserves a
representative variant payload. ROLLUP, CUBE and GROUPING SETS distinguish subtotal
NULLs from actual NULL groups. Repeated named parameters reuse one bound slot,
including prepared grouping expressions.

GROUPING returns TINYINT and GROUPING_ID returns INT, including empty results,
conditional type inference, variant properties and SELECT INTO catalog columns.
Grouping indicators retain argument bit order and validate their basic signatures.

Grouping constructs enforce the 4,096-set and 32-distinct-expression limits before
execution/preparation. Nested CUBE/ROLLUP inside GROUPING SETS preserve duplicate
totals and variant numeric equality.

Legacy GROUP BY WITH CUBE/WITH ROLLUP uses the same typed aggregation paths,
removes repeated grouping expressions and rejects combinations with modern
grouping constructs. Qualified duplicates, variant keys and prepared expressions
are covered.

GROUP BY ALL with an explicit column list retains groups excluded by WHERE.
Aggregates consume matching rows, with zero counts and NULL sums/extrema for
excluded groups. The source and predicate are materialized once; HAVING, prepared
parameters, joins, CTEs, grouped windows and integer/BIT variant keys are covered.

Known SELECT aliases cannot stand in for source columns in WHERE, GROUP BY or
HAVING (207). Real source columns with the same name, derived/CTE columns and
ORDER BY aliases remain valid.

Aggregate calls and subqueries inside GROUP BY expressions are rejected before
execution/preparation with error 144 (severity 15), including nested expressions,
grouping sets, legacy modifiers and GROUP BY ALL.

Constant-only GROUP BY keys (including numeric literals and parameters) return
error 164, severity 15. Empty grouping sets remain valid, as do expressions over
source columns whose values happen to be constant.

In expression subqueries, GROUP BY keys resolved exclusively to enclosing queries
also return 164. Local columns take precedence over outer names, and a grouping expression
may combine local and outer columns. Unknown bindings remain for name resolution.

Parenthesized branches in correlated UNION/INTERSECT/EXCEPT queries retain their
outer grouping scope. Scalar subqueries may start with a parenthesized set branch;
ordinary arithmetic around scalar subqueries remains supported.

Grouping validation inside CROSS/OUTER APPLY sees the left-hand input and enclosing
query scopes. Outer-only keys return 164; local shadowing, mixed local/outer keys,
chained APPLY inputs and OUTER APPLY's empty-result NULL row are covered.

Parenthesized join trees preserve source names and types for aggregate metadata,
variant grouping and alias checks. APPLY lowering and left-input scope tracking
also traverse nested join trees without removing their parentheses.

Joined APPLY right inputs propagate the external left-input scope through nested
join constituents. Outer-only grouping keys return 164, including in chained
inner APPLY queries. Duplicate right rows, OUTER NULL extension and prepared
parameter rebinding are covered.

JOIN ON subqueries use the current join inputs for grouping validation. Later
join sources cannot hide outer-only grouping keys through name collisions or be
mistaken for visible outer columns. WHERE returns to the full SELECT scope.

UPDATE/DELETE retain original JOIN scopes through semantic binding before target
lowering. Invalid outer-only grouping keys reject without writes, and known
out-of-scope qualified ON references report 4104. Prepared valid updates retain
parameter rebinding.

Unqualified ON-expression columns retain their resolved qualifiers through DML
lowering, including correlated grouping, filters and sort expressions. Datepart
units and standalone ORDER BY output aliases keep their syntax and precedence.

DATEADD supports typed DATE inputs for year, quarter, month, week, day,
weekday and dayofyear (including aliases), preserving DATE result metadata.
Month changes clamp to the target month's last day; INT offsets truncate
fractions and calendar overflow reports 517. Subday DATE parts report 9810.
DATETIME2 inputs retain scales 0–7 and exact 100ns precision, including subday
units and rounded nanosecond offsets. Other temporal families and string-literal
DATETIME return semantics remain unsupported.

Temporal datepart keywords retain their syntax during type annotation, including
when DATEADD/DATEPART/DATENAME appear inside aggregates, CASE and predicates and
a source column has the same name as the keyword.

DATEDIFF and DATEDIFF_BIG count calendar/time boundaries with exact 100ns
precision, Sunday weeks independent of DATEFIRST, INT/BIGINT metadata, NULLs
and checked overflow (535). DATE, DATETIME2, TIME, ISO text and integral day
offsets are supported; complete legacy coercion and timezone semantics remain open.

DATETIMEOFFSET retains UTC ticks, offsets and scales 0–7 through casts, TRY
conversions, RPCs, variables, columns, defaults and assignments. Comparisons use
UTC instants, including GROUP BY keys, grouping-set subtotals, DISTINCT and
UNION/INTERSECT/EXCEPT. Set branches normalize DATETIMEOFFSET scales while
UNION ALL preserves duplicates. CASE/COALESCE/IIF/CHOOSE normalize offset scales;
ISNULL retains the first argument’s type and scale. DATE/TIME/DATETIME2 casts and assignments
retain local components, as do YEAR/MONTH/DAY and DATEPART/DATENAME. DATEPART
tzoffset returns signed minutes; DATENAME returns a signed HH:MM offset.
DATEDIFF/DATEDIFF_BIG use exact UTC boundary counts. Full mixed temporal coercion
and date function integration remain unfinished.
DATEADD supports DATETIMEOFFSET calendar and subday arithmetic while retaining
the fixed offset and scale and validating both local and UTC ranges.

DATEADD also supports TIME hour-to-nanosecond arithmetic, retaining declared
scale and wrapping across midnight. Calendar units on TIME report 9810.

TIME scales also survive MIN/MAX and FIRST_VALUE/LAST_VALUE/LAG/LEAD results,
including known views, empty results and composed DATEADD expressions.
LAG/LEAD defaults convert to the first argument's TIME scale before evaluation
by outer expressions.

UNION/INTERSECT/EXCEPT branches with known TIME declarations retain their common
maximum scale through derived tables, CTEs, views and empty results. Aggregates,
DATEADD and window defaults consume that scale when composed over these sets.
Mixed TIME/non-TIME set coercion and live reference comparison remain open.

LAG/LEAD DATETIME2 and DATETIMEOFFSET defaults also convert to the input's scale,
including ISO text and different-scale temporal defaults, while preserving
DATETIMEOFFSET offsets.

SWITCHOFFSET preserves DATETIMEOFFSET UTC ticks and scale while applying a new
fixed offset. It accepts signed integer minutes or ±HH:MM text, supports typed
NULL/empty results, and checks invalid offsets and local-range overflow.

TODATETIMEOFFSET attaches a fixed offset to local DATETIME2 fields, preserving
scale and checking the resulting UTC range. Integer minutes and ±HH:MM text
share SWITCHOFFSET's validation, including NULLs and errors 9812/9813.

TIMEFROMPARTS constructs exact TIME(0–7) values from integer components and a
constant precision expression. Components use the integer conversion path; native range
validation reports error 289 and NULL components produce typed NULLs. Tests cover
prepared calls, defaults/storage, views, set composition and empty metadata.
See [TIMEFROMPARTS](docs/timefromparts.md) for reference and remaining limits.

DATETIME2FROMPARTS constructs exact scaled values with Gregorian date validation,
NULL propagation and checked integer components. Supported constant precision
expressions resolve before execution; values retain scale through storage,
views, prepared calls and temporal composition. See
[constructor notes](docs/datetime2fromparts.md) for remaining compatibility limits.

TIMEFROMPARTS and DATETIME2FROMPARTS share checked precision-expression
evaluation, including arithmetic and bitwise integer constants. Unsupported
expressions report 10760 without executing their contents; additional forms and
exact diagnostic precedence remain open.

DATETIMEOFFSETFROMPARTS constructs exact scaled values with checked local calendar
fields and signed offset components, including negative subhour offsets. It
validates both local and UTC ranges and retains scale through storage, prepared
calls, views and temporal/set composition. See
[constructor notes](docs/datetimeoffsetfromparts.md) for coverage and open details.

ISJSON validates JSON syntax with SQL Server's default object/array root rule and
VALUE/ARRAY/OBJECT/SCALAR constraints, returning INT or typed NULL. It supports
prepared text, long Unicode documents, duplicate keys, predicates and CHECK
constraints. See [JSON validation](docs/isjson.md) for coverage and remaining limits.

STRING_ESCAPE applies JSON special/control-character escaping with NVARCHAR(MAX)
results. Prepared calls, long outputs, NULLs, stored defaults, views and LEN/
DATALENGTH metadata are covered. See [escaping rules and limits](docs/string-escape.md).

JSON_PATH_EXISTS tests property, index and array-wildcard paths, distinguishing
JSON null from a missing value and retaining INT metadata through prepared/stored
queries and empty results. See [path existence](docs/json-path-exists.md) for
coverage and remaining comparison work.

JSON_VALUE and JSON_QUERY support lax/strict property and array-index paths,
first-match duplicate keys, decoded strings, lexical numbers and unchanged
container fragments. JSON_VALUE enforces its 4000 UTF-16-unit limit and result
metadata; JSON_QUERY returns NVARCHAR(MAX). See
[JSON extraction](docs/json-extraction.md) for tests and remaining differences.

JSON extraction now stops after a matching value or fragment, while validating
preceding JSON and scanning the whole document when a path is missing. This
allows an early match before an unrelated malformed suffix; ISJSON still rejects
the full malformed document.

JSON extraction diagnostics now preserve their error state and canonical message
through wire errors, TRY/CATCH and bare rethrow, including prepared calls.

OPENJSON default-schema rows now expose typed key/value/type columns, preserving
duplicate properties, lexical numbers and original container fragments. Literal
paths, prepared JSON input, CROSS/OUTER APPLY, views and empty metadata are covered.
Complete type conversion and collation fidelity remain unfinished; see
[OPENJSON notes](docs/openjson.md). INSERT also accepts T-SQL's optional INTO keyword.

OPENJSON WITH now maps named or explicit column paths to declared types and
preserves AS JSON fragments. Tests cover bounded/long strings, numeric and
temporal conversions, nested APPLY, prepared recovery and empty/view metadata.
Binary Base64 conversion, complete coercion and collation behavior remain open;
see [OPENJSON notes](docs/openjson.md).

OPENJSON top-level paths accept local variables and bound parameters, including
prepared re-execution with changed or NULL paths. Undeclared-variable checks and
view restrictions also cover path variables; WITH column paths remain literals.

OPENJSON binary schemas now decode Base64 strings, preserve VARBINARY lengths,
pad BINARY values, and report malformed input or width overflow. Bounded binary
wire metadata survives empty results and views. Non-string binary coercion and
exact Base64 acceptance still need reference comparison.

OPENJSON WITH character declarations now default omitted lengths to 1 and retain
VARCHAR/CHAR/NVARCHAR/NCHAR result families and widths through views and empty
results. Ordinary CAST/CONVERT defaults remain 30.

Character INSERT/UPDATE targets now enforce declared VARCHAR/CHAR/NVARCHAR/NCHAR
widths, pad fixed-width values, and reject excess non-space text atomically.
Defaults and ALTER COLUMN use the same conversion, with restart tests for stored
declarations. See [character storage](docs/character-storage.md) for limits.

DATALENGTH now counts declared character/binary bytes, including trailing spaces,
and preserves INT/BIGINT MAX metadata through catalog-backed queries. BIT and
integer widths are supported; other storage representations and general
expression inference remain open. See [DATALENGTH](docs/datalength.md).

LEN now preserves BIGINT for catalog-backed MAX inputs. LEN and DATALENGTH
retain character families through nested UPPER/LOWER and LTRIM/RTRIM/TRIM,
including derived queries, views, prepared execution and empty results.

DATALENGTH also supports DECIMAL/NUMERIC precision bands, MONEY/SMALLMONEY and
REAL/FLOAT widths, including numeric literals and catalog-backed declarations.
Temporal and SQL_VARIANT representations and general expression inference remain
open.

DATALENGTH supports DATE, legacy datetime types and declared TIME/DATETIME2/
DATETIMEOFFSET scales, including temporal constructors and CAST/CONVERT.
SQL_VARIANT widths and general computed-expression inference remain open.

Character storage and CAST rules now run in `msduck-core`, using validated family/
length types and typed errors. TDS and SQL adapters share the core's Windows-1252
codec; DuckDB vector handling remains outside the core.

RPC parameters and local variables now carry core scalar values, including exact
decimals with retained precision/scale. DuckDB conversion stays in the execution
adapter. Parameter declarations now use core logical SQL types, with explicit
length/precision/scale defaults and validation. Omitted character lengths in
DECLARE use 1; fixed CHAR/NCHAR assignments retain padding. Compiler/catalog
extraction remains in progress; standalone syntax and AST passes now live in
`msduck-sql`.

SQL runtime error identity now lives in `msduck-core::diagnostic::SqlError`. JSON
classifiers, TRY/CATCH, THROW and the TDS encoder share its number, state and
message; backend wrapper recognition and severity policy remain in adapters.

Top-level `SELECT ... FOR JSON PATH` now supports nested aliases, bound stars,
ROOT, INCLUDE_NULL_VALUES, WITHOUT_ARRAY_WRAPPER and direct JSON_QUERY promotion.
Typed serialization preserves integer/decimal precision, binary Base64, known
currency numbers and temporal values; prepared execution and multi-batch results
are covered. Nested/correlated PATH queries now also work as scalar expressions,
including prepared queries, variables, INSERT and views. Nested arrays are embedded;
WITHOUT_ARRAY_WRAPPER output stays text unless promoted by JSON_QUERY. AUTO,
complete correlated/recursive catalog and expression provenance and SQL Server row
chunking remain open. Inherited nonrecursive CTE stars now retain renamed columns,
MONEY numbers and TIME scale in nested JSON. Correlated named-column references
and qualified stars retain those logical types through enclosing row scopes, with
local shadowing, fragment provenance and prepared reuse covered. See [FOR JSON details](docs/for-json.md).

JSON_QUERY and array-wrapped FOR JSON fragments retain promotion through direct
derived-table/CTE columns, aliases, stars and correlated references. Persisted text
and WITHOUT_ARRAY_WRAPPER remain text unless explicitly promoted. See
[fragment provenance limits](docs/for-json.md#forwarded-json-fragments).

Recursive CTE execution now supports anchor/UNION ALL definitions with direct
self references, including hierarchy joins, multiple recursive members, known
stars and prepared parameters. An internal depth column enforces the default
100 recursive steps and is excluded from visible columns. Exhaustion reports
530, is catchable and rolls back a failed INSERT. Known anchor/member metadata
mismatches report 240. OPTION (MAXRECURSION n) supports limits 0–32767; zero removes the limit, and an
omitted hint uses 100. SELECT and CTE-prefixed INSERT/UPDATE/DELETE are covered.
Known anchor types now survive recursive binding and later CTE/derived projections,
including VARCHAR/NVARCHAR concatenation, prepared reuse and empty result widths.
Multiple same-encoding character anchors now combine fixed/varying widths and MAX.
Complete recursive type/metadata inference, general common types across anchors,
all recursive-member restrictions and broader query-hint combinations remain unfinished.


Result metadata now carries declared nullability and column provenance through
catalog snapshots and deterministic SQL projection inference. TDS flags distinguish
computed expressions, stored and identity columns, and derived results, including
outer-join NULL extension and empty results. Unclassified functions and persisted
view origins remain conservative. Fixed non-null scalar encodings and complete
width/collation fidelity remain unfinished; see [result metadata](docs/result-metadata.md).

Optional Linux build/test offloading is available through `npm run remote -- fast`
and the other actions in [remote build setup](docs/remote-build.md). Host, isolated
workspace and job count are configured in a gitignored `.env`.
