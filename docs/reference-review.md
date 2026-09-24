# mssqlite reference review

Inspected upstream https://github.com/mirek/mssqlite at commit
`7f71f2081602f8e3051998f5c11f058e65fe24ec` (2026-09-20).
The checkout used for inspection is `/tmp/msduck-mssqlite-reference`; it is
not a runtime dependency. Upstream package.json declares MIT, author Mirek
Rusin. Its `License.md` MIT notice is retained with the copied material; see
THIRD_PARTY_NOTICES.md.

Copied without changing the upstream content:

- `.agents/skills/tds-protocol/`: wire specification, annotated vectors,
  packet framing, login, data types, token streams, transport state machines.
- `.agents/skills/t-sql/`: language/type/conversion/statement references.
- `.agents/skills/tedious/`: real driver behavior and integration test patterns.
- `.agents/skills/sys/`: SQL Server catalog schemas and contracts.
- `reference/mssqlite/corpus.ts`, `types.ts`: differential compatibility cases.

**Implementation-status paragraphs and package links in copied skills describe
mssqlite, not msduck.** They are reference material, not evidence that msduck
implements those features. msduck's status lives in README.md and ROADMAP.md.
The SQLite/node-specific skills are not applicable to the Rust/DuckDB backend.

For the parameter boundary, reviewed `packages/tds/src/value.ts`: its wire value
union is independent of SQLite, with separate type information. Adopted that
separation, while retaining Rust integer/floating widths and exact scaled i128
decimals rather than JavaScript number coercion. The Rust core value and adapter
implementations are new. Decimal construction follows the documented
[SQL Server precision/scale limits](https://learn.microsoft.com/en-us/sql/t-sql/data-types/decimal-and-numeric-transact-sql);
it does not implement decimal arithmetic or conversion rounding.

## Architecture and implementation findings

The reference separates byte codecs, TDS, T-SQL parser, transpiler, catalog,
engine, and server. Its server dispatches login, SQL batches, RPC, transaction
manager, bulk load, and attention; MARS adds independent framing/flow control.
Its engine interprets procedural statements and lowers relational queries to
SQLite. `respond.ts` distinguishes result sets, counts, messages, and errors,
then emits DONE/DONEINPROC/DONEPROC according to batch/procedure context.

Reviewed architecture and protocol skills, packet/prelogin/login7/all-headers/
RPC codecs, result/metadata/login/environment/error token encoders,
authentication implementation, and differential harness contracts/corpus.
The architecture guide also details modules, triggers, cursors, identity,
sequence/rowversion state, catalog lifecycle, implicit conversions, collation,
character widths, JSON/XML, APPLY, PIVOT, grouping, and temporal/decimal types.
Those are separate compatibility requirements, not solved by passing SQL text
through to another database.

Directly reusable design constraints:

- Preserve positional result rows, including duplicate labels and empty-result
  metadata. Infer wire types before observing row values.
- Bound message/value lengths and reject malformed offsets and non-progressing
  length fields. Never emit a partially encoded row after a conversion error.
- Preserve RPC declared types and bind values. Do not interpolate parameters
  into SQL text. Accept sp_executesql by both numeric ID and procedure name.
- Preserve per-statement NOCOUNT and row counts. Final DONEPROC is distinct
  from intermediate DONEINPROC and return status.
- Authentication precedes session allocation. Full-session TLS needs a TDS
  handshake adapter; a raw TLS listener alone is not TDS 7.x TLS support.
- Attention after request EOM requires finishing the original response and
  sending a separate acknowledgement. Bulk IGNORE is a separate path.
- Transactions, temp objects, prepared handles, and execution settings belong
  to sessions. Identity/sequence/rowversion allocation has additional
  rollback-independent database-wide semantics.

DuckDB changes the implementation choices:

- Native schemas replace SQLite's flattened names. A database owner connection
  creates independent client connections with `try_clone`, preserving shared
  storage while isolating transactions.
- Native typed columns, decimals, dates, joins, grouping sets, and lateral
  queries reduce rewriting, but their SQL Server semantics and metadata still
  require explicit adaptation.
- Native DuckDB SQL is not a compatibility specification: COUNT width,
  integer division, rounding, collation, VARCHAR limits, error behavior,
  transaction isolation, and NULL uniqueness all require tests and adaptation.
- AST translation avoids regex replacements affecting string literals,
  comments, identifiers, or nested SELECT scopes.

## Ground truth and validation strategy

Reference differential snapshots compare ordered columns/rows, metadata,
DONE-family sequence and counts, stable error fields, transaction state, and
post-error reuse. Keep those dimensions; do not normalize away discrepancies.
The copied corpus's `differences` apply only to upstream mssqlite, and must not
be accepted automatically as msduck exceptions.

Upstream's open audit briefs concern character/catalog metadata, ORDER tokens,
RPC completions, and runtime error streams. Its tests passing do not prove
perfect SQL Server compatibility, nor should these known gaps be copied as
intended behavior.

Primary wire specification:
https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tds/b46a581a-39de-4745-b076-ec4dbb7d13ec

DuckDB Rust API:
https://docs.rs/duckdb/1.10505.0/duckdb/

## Rust implementation discoveries

- Tiberius 0.12.3 `client/connection.rs::send` copies the same packet header
  across message fragments. Accept either repeated or incrementing IDs while
  retaining type, length, EOM and total-message checks; TCP supplies ordering.
- Tiberius may use TDS NULLTYPE (`0x1f`) for NULL RPC values. The
  sp_executesql declaration string must supply the SQL type, including for
  empty/all-NULL result sets. Metadata cannot come only from runtime values.
- Tedious startup SQL omits semicolons between SET statements. sqlparser's
  standard parse_sql API requires delimiters; invoking parse_statement until
  EOF supports T-SQL's optional delimiters without text splitting.

The FOR JSON review covered `packages/transpile/src/for-json.ts` and its planner
tests. Its ordered alias tree informed `msduck-core::for_json`; the Rust core
accepts typed values instead of generating SQLite JSON SQL. It additionally
rejects reopened paths, following Microsoft's documented ordered-path constraint.
Top-level PATH execution is now connected; see [FOR JSON status](for-json.md).
The adapter review also checked upstream projection descriptors, star expansion
and explicit JSON_QUERY promotion. SQLite JSON aggregation was not copied.
Microsoft's [data type conversion table](https://learn.microsoft.com/en-us/sql/relational-databases/json/how-for-json-converts-sql-server-data-types-to-json-data-types-sql-server?view=sql-server-ver17)
and [error catalog](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-13000-to-13999?view=sql-server-ver17)
informed type categories and diagnostic numbers. Live comparison is still needed
for exact formatting, diagnostic states and chunking.

## Logical scalar type boundary

Reviewed upstream `packages/transpile/src/type.ts`, which separates SQL type
categories from SQLite affinity. The Rust core retains logical families and
validated parameters independently of parser or backend storage; no SQLite
mapping was copied. Declaration defaults and sizes were checked against
Microsoft's [character declarations](https://learn.microsoft.com/en-us/sql/t-sql/data-types/char-and-varchar-transact-sql),
[Unicode declarations](https://learn.microsoft.com/en-us/sql/t-sql/data-types/nchar-and-nvarchar-transact-sql),
[TIME precision](https://learn.microsoft.com/en-us/sql/t-sql/data-types/time-transact-sql)
and [UNIQUEIDENTIFIER storage](https://learn.microsoft.com/en-us/sql/t-sql/data-types/uniqueidentifier-transact-sql).
The added local capture preserves declaration results and wire metadata; live
SQL Server diagnostic comparison remains outstanding.

For the SQL crate extraction, reviewed upstream `packages/tsql/package.json`
and `packages/tsql/src/index.ts`: its lexer/parser/AST package is independent of
storage and runtime. The new Rust `msduck-sql` follows that dependency direction
and also houses existing deterministic AST passes. It reuses msduck's current
implementations and the pinned sqlparser dependency; no TypeScript parser code
was copied. Catalog-dependent binding and execution remain explicit root adapters.

## Nested FOR JSON execution

Revisited upstream `packages/transpile/src/for-json.ts` descriptors, source wrappers,
row aggregation and nested-query promotion. The Rust implementation uses the
same general source/row/aggregate separation with typed native values instead of
SQLite JSON constructors. Correlation stays in the database query. The upstream
`isJson` predicate promotes all FOR JSON subqueries; Microsoft documents an
exception for WITHOUT_ARRAY_WRAPPER, which Rust preserves as quoted text unless
wrapped in JSON_QUERY. See [Microsoft's common JSON issues](https://learn.microsoft.com/en-us/sql/relational-databases/json/solve-common-issues-with-json-in-sql-server?view=sql-server-ver17).
This documentation informs the tests; it is not a live SQL Server comparison.


## Inherited CTE metadata for JSON

Reviewed upstream `statement.ts` CTE rendering and `for-json.ts` source descriptors.
Its JSON star expansion requires complete source column metadata and treats an
unknown join side as unknown. Rust now propagates inherited CTE catalog snapshots
through nested JSON binding and applies the same completeness requirement rather
than treating unresolved sources as empty tables. Microsoft's
[CTE reference](https://learn.microsoft.com/en-us/sql/t-sql/queries/with-common-table-expression-transact-sql?view=sql-server-ver17)
documents declaration ordering and CTE names shadowing base objects. The new
snapshot tests cover these metadata boundaries; they do not claim complete
recursive CTE execution or canonical forward-reference diagnostics.


## Correlated JSON type scope

Reviewed upstream `context.ts` source-type stacks and innermost-first `columnType`
lookup. Rust retains the lexical stack approach but explicitly blocks outward
lookup for an unknown local type, an ambiguous name, a matching local qualifier,
or an unresolved source scope. Simply omitting unknown/ambiguous entries from a
map could otherwise expose an unrelated outer declaration. Microsoft's
[subquery column-resolution documentation](https://learn.microsoft.com/en-us/sql/relational-databases/performance/subqueries?view=sql-server-ver17)
supports implicit outer qualification when the inner sources lack the name.
The regression checks metadata and client behavior locally; it is not a live SQL
Server differential result.

## Catalog boundary extraction

Revisited upstream `packages/transpile/src/context.ts`: source descriptors supply
logical types, collations and nullability to lexical lookup without exposing SQLite
values. Rust now similarly uses backend-independent type metadata and pure
row-source lookup, while retaining its explicit unknown/ambiguity barriers. Database
reads and persistence remain root adapters. This is an architectural extraction of
existing behavior, not new SQL Server compatibility evidence.

## Explicit catalog acquisition

Reviewed upstream `packages/engine/src/execute.ts` target-column collection and
`metadata.ts` catalog lookup: the engine resolves table identities and supplies
named column/type/collation descriptors. Rust now captures type and source-column
records before projection inference, with no connection in the recursive inference
module. The snapshot test checks old/new declarations across DDL and inference
after database closure; it does not claim that all compiler catalog access has
been extracted or that concurrent DDL acquisition is atomic.

## Projection inference extraction

Revisited upstream `packages/transpile/src/for-json.ts` source descriptors. Its
star expansion consumes supplied source metadata and propagates unknown join
sides. Rust projection inference now runs in the SQL crate over explicit snapshots
with the same completeness requirement, while preserving Rust's tested lexical
shadowing barriers. Shared expression classifiers moved with it; native execution
and database/client regression tests remain in the root. This extraction preserves
existing semantics and does not establish complete SQL Server binding behavior.

## Forwarded JSON fragments

Upstream `for-json.ts` recognizes direct JSON_QUERY/FOR JSON expressions via
`isJson`; the inspected predicate does not resolve a column through source
provenance. Rust now propagates direct fragment origin through its typed projection
fields and lexical scopes. It continues to preserve Microsoft's documented
WITHOUT_ARRAY_WRAPPER exception. General expression and persisted view provenance
remain open; the added tests and audit capture are local evidence only.

## Correlated qualified stars

Reviewed upstream `for-json.ts` source aliases and column descriptor expansion.
Its helper derives stars from the current FROM source; Rust now additionally
resolves qualified stars from explicit enclosing scopes. Expansion retains
qualified column references, while local aliases and unknown/ambiguous scopes
block fallback. Regression testing exposed DuckDB's no-local-FROM star rejection,
which the pure expansion pass avoids. No SQL Server diagnostic text or precedence
claim is made for unresolved qualifiers.

## Consistent declaration snapshots

Revisited upstream `statement.ts` ordered `cteDefinitions` rendering alongside
Microsoft's [CTE visibility rules](https://learn.microsoft.com/en-us/sql/t-sql/queries/with-common-table-expression-transact-sql?view=sql-server-ver17).
Rust's visitor path already reserved declaration names, but standalone projection
inference could read a same-named base object. Both now use one declaration
snapshot routine. Regression coverage includes unresolved self/forward names,
outer-CTE shadowing, explicit schema-qualified tables and valid fragment aliases.
This does not claim complete runtime validation or recursive CTE support.

## CTE column-list cardinality

Microsoft's [CTE reference](https://learn.microsoft.com/en-us/sql/t-sql/queries/with-common-table-expression-transact-sql?view=sql-server-ver17)
requires the explicit column list to match the definition's result width. Its
[error catalog](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-8000-to-8999?view=sql-server-ver17)
lists errors 8158 and 8159 at severity 15. Local regressions cover syntax and
catalog stars, CTE chains, preparation, INSERT targets, schema changes and reuse.
State 1 and precedence among multiple errors have not been compared against a
live SQL Server. This is limited cardinality validation, not full CTE validation.

## CTE output names

Reviewed upstream `packages/transpile/src/statement.ts::cteDefinitions`, which
renders explicit column lists directly. Rust now checks unnamed and duplicate
CTE outputs before backend execution instead of accepting DuckDB-generated names.
Microsoft's [CTE rules](https://learn.microsoft.com/en-us/sql/t-sql/queries/with-common-table-expression-transact-sql?view=sql-server-ver17)
require distinct names when the explicit list is omitted. The
[error catalog](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-8000-to-8999?view=sql-server-ver17)
lists 8155/8156 at severity 15. Local tests check these identities, direct and
prepared rejection, explicit overrides, set branches, stars and recovery. State
1, precedence among multiple errors and complete identifier collation behavior
remain unverified against a live SQL Server. Derived-table naming validation is
not covered by this CTE pass.

## Duplicate CTE declarations

The upstream `cteDefinitions` renderer preserves declaration order and writes the
CTE names directly; it does not provide a reusable SQL Server duplicate-name
check. Rust now rejects repeated names before building its scope snapshots,
preventing map replacement and backend-specific diagnostics. Microsoft's
[error catalog](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-0-to-999?view=sql-server-ver17)
lists duplicate CTE declaration error 239 at severity 16. Names are compared with
the existing case-insensitive binder policy. State 1 and precedence when multiple
independent errors occur have not been verified against a live SQL Server.

## Recursive CTE structure

The copied T-SQL query skill and Microsoft's
[recursive CTE reference](https://learn.microsoft.com/en-us/sql/t-sql/queries/recursive-common-table-expression-transact-sql?view=sql-server-ver17)
define anchor members followed by recursive members joined with UNION ALL.
Microsoft's [error catalog](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-0-to-999?view=sql-server-ver17)
lists 246/247/252/253 for the structural failures now checked by the pure SQL
pass. Upstream CTE rendering forwards the definitions directly; it supplies no
reusable recursive shape validator or bounded-execution implementation.

Local tests distinguish unqualified self references from schema-qualified base
objects, table aliases and nested CTE shadowing. The former server returned base
table rows for a malformed self reference; the new pass rejects it before writes
or preparation. Error state and precedence among multiple simultaneous errors
have not been compared against SQL Server. Valid recursive execution, exact
anchor/member type matching and MAXRECURSION remain unfinished. This structural
pass must not be described as complete recursive CTE support.

## Bounded recursive execution

Microsoft's [recursive CTE semantics](https://learn.microsoft.com/en-us/sql/t-sql/queries/recursive-common-table-expression-transact-sql?view=sql-server-ver17)
describe repeated recursive members over the previous iteration and UNION ALL of
the generations. The [query-hint reference](https://learn.microsoft.com/en-us/sql/t-sql/queries/hints-transact-sql-query?view=sql-server-ver17)
defines the default recursion limit and the rollback behavior on exhaustion.
Rust now lowers common recursive definitions to a guarded native DuckDB CTE with
an internal depth column and a visible-column wrapper. The upstream renderer
has no reusable bounded-execution pass. Local regressions cover finite series,
hierarchy joins, multiple recursive members, stars, name collisions, same-named
base tables, prepared reuse, known type mismatches, exhaustion, TRY/CATCH and
INSERT rollback. No live SQL Server comparison was performed; explicit hints
and complete recursive type/restriction coverage are still required.

## MAXRECURSION query hints

The copied query skill and Microsoft's
[query-hint reference](https://learn.microsoft.com/en-us/sql/t-sql/queries/hints-transact-sql-query?view=sql-server-ver17)
define the 0–32767 range, zero as unlimited and the default of 100. Microsoft's
[error catalog](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-0-to-999?view=sql-server-ver17)
identifies an excessive value as 310, severity 15. Searching the upstream packages
found no MAXRECURSION implementation to reuse. The Rust parser now carries the
hint through AST normalization into deterministic recursive lowering. Local tests
cover custom/default boundaries, finite unlimited execution, prepared recovery,
invalid literal forms, preflight rejection, INSERT/UPDATE rollback and DELETE.
The exact state and precedence of malformed or combined hints remain unverified
against SQL Server; no live comparison was performed.

## Recursive-member restrictions

The copied T-SQL query skill lists restrictions on recursive members. Microsoft's
[CTE reference](https://learn.microsoft.com/en-us/sql/t-sql/queries/with-common-table-expression-transact-sql?view=sql-server-ver17)
and [error catalog](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-0-to-999?view=sql-server-ver17)
provide the distinction between anchor/recursive members and errors
460/461/462/465/467 at severity 16. The Rust pure pass now reports those errors
before writes or preparation. The upstream CTE renderer does not provide a
reusable validator for these restrictions. Local regressions preserve aggregates
and DISTINCT in anchors, windowed SUM in a recursive member, and connection reuse.
Error state and precedence among simultaneous failures have not been compared
against a live SQL Server; not all recursive-member restrictions are implemented.


## Recursive table-source restrictions

Microsoft's [4000–4999 error catalog](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-4000-to-4999?view=sql-server-ver17)
identifies recursive-reference hints as 4150 and recursive-member PIVOT as 4190,
both severity 16. The upstream `packages/transpile/src/statement.ts` CTE renderer
renders definitions directly; it supplies no corresponding checks to reuse.
Rust checks both in the pure SQL validator before preparation or execution.
The regression first observed a native DuckDB syntax error on `WITH (NOLOCK)`.
New client cases cover aliases, preparation, prior-write prevention, and reuse;
pure cases distinguish anchor PIVOT, hints on other tables and qualified base
references. Error precedence and state values have not been compared live.


## Recursive anchor type propagation

Microsoft's [CTE reference](https://learn.microsoft.com/en-us/sql/t-sql/queries/with-common-table-expression-transact-sql?view=sql-server-ver17)
requires recursive member columns to match the anchor types and makes recursive
outputs nullable. The local regression reproduced a DuckDB character `+` binder
failure despite explicit matching VARCHAR(5) casts. Inspection found both pure
projection and backend column inference losing recursive self types; additionally,
arithmetic lowering did not receive bound column types. The shared deterministic
anchor extraction now seeds both inference paths, and the backend binder annotates
known arithmetic operands. Tests cover VARCHAR/NVARCHAR recursion, empty result
widths, subsequent CTE and derived projections, prepared reuse, same-named base
tables, known width mismatch, and ordinary table arithmetic/conversion behavior.
The upstream CTE renderer supplies no equivalent recursive type-binding mechanism
to copy. Exact arithmetic and multi-anchor type inference still need work.


## Multiple character anchor types

Microsoft's [precision, scale and length rules](https://learn.microsoft.com/en-us/sql/t-sql/data-types/precision-scale-and-length-transact-sql?view=sql-server-ver17)
choose the greater input length for character set results. The existing local
wire-descriptor path handled several such combinations, but pure projection and
root operand inference discarded them. A shared SQL-crate rule now covers inputs
with the same encoding, fixed/varying families and MAX. It does not claim complete
[data type precedence](https://learn.microsoft.com/en-us/sql/t-sql/data-types/data-type-precedence-transact-sql?view=sql-server-ver17)
or collation-label resolution. The client regression first reproduced native
character-addition failure with VARCHAR(3)/VARCHAR(5) anchors; coverage now includes
fixed/varying pairs, Unicode, MAX, empty descriptors, prepared execution, and
recursive member width mismatch. TRIM-family character retention also participates
in scalar classification, using the already shared expression rule.


## Recursive numeric expression types

The numeric precision/scale formulas were adapted from
[`packages/transpile/src/decimal.ts` in the reviewed reference](https://github.com/mirek/mssqlite/blob/7f71f2081602f8e3051998f5c11f058e65fe24ec/packages/transpile/src/decimal.ts)
and checked against Microsoft's
[precision/scale documentation](https://learn.microsoft.com/en-us/sql/t-sql/data-types/precision-scale-and-length-transact-sql?view=sql-server-ver17).
The Rust port retains the addition/subtraction, multiplication, division and
modulo formulas and the distinct multiplication/division reduction above 38
digits. Pure tests include Microsoft's DECIMAL(30,20) multiplication result
DECIMAL(38,17) and DECIMAL(30,10) result DECIMAL(38,6).

Recursive member inference now applies numeric precedence and those formulas to
bound operands. The regression initially observed missing rejection for a
SMALLINT anchor with `n+1`. Client coverage now expects 240 for that promotion,
INT/BIGINT promotion and widened decimal/numeric arithmetic, including nested
expressions. Explicit matching casts, same-width integer operands, decimal
modulo and prepared reuse are positive cases. Failed INSERT validation leaves
the destination empty. No claim is made about complete runtime decimal arithmetic
or all recursive expression types.


## Numeric anchor sets and conversion

Microsoft's [precision/scale table](https://learn.microsoft.com/en-us/sql/t-sql/data-types/precision-scale-and-length-transact-sql?view=sql-server-ver17)
specifies decimal set-result capacity separately from binary addition. The shared
arithmetic module now exposes that rule for both catalog records and AST types.
The initial regression reproduced missing 240 for differing decimal anchors
followed by a widening recursive expression. Tests now cover matching recursive
casts, mixed integer anchors, DATALENGTH, prepared execution and persisted decimal
view metadata. The precision-reduction/deduplication case already worked under
DuckDB before explicit operand casts were added; it is retained as a regression,
not presented as a newly fixed backend defect.

A separate probe found missing currency overflow in MONEY/BIGINT UNION ALL.
Microsoft's [currency ranges](https://learn.microsoft.com/en-us/sql/t-sql/data-types/money-and-smallmoney-transact-sql?view=sql-server-ver17)
are narrower than the backing DECIMAL declarations. The set conversion path now
checks the signed scaled-integer bounds with a native scalar callback. Tests cover
both currency overflows, valid values, NULLs and 3,001 rows spanning vector chunks.
The GENERATE_SERIES fixture uses positional SELECT *; its SQL Server `value`
column name is still not mapped by the current backend path.

The currency range rule has since moved to `msduck-core::money`, using the same
Microsoft endpoints above and typed SQL error identity. The core checks exact
scale-four integers; native storage, rounding and NULL handling stay in the root.
Pure tests include each endpoint, adjacent representable units and i128 extremes.
Inspection of upstream `packages/engine/src/decimal.ts` confirms its generic
decimal formatter checks decimal precision; that alone does not enforce the
narrower currency ranges. Local standalone CAST/TRY_CAST and table-assignment
probes still admit out-of-range currency values, now recorded in ROADMAP.md.

Currency conversion integration uses Microsoft's documented
[currency ranges](https://learn.microsoft.com/en-us/sql/t-sql/data-types/money-and-smallmoney-transact-sql?view=sql-server-ver17)
and [TRY_CAST behavior](https://learn.microsoft.com/en-us/sql/t-sql/functions/try-cast-transact-sql?view=sql-server-ver17).
The regression first reproduced missing rejection for a currency cast; numeric
endpoint and rounding overflow now fail, TRY conversions return NULL, and failed
storage conversions preserve data. TRY still propagates source divide-by-zero.
The pure SQL module constructs the conversion once and the root adapter applies
the core range rule after rounding. Upstream `packages/engine/src/decimal.ts`
retains useful exact-decimal rounding logic, but its decimal precision check is
not a replacement for currency bounds.

Review of Microsoft's
[error catalog](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-0-to-999?view=sql-server-ver17)
identified distinct character-to-money errors 235 and 236. Numeric overflow tests
therefore use numeric source expressions; they do not assert that the current
8115 character-overflow diagnostic matches SQL Server. Character lexical and
source-specific diagnostic integration remains explicit in ROADMAP.md.

Currency output encoding follows MS-TDS fixed-point encoding and MONEYNTYPE
length rules, using the copied TDS skill and Microsoft's
[MS-TDS specification, sections 2.2.5.4 and 2.2.5.5.1.4](https://winprotocoldoc.z19.web.core.windows.net/MS-TDS/%5BMS-TDS%5D-200615.pdf).
Upstream `packages/tds/src/value.ts` and its moneyN round-trip tests confirm the
high-word-first layout. The Rust codec works from an exact integer coefficient
instead of copying upstream's JavaScript Number scaling, preserving endpoints.
Independent client checks include values using both words, negative smallmoney,
NULLs and empty metadata. The initial regression reproduced DecimalN output for
explicit currency casts; those declarations now emit MoneyN.

The installed tedious SMALLMONEY parameter encoder uses `writeInt32LE(value *
10000)`, which sends coefficient 214747 for JavaScript 21.4748. That source-byte
observation is retained in `/tmp/msduck-money-wire-driver.log`; the prepared
fixture now uses 21.5 while SQL literals and pure wire vectors still cover four
fractional digits. No server-side value normalization was added for this client
input behavior.


### GENERATE_SERIES review

Reviewed upstream `packages/transpile/src/table-function.ts` (generateSeries and
its APPLY route) and `packages/engine/src/udf.ts` (series_step and correlated
series), at the recorded mssqlite revision. The relational expansion informed
our lowering, but upstream's repeated argument expressions, JavaScript Number
arithmetic and 100,000-row correlated cap are not carried over. msduck uses a
materialized input projection, native integer generation and exact decimal
recursion.

Microsoft's [GENERATE_SERIES reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/generate-series-transact-sql?view=sql-server-ver17)
specifies the value column, supported numeric types, direction defaults, empty
wrong-direction results and matching start/stop types. The
[4000–4999 error catalog](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-4000-to-4999?view=sql-server-ver17)
identifies invalid argument value error 4199. Tests exercise these local
observations; live SQL Server equivalence remains unverified.

### Currency character conversion review

Revisited upstream `packages/engine/src/execute.ts` (decimalType/decimalShape),
`packages/engine/src/decimal.ts` (cast/signed/rescale), and
`packages/transpile/src/type.ts` at the recorded mssqlite revision. Upstream maps
currency declarations to decimal shapes and uses its shared exact decimal parser.
That architecture supplies useful exact-rounding patterns, but it does not supply
SQL Server's distinct money text syntax and diagnostics; those rules now have a
separate deterministic implementation in `msduck-core::money::parse_text`.

Microsoft's [money types reference](https://learn.microsoft.com/en-us/sql/t-sql/data-types/money-and-smallmoney-transact-sql?view=sql-server-ver17)
provides the range and currency-symbol list. Its
[constants reference](https://learn.microsoft.com/en-us/sql/t-sql/data-types/constants-transact-sql?view=sql-server-ver17)
explicitly permits commas anywhere in converted money strings, without grouping
validation, and shows a currency prefix before the sign. The
[character conversion reference](https://learn.microsoft.com/en-us/sql/t-sql/data-types/char-and-varchar-transact-sql?view=sql-server-ver17)
distinguishes exact-numeric text from exponent notation. The
[conversion reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/cast-and-convert-transact-sql?view=sql-server-ver17)
describes rounding to fewer fractional places. The
[error catalog](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-0-to-999?view=sql-server-ver17)
identifies 235/236 for invalid money text and overflow.

Before this change, local probes returned error 245 for both a valid currency
prefix/comma string and invalid text, and 8115 for character-to-money overflow.
The new client checks cover the corresponding values and diagnostics, typed
NULL/empty metadata, exact endpoints, excess fractional digits, leading zeros,
prepared reuse and storage atomicity. This is documented-rule implementation
with local evidence, not a live SQL Server comparison. Less-documented empty,
sign-only, whitespace and punctuation cases, narrowing to SMALLMONEY and error
state/precedence need reference validation. Numeric conversion remains a separate
path; neither explicit styles nor full currency arithmetic is completed here.

### Currency character output review

Reviewed upstream `packages/transpile/src/expression.ts` (cast/sourceType routing)
and `packages/transpile/src/functions.ts` (convertStyles). The source-type lookup
is useful precedent, but the shared style table is for datetime `strftime`
formats. Applying it to money would misinterpret style 126. msduck now has a
currency-specific pure SQL plan and exact integer formatter in the core.

Microsoft's [CAST/CONVERT reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/cast-and-convert-transact-sql?view=sql-server-ver17)
defines money styles 0 (two places), 1 (grouped two places), 2 (four places),
126 (equivalent to 2 for CHAR/VARCHAR), and fallback to 0. It also requires
errors when numeric display cannot fit a character target. Native adapters use
existing character numeric-width handling; exact money-specific short-target
error numbers/states remain unverified. The pure formatter covers both i64
endpoints and rounding that carries into another whole digit.

Baseline probes showed default CAST returning four places and style 1 failing
in DuckDB's parser. Client tests now cover explicit/TRY conversion, dynamic styles,
MONEY RPC inputs, same-storage DECIMAL distinctions, catalog columns, CTEs,
derived/correlated queries, views, fixed padding, NULL and empty result metadata.
Native sequence tests verify a single value/style evaluation per row across
6,000 rows. No live SQL Server comparison was performed. Full inference through
conditional/arithmetic/aggregate currency expressions, signed-zero display,
less-documented style interactions and diagnostic parity remain follow-up work.

### Conditional currency identity

Revisited `packages/transpile/src/implicit.ts` in mssqlite: CASE/COALESCE/IIF/CHOOSE
select a common result type, while ISNULL and NULLIF retain the first argument's
type (with ISNULL's literal-NULL exception). msduck's new pure currency inference
reuses its existing argument validators and numeric precedence/shape rules rather
than treating every decimal-backed expression as money. Unknown branches prevent
common-type inference; DECIMAL/FLOAT precedence prevents a money result claim.

The rule follows Microsoft's [CASE return types](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/case-transact-sql?view=sql-server-ver17)
and [ISNULL return types](https://learn.microsoft.com/en-us/sql/t-sql/functions/isnull-transact-sql?view=sql-server-ver17).
Before this change, a style-1 conversion of a MONEY/SMALLMONEY CASE failed in
DuckDB's parser, and a MONEY/INT CASE was exposed as DecimalN. Tests now cover
formatting and MoneyN descriptors for these expressions, first-argument identity,
empty results, parameters, bound columns and a later CTE projection. Formatting
checks the inferred currency range before display-width conversion, including
SMALLMONEY chosen from an integer replacement. This does not complete general
conditional value coercion, nonnumeric mixed branches, aggregate/arithmetic
currency inference, or live SQL Server comparison.

### Conditional currency value conversions

Revisited mssqlite `transpile/src/expression.ts` (CASE result coercion and ISNULL
character handling) and `transpile/src/implicit.ts` (coerce/compatible). The useful
pattern is conversion of result operands before backend type unification. msduck
now applies that pattern to known currency CASE/COALESCE/IIF/CHOOSE results and
ISNULL replacements through logical currency casts, reusing the exact conversion
callbacks. NULLIF comparison coercion is left separate from its return type.

Microsoft's [type precedence](https://learn.microsoft.com/en-us/sql/t-sql/data-types/data-type-precedence-transact-sql?view=sql-server-ver17)
and [ISNULL conversion rule](https://learn.microsoft.com/en-us/sql/t-sql/functions/isnull-transact-sql?view=sql-server-ver17)
require lower-precedence character operands and replacements to convert to the
currency result type. Before this change, ISNULL/CASE currency strings failed
with 245 through DuckDB decimal conversion, and an overflowing SMALLMONEY
replacement could enter a predicate without an error. The new tests check exact
rounding, 235/236 text errors, 8115 narrowing errors before predicate evaluation,
unselected invalid branches, NULL/empty metadata and atomic INSERT failure.

A native test exposed missing classification of a known character concatenation;
the currency inference now tracks character category without inventing widths.
A client test exposed missing character types in VALUES-derived columns; the
operand binder now merges known character declarations using the existing shared
rule and skips untyped NULLs. Unknown or mixed unsupported sources stay unknown.
Native sequence counts verify 6,000 conditions and 3,000 selected producers.
These observations do not prove all SQL Server evaluation-order or conversion
semantics, especially general COALESCE/subquery reevaluation and NULLIF comparison.

### Batch parser extraction

Revisited mssqlite `packages/tsql/src/parse.ts` and the `batch` parser in
`parse/statement.ts` at the recorded reference revision. Their parser consumes
optional semicolons and delegates actual statement boundaries to the grammar,
independently of the engine. msduck already followed that approach; the existing
pipeline now lives in `msduck-sql::batch` with its syntax-only helpers and RPC
parameter declarations. This preserves behavior and makes parser/declaration
checks runnable without DuckDB. The root retains session preflight and execution;
upstream module-definition handling is not an implementation claim for msduck.

### Explicit batch preflight inputs

Revisited mssqlite `packages/engine/src/bind.ts`: ordinary variable binding reads
the session map, while global binding reads session/server state. The copied
T-SQL language guide also records batch scope across BEGIN/END. msduck's existing
preflight traversal has now moved into the SQL crate with an explicit parameter
map. It validates declarations/references and constructs typed NULL bindings;
actual values, globals and initializer evaluation remain in execution. Existing
view and DDL structural checks moved with it without changing traversal order.
This extraction does not add the upstream engine's broader global-variable or
module scope support.

### NULLIF currency comparison conversion

Revisited mssqlite `packages/transpile/src/implicit.ts`: NULLIF retains the first
argument's type, separately from common-result functions. Microsoft's
[NULLIF specification](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/nullif-transact-sql?view=sql-server-ver17)
and [type precedence](https://learn.microsoft.com/en-us/sql/t-sql/data-types/data-type-precedence-transact-sql?view=sql-server-ver17)
supply the comparison/result distinction. Before this change, NULLIF(MONEY,'$12')
and the reversed character/currency order failed with DuckDB decimal conversion
error 245, while NULLIF(SMALLMONEY,214749) returned 1 without narrowing overflow.

The pure currency comparison rule now inserts logical casts on the equality
operands of the existing CASE lowering, leaving the first result operand intact.
DECIMAL/FLOAT precedence and unknown operands block currency narrowing. Existing
matching TRY casts remain intact. Projection inference also retains the first
argument's declaration, including character literals and bound column widths;
a focused client assertion exposed that missing metadata rule.

Tests cover equality, rounding, unequal text preservation, SMALLMONEY overflow,
235/236 text diagnostics, first-result metadata with empty rows, prepared reuse,
CTE/table sources and atomic INSERT failure. NULLIF retains its existing CASE
evaluation model; this does not promise one evaluation of its first expression.
General binary currency comparisons and other mixed-type coercions remain open.

### Currency binary predicate coercion

Revisited mssqlite `packages/transpile/src/expression.ts` `binaryOp`: it obtains
common operand types before rendering coercions, with specialized exact numeric
handling. Microsoft's [type precedence](https://learn.microsoft.com/en-us/sql/t-sql/data-types/data-type-precedence-transact-sql?view=sql-server-ver17)
places currency above integer and character types and below DECIMAL/FLOAT. The
local baseline rejected MONEY='$12' with DuckDB error 245 and evaluated
SMALLMONEY<214749 without currency range validation.

The SQL comparison pass now applies known currency precedence to the six binary
comparison operators and IS [NOT] DISTINCT FROM. It uses the same logical cast
helper as conditional results and NULLIF; native conversion still performs exact
rounding and range checks. Tests cover both operand orders, NULLs, matching TRY
casts, prepared parameters, joins, CTEs, correlated predicates and DML atomicity.
A native sequence test checks each operand's evaluation count over 6,000 rows.
Unknown operands and higher-precedence numeric types remain outside currency
narrowing. BETWEEN, IN/subqueries, simple CASE comparisons and full arithmetic
currency inference remain separate gaps, not implied by binary predicate support.

### Currency range, list and simple CASE comparison operands

Revisited mssqlite `transpile/src/expression.ts` for BETWEEN, IN and simple CASE.
It selects a common comparison declaration across the input and comparison
operands separately from CASE result typing. Microsoft's
[BETWEEN](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/between-transact-sql?view=sql-server-ver17),
[IN](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/in-transact-sql?view=sql-server-ver17)
and [CASE](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/case-transact-sql?view=sql-server-ver17)
references describe matching operand types and their predicate/result behavior.
The local baseline failed all three currency-text forms with DuckDB error 245.

Currency comparison inference now folds all known comparison operands, skipping
untyped NULLs and preserving unknown/higher-precedence barriers. The existing AST
nodes remain intact, including negation, NULLs, branch results and ELSE. Tests
cover exact rounding, both text/currency directions, false versus UNKNOWN,
narrowing failures, empty result metadata, prepared reuse, CTEs and atomic DML.
The compiler does not add operand copies; this does not establish a new backend
evaluation-order guarantee for volatile BETWEEN or CASE expressions. IN with a
subquery and quantified comparisons still require separate projection coercion.

### Currency subquery comparison projections

Revisited mssqlite `transpile/src/expression.ts` `firstProjectionType` and
`convertedProjection`, alongside Microsoft's
[ALL](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/all-transact-sql?view=sql-server-ver17)
and [ANY/SOME](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/some-any-transact-sql?view=sql-server-ver17)
references. The input and one-column result need compatible comparison types.
The local baseline rejected currency/text IN and ALL with a DuckDB binder error.

The operand binder now supplies explicit output declarations to the pure currency
subquery planner for IN/NOT IN and ANY/SOME/ALL. It wraps the entire source query
when the projected values need currency conversion, preserving its DISTINCT,
ordering and row limits. Existing positional-set wrapping supplies collision-free
private names. A matching currency output requires only left-side conversion.
Unknown output types, invalid result arity and higher-precedence numeric types
remain outside this conversion rule.

Client coverage includes empty and NULL-containing sets, correlated outer names,
prepared reuse, CTEs, DISTINCT/OFFSET/FETCH, TOP ordered by text (where numeric
order differs), generated-name collisions and atomic UPDATE failure. A pure
structural test verifies the original complete query remains intact under the
wrapper. Scalar subquery expression typing and general noncurrency quantified
coercion remain incomplete; these tests are local evidence, not a live reference
comparison.

### Scalar currency query declarations

Revisited mssqlite `transpile/src/implicit.ts` scalar-subquery inference: it
uses a single projected expression's type. Microsoft's
[subquery reference](https://learn.microsoft.com/en-us/sql/relational-databases/performance/subqueries?view=sql-server-ver17)
distinguishes single-value expressions from IN/ANY/ALL sets and requires an error
for multiple scalar rows. The local baseline exposed scalar MONEY as DecimalN,
failed currency-text comparison with 245, and failed styled formatting in parsing.

Projection inference now carries full lexical scopes, including CTE declarations,
into scalar query outputs. Currency conditional inference can consult these
resolved declarations. The operand binder retains known scalar currency,
character and numeric declarations in logical casts, then applies currency plans
after child query typing as well as on entry. For quantified RHS queries, the
scalar annotation is removed before set comparison planning; it must not turn a
multi-row set into a scalar expression.

Tests verify direct/empty MoneyN metadata, comparisons, formatting, conditional
identity, nested correlation, inherited CTEs, SELECT INTO declarations, prepared
reuse, scalar error 512 and atomic writes. Quantified multi-row currency queries
retain their behavior. Unknown or multi-column projections remain unresolved;
full nullability/collation typing and arbitrary function/arithmetic provenance
remain incomplete.

## Exact currency aggregates

The upstream `packages/transpile/src/implicit.ts` aggregate branch resolves its
input declaration, preserves MIN/MAX, promotes small integer SUM/AVG and widens
decimal results. Its currency fallback retains the input type, so SMALLMONEY
promotion must not be copied unchanged. Microsoft documents MONEY results for
both currency input families in SUM and AVG, and an error when AVG's sum exceeds
the return type's range:

- https://learn.microsoft.com/en-us/sql/t-sql/functions/sum-transact-sql?view=sql-server-ver17
- https://learn.microsoft.com/en-us/sql/t-sql/functions/avg-transact-sql?view=sql-server-ver17
- https://learn.microsoft.com/en-us/sql/t-sql/functions/min-transact-sql?view=sql-server-ver17

Before this change, a local SUM(MONEY) advertised DECIMAL(38,4), AVG(MONEY)
advertised FLOAT, and averaging MONEY's maximum with 0.0001 succeeded. The new
path retains logical currency declarations, widens SMALLMONEY inputs to MONEY,
and uses signed scaled-integer accumulation. AVG truncates the scaled quotient
without an intermediate floating-point conversion. NULLs do not increment the
count; empty groups return typed NULL. DISTINCT and window frames remain on the
native aggregate, with one source expression and sticky state overflow.

Client checks cover exact positive/negative fractions, boundary coefficients,
SMALLMONEY widening, metadata, formatting, prepared rebinding, derived/CTE
results, SELECT INTO, windows and atomic failed writes. Native checks cover
6,000-row chunks, shared window states and a volatile input consumed once.
Live SQL Server comparison of rounding and execution-order-sensitive overflow
remains outstanding; arbitrary currency arithmetic provenance is still open.

The new local overflow capture reports number 8115/state 1/severity 16 but still
includes DuckDB's `Invalid Input Error:` message prefix. The raw audit retains
that text; this change does not claim exact SQL Server diagnostic wording.

## Numeric runtime diagnostic identity

Microsoft's Database Engine error list specifies severity 16 and canonical text
for arithmetic overflow (8115) and divide-by-zero (8134):
https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-8000-to-8999?view=sql-server-ver17
The upstream `packages/engine/src/engine.test.ts` divide-by-zero tests exercise
catchability, error 8134, continued execution and single evaluation. Those are
useful behavioral dimensions; this change specifically closes the backend
message-prefix leak in direct errors, ERROR_MESSAGE and bare rethrow.

The deterministic core recognizes exact numeric messages that msduck itself
emits: division by zero, character-to-INT overflow, and expression overflow for
supported integer, currency and character targets. The root removes one known
DuckDB `Invalid Input Error:` envelope before recognition. Unknown targets,
prefixes and suffixes do not match the canonical classifier. The legacy backend
fallback remains for unrelated errors. Explicit typed THROW errors bypass all
recognition, retaining their application number, state and literal message even
when that message looks exactly like a backend-wrapped built-in error.

This supersedes the preceding currency-aggregate note about the retained
backend prefix for recognized MONEY overflow. It does not establish complete
SQL Server error wording, state selection, line attribution or batch-abort
semantics for every backend failure. No live SQL Server endpoint was used.

## Currency arithmetic plans and execution

The upstream `packages/transpile/src/decimal.ts` recognizes MONEY as precision
19/scale 4 and SMALLMONEY as precision 10/scale 4. Those shapes alone do not
supply currency result identity or signed coefficient bounds. Local baseline
queries returned DECIMAL for MONEY addition, FLOAT for MONEY division, accepted
SMALLMONEY addition beyond 214748.3647, and rejected a valid '$2' operand using
DuckDB's numeric lexer.

Microsoft documents same-type arithmetic result identity and higher-precedence
mixed results in:
https://learn.microsoft.com/en-us/sql/t-sql/data-types/precision-scale-and-length-transact-sql?view=sql-server-ver17
https://learn.microsoft.com/en-us/sql/t-sql/language-elements/divide-transact-sql?view=sql-server-ver17
Its SqlMoney implementation also uses exact decimal multiplication/division and
rounds the resulting coefficient to scale four:
https://github.com/dotnet/runtime/blob/main/src/libraries/System.Data.Common/src/System/Data/SQLTypes/SQLMoney.cs
This is implementation evidence, not a live SQL Server result. The arithmetic
path initially used nearest rounding with ties away from zero for both operations.
A subsequent live SQL Server 2025 comparison disproved that inference for division:
MONEY division truncates toward zero, while multiplication rounds. The core now
implements that distinction; the SqlMoney client type is not a substitute for
server ground truth.

Currency-valued +, -, *, /, %, and unary negation now use exact i128
intermediates, scale-four results and signed MONEY/SMALLMONEY range checks.
Known lower-precedence operands use existing currency conversion rules first;
DECIMAL/FLOAT and unknown operands remain outside currency narrowing. NULL
propagation belongs to the adapter, including NULL divided by zero. A surrounding
TRY_CAST does not suppress an error while producing its input expression.

Tests cover metadata, positive/negative fractional ties, coefficient endpoints,
zero divisors, mixed precedence, prepared calls, CTEs and correlated queries,
aggregate inputs, SELECT INTO and atomic writes. Native tests check 6,000 rows
with NULLs and two independently counted volatile operands. Complete arithmetic
coercion for all SQL types and live diagnostic/evaluation-order parity remain
unfinished.


### Live currency character alignment and Unicode styles

SQL Server 2025 RTM-CU7 was probed across INT, BIGINT, DECIMAL, FLOAT, REAL,
MONEY, SMALLMONEY, BIT and character sources, with fixed and varying character
families. Only MONEY/SMALLMONEY were right-aligned in CHAR/NCHAR. Other numeric
sources and text remained left-aligned, including the integer overflow asterisk.
The style matrix also showed 126 behaving as style 2 for NCHAR/NVARCHAR, extending
the documented CHAR/VARCHAR case assumed by the original implementation.

`artifacts/compatibility/sql-server-character-alignment.json` preserves all
26 reference captures and server version. The combined regression is captured
in `artifacts/compatibility/sql-server-character-alignment-probe.json`.
The pure character core now distinguishes currency from other numeric sources;
the exact money formatter no longer branches on the target's Unicode encoding.
Storage padding remains independent of formatted numeric CAST alignment.


The corrected local audit completed all 275 cases. Of 274 preceding executions,
273 were unchanged; the sole change was the fixed-width currency value in
`Currency character output styles`, moving four spaces from its right to its
left. That case's rows now match the preserved SQL Server rows. The new combined
alignment/style-126 probe also matches its live reference rows exactly. Metadata,
diagnostics and completion events in existing local captures were unchanged.
Raw evidence is retained in
`artifacts/compatibility/character-alignment-local-diff.json`.


Currency alignment verification (2026-09-22): formatting, strict Clippy, all
324 workspace Rust tests and all 339 client/harness tests passed, with zero
failures, cancellations or skips. All 275 local audit cases completed; 273
preceding captures were unchanged and one corrected its currency alignment.
The new combined probe matches live SQL Server rows. The reference container
was removed, its dedicated VM stopped, and the Docker context was unchanged.
Full SQL Server compatibility remains incomplete.


### Result declaration and TDS flag review

Reviewed upstream `packages/tds/src/token/col-metadata.ts` alongside the copied
MS-TDS flag table. Its named bit definitions confirm nullable (1), unknown
updatability (8), identity (16) and computed (32). Its convenience helper defaults
to nullable/read-write; those defaults were not adopted as SQL Server ground
truth. Live SQL Server 2025 RTM-CU7 probes determine the ordinary-column,
identity, expression and derived-result combinations used here.

Result properties now flow through deterministic catalog snapshots and projection
inference into the byte codec. The final audit completed 276 cases. For matching
execution-column positions/names in the preserved live corpus, 164 flags improved
and none previously matching regressed. The new declaration/join probe's flag
matrix matches its live capture. Raw differences, inference limits and the
remaining fixed-type/width gaps are documented in [result metadata](result-metadata.md).


### Grouping result-property context

Reviewed upstream `packages/transpile/src/grouping.ts`: its recursive rewrite
checks whole grouping expressions before descending and leaves aggregate inputs
intact. The Rust metadata planner follows that expression-boundary distinction
without adopting SQLite's UNION expansion. It reuses the existing bounded Rust
grouping validation. Upstream `packages/tds/src/token/col-metadata.ts` provides
flag constants, but its default nullable/updateable policy does not establish
SQL Server's computed-key flags. Eleven live SQL Server captures establish those
properties; see `docs/result-metadata.md` and its referenced raw artifacts.

### Fixed scalar TYPE_INFO and payloads

Reviewed upstream `packages/tds/src/type-info.ts::fixedInt` and
`packages/tds/src/value.ts::encode`: fixed families use a single type token,
unprefixed payload bytes, and reject NULL. The Rust TDS crate now exposes one
column decision consumed by both metadata and row encoding. Integers, BIT,
REAL/FLOAT and MONEY/SMALLMONEY use fixed families only with explicit non-null
properties. MONEY preserves the existing high-word-first byte layout.

Live SQL Server scalar and conditional-conversion probes are recorded alongside
paired local captures in artifacts/compatibility; see docs/result-metadata.md.
The broad audit exposed nullable COALESCE conversions that were hidden while all
wire types used nullable families. Ordinary cast NULL propagation inside ISNULL
is handled separately from a standalone CAST's nullable result descriptor.
