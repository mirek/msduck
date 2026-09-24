# Integer IDENTITY foundation

CREATE TABLE and ALTER TABLE ADD accept one IDENTITY column of TINYINT, SMALLINT, INT or BIGINT.
The default seed/increment are 1,1; signed nonzero increments and seeds within
the column range are supported. The column is NOT NULL. A persistent DuckDB
sequence supplies defaults, with table and sequence creation in one transaction.
Allocation is shared across connections and survives rollback; native tests also
verify reopening the database and concurrent allocation.

INSERT with a column list can omit the identity column. INSERT without a column
list supplies the nonidentity columns; DEFAULT VALUES also allocates an ID.
Explicit identity-column inserts report 544 while IDENTITY_INSERT is off, and
UPDATE of the identity column reports 8102. Preparation binds the default without
allocating. Existing integer assignment conversion still applies to other values.

This is an initial implementation. SCOPE_IDENTITY, @@IDENTITY,
SET IDENTITY_INSERT, reseeding, decimal identity columns,
zero increments and full diagnostic fidelity remain open.
ALTER can add, change or drop ordinary columns while retaining identity
allocation. Dropping the identity column removes its private sequence and
original definition in the same transaction; rollback restores both. Dependent
constraints or allocation objects prevent the drop. Changing the identity
column's type/nullability remains unsupported.
TRUNCATE resets allocation to the original seed transactionally; rollback restores
the previous allocator even after inserts following the reset. DROP TABLE now removes its private sequence in the same transaction;
rollback restores the table and sequence. Dependency failures roll back the drop,
and dropping/recreating a table starts at the declared seed. Native tests verify
sequence removal directly and preservation when another object depends on it.
Sequences orphaned by older versions are not automatically swept. Full DDL
lifecycle handling remains open. Identity recognition
uses the generated sequence default until a complete SQL Server catalog exists.
Permissions, replication, crash/cache behavior and all failure-allocation details
still need validation. No live SQL Server differential run has been performed.

The upstream `identity.ts` was inspected for persistent definitions, shared
allocation state, non-rollback counters and DML restrictions. DuckDB sequences
provide the corresponding allocation mechanism here. The copied seed/increment
and explicit-insert corpus cases exercise the initial server integration.

References: [IDENTITY property](https://learn.microsoft.com/en-us/sql/t-sql/statements/create-table-transact-sql-identity-property),
[SET IDENTITY_INSERT](https://learn.microsoft.com/en-us/sql/t-sql/statements/set-identity-insert-transact-sql).

IDENT_SEED and IDENT_INCR now read original definitions as NUMERIC(38,0), with
NULLs for NULL, malformed, missing and nonidentity names. One- and two-part names,
bracket/double-quoted identifiers, column inputs and prepared execution are
supported. Definitions live in a private table, created/removed in the identity
DDL transaction, so lookups see uncommitted changes and rollback. Allocation does
not change the original seed. Reopen tests verify seed retention independently
of allocator position. Metadata access restrictions, view lineage, cross-database
names and default-schema rules beyond dbo remain open. Identity tables created by
older versions without definition records currently return NULL from these functions.

A native 6,000-row sequence test checks one name evaluation per input. The
catalog map is an uncorrelated subquery; name expressions remain outside it.
References: [IDENT_SEED](https://learn.microsoft.com/en-us/sql/t-sql/functions/ident-seed-transact-sql),
[IDENT_INCR](https://learn.microsoft.com/en-us/sql/t-sql/functions/ident-incr-transact-sql).

IDENT_CURRENT now reads the last allocated value for a table across connections,
with the seed before first use and NUMERIC(38,0) metadata. Allocations remain
visible after failed inserts, rollback and DELETE. Lookups use the same name and
permission limitations as IDENT_SEED/IDENT_INCR. A scoped bundled-DuckDB patch
preserves the last allocation across checkpoint/WAL reload and prevents failed
private-sequence calls from advancing the saved next counter. Tests cover reopen,
WAL recovery, concurrent connections, prepared reuse, NULL/empty metadata and
repeated exhaustion. BIGINT terminal values can now be allocated before exhaustion,
including positive/negative increments and restart. Native tests compare 42 seed/
increment combinations against i128 arithmetic and verify endpoint WAL recovery. Live SQL Server differential
validation remains pending. See [IDENT_CURRENT](https://learn.microsoft.com/en-us/sql/t-sql/functions/ident-current-transact-sql).


ALTER TABLE ADD IDENTITY allocates once per existing row and enforces NOT NULL. Allocation order among pre-existing rows is not promised. Empty tables
retain the seed until their first insert. The shared CREATE/ALTER validation
rejects a second identity column, NULL/DEFAULT options, unsupported types and
out-of-range seeds. The sequence, original definition and all added columns
share the ALTER transaction: failed population or a later column failure rolls
back their creation in autocommit mode, and explicit rollback removes a completed
addition. Tests cover 6,000-row population, cleanup after overflow/later failure,
negative increments, exact BIGINT endpoints, prepared insertion and WAL recovery. Adding named
constraints or PRIMARY KEY with the new column remains outside the current ALTER
constraint support.


sys.identity_columns exposes the inherited sys.columns fields plus seed_value,
increment_value, last_value and is_not_for_replication. Values come from the
original persisted definition and live allocator. An unused or truncated identity
has NULL last_value; DELETE preserves it, allocation survives failed statements
and rollback, and rollback of TRUNCATE restores the old allocator. ALTER ADD/DROP
and table recreation follow the live identity definition.

The three values use sql_variant wire metadata with TINYINT, SMALLINT, INT or
BIGINT payloads, including exact BIGINT endpoints. NULL and empty results retain
the variant descriptor. The internal representation is a tagged integer structure;
General variant casts, arithmetic/comparison
coercion, RPC input variants and other base types remain unfinished. Seeds and
increments outside their catalog payload type's range fail explicitly. Decimal
identity and full NOT FOR REPLICATION behavior remain open; the replication flag
currently reports false. Permission filtering inherits the catalog limitations.

Native tests cover exact wire vectors, restart persistence and results spanning
6,000 rows. Client tests cover prepared reads, failed allocation, transaction
rollback, DELETE/TRUNCATE, identity replacement and all four integer payloads.

Reference: [sys.identity_columns](https://learn.microsoft.com/en-us/sql/relational-databases/system-catalog-views/sys-identity-columns-transact-sql).


SQL_VARIANT_PROPERTY now reads BaseType, Precision, Scale, MaxLength and TotalBytes
for integer catalog variants and ordinary TINYINT/SMALLINT/INT/BIGINT expressions.
Collation is NULL for these integer inputs; unknown properties and NULL arguments
also return NULL. Property matching is currently case insensitive. BaseType returns
a Unicode sysname payload inside sql_variant, while numeric properties return INT
payloads. Empty results retain sql_variant metadata. Native vector tests check
wire bytes and once-per-row evaluation over 6,000 rows; client tests cover all
four widths, catalog values and prepared arguments. Other input base types,
including property inspection of returned sysname variants, remain unfinished.

Reference: [SQL_VARIANT_PROPERTY](https://learn.microsoft.com/en-us/sql/t-sql/functions/sql-variant-property-transact-sql).


Explicit CAST/CONVERT and TRY_CAST/TRY_CONVERT can now unwrap integer variants
into TINYINT, SMALLINT, INT and BIGINT. Conversion shares the existing integer
range checks: overflow raises 8115, TRY conversion returns NULL, and exact BIGINT
values never pass through floating point. NULL variants remain NULL with the target
integer descriptor, including empty result sets. Implicit variant-to-integer
assignment remains rejected; INSERT SELECT with an explicit cast is supported.
The parser marks user-written integer casts so internal assignment conversions
do not silently gain variant coercion. Native tests verify single evaluation of
ordinary and variant inputs over 6,000 rows; client tests cover all target widths,
overflow, metadata, prepared reuse and explicit versus implicit INSERT.
Conversions from other variant payload families remain unfinished.


CAST/CONVERT and their TRY forms can construct sql_variant values from TINYINT,
SMALLINT, INT and BIGINT inputs, including NULL. Each row retains its original
base type; packing an existing integer variant retains that type. SELECT INTO
can materialize these values, and subsequent explicit-variant INSERT values may
use different integer base types in the same column. Views preserve the tagged
values and sys.columns records sql_variant for direct or explicitly cast outputs.
Native tests cover restart persistence and 6,000-row single evaluation. Client
tests cover mixed stored base types, rollback, prepared BIGINT endpoints and
empty metadata. Payloads other than BIT and integers, and full SQL Server variant comparison/ordering rules, remain
unfinished. Integer predicates and direct-column ORDER BY now use numeric comparison keys,
as described below; broader backend structure comparison is not SQL Server parity.


CREATE TABLE and ALTER TABLE ADD now accept sql_variant columns backed by tagged
integer storage. INSERT VALUES, INSERT SELECT and UPDATE implicitly pack integer
sources; defaults, UPDATE DEFAULT, NOT NULL and ADD DEFAULT WITH VALUES use the
same conversion. ALTER COLUMN from an integer type packs existing values and
records the declared variant type transactionally. Rollback restores the original
values and catalog declaration. Reopening retains stored variants and typed defaults.
Separate inserts can retain different integer base types in one column. Multi-row
VALUES first resolves its common source type, so mixed integer widths may become
BIGINT before assignment. Unsupported inputs outside BIT and the integer types fail explicitly.
SQL_VARIANT variables, RPC input variants, noninteger storage, constraint/index
comparison semantics and broader ALTER conversions remain unfinished.


Integer variants now compare by numeric value independently of their integer
base type. The six ordinary comparison operators, IS [NOT] DISTINCT FROM, BETWEEN, IN/NOT IN lists,
simple CASE and resolved IN/scalar subqueries use nullable BIGINT keys. Ordinary
integer operands join the same comparison path. Catalog-aware annotations resolve
columns through aliases, joins, direct CTE/derived projections and DML scopes.
Direct variant ORDER BY expressions sort numerically without changing returned
payload types; NULL follows the existing SQL Server ordering configuration.
Native tests verify all six nullable truth tables and once-per-row evaluation over
6,000 rows. Client tests cover mixed base tags, prepared predicates, joins,
subqueries, ordering, UPDATE and DELETE filters.

This does not implement full variant comparison semantics: noninteger families,
collations, GROUP BY and DISTINCT aggregates, set-operation deduplication, index uniqueness,
unrecognized expression provenance still
need compatible handling. Explicit integer conversion remains available where
a numeric comparison key is required in those contexts.


Conditional expressions with known integer variant results now retain variant
payloads through CASE, COALESCE, IIF, CHOOSE and ISNULL. Integer branches and
replacements are packed only when selected. NULLIF compares numeric keys but
returns its original first value when unequal, preserving that value's base type.
An ordinary integer first argument still yields an integer result even if the
second argument is a variant. Result inference propagates through nested
conditionals and direct CTE projections for subsequent predicates.
Native tests verify COALESCE evaluation counts over 6,000 rows; client tests cover
NULLIF, payload types, empty descriptors, prepared reuse and unselected failing
branches. NULLIF retains searched-CASE evaluation semantics and may evaluate its
first argument twice, as documented by SQL Server. Variant conditionals for other payload families and full catalog projection provenance remain unfinished.


BIT is now supported as a variant payload alongside the four integer widths.
Packing, column assignment, defaults, conditional results and restart preserve
its Boolean base type. Wire values use the fixed BIT token inside sql_variant;
false and true are distinct from NULL. SQL_VARIANT_PROPERTY reports BaseType bit,
Precision 1, Scale 0, MaxLength 1 and TotalBytes 3. Integer-family predicates compare
BIT as 0 or 1 without changing returned Boolean payloads.
Explicit CAST/CONVERT and TRY forms can convert BIT/integer variant payloads to
BIT; zero becomes false, nonzero becomes true, and NULL remains NULL. Conversion
of BIT variants to integer targets also works. Native tests include exact BIT
wire vectors, persisted defaults and 6,000-row evaluation counts; client tests
cover prepared BIT parameters, storage, properties, predicates, conditionals and
both conversion directions. This does not add BIT IDENTITY or implicit
variant-to-BIT assignment. Other variant base types remain unfinished.

IS DISTINCT FROM and IS NOT DISTINCT FROM use the same numeric comparison keys
for integer/BIT variants. Both NULL operands compare as not distinct; exactly one
NULL compares as distinct. Tests cover mixed base tags, prepared parameters,
CTEs, scalar subqueries, joins and transactional UPDATE/DELETE predicates.
This implements the [SQL Server 2022+ predicate truth table](https://learn.microsoft.com/en-us/sql/t-sql/queries/is-distinct-from-transact-sql?view=sql-server-ver17);
it does not change DISTINCT/GROUP BY deduplication.

ORDER BY now resolves known variant output aliases and ordinal positions to
numeric sort keys. A derived projection keeps each selected value available to
both output and sorting, preserving base tags and volatile evaluation. TOP and
OFFSET/FETCH remain outside that projection and therefore select rows after
numeric ordering. Hidden source sort expressions support secondary ordering
without adding result columns. Tests cover alias collisions, stars, computed
COALESCE outputs, prepared paging, empty metadata and 6,000 volatile values.
Unknown output types and ordering expressions outside resolved query scopes
remain limited; full variant set/group/index equality is still unfinished.

UNION ALL and multi-row VALUES now resolve a common SQL_VARIANT result when a
known operand is a variant. Integer/BIT operands are packed before branch/row
combination, preserving their individual base types, NULLs and duplicates.
Inference propagates through CTE/derived outputs to predicates, numeric ordering
and assignments. Client tests cover prepared parameters, paging, empty result
metadata and INSERT SELECT; native tests verify 12,000 rows and single evaluation
of volatile branch inputs. UNION without ALL, INTERSECT and EXCEPT still need
variant-aware duplicate comparison; this increment does not implement those.

SELECT DISTINCT now deduplicates known integer/BIT variant projections by their
numeric values, treating repeated NULLs as a single value. Other projected columns
remain part of the distinct tuple. An inner projection evaluates values before
key extraction; a representative original variant is returned without forcing a
new base type. Which base tag survives among equal variants is not promised.
TOP and OFFSET/FETCH apply after deduplication. Client tests cover aliases,
qualified source projections, stars, prepared conditionals, empty metadata and
multi-column tuples; a native test exercises 6,000 nullable mixed-tag values.
GROUP BY, DISTINCT aggregates, and duplicate-eliminating set operations still need
variant-aware equality, as do noninteger families and collation semantics.

DISTINCT ordering now resolves ordinary and qualified wildcard projections to
source-column identities before wrapping. Mixed wildcard/explicit projections,
alias precedence, quoted identifiers and joined sources with matching column
names retain their output positions. Qualified references to columns absent from
the DISTINCT projection still fail. Client coverage includes TOP, prepared CTE
paging and multiple sort columns from separate joined sources.

UNION now applies numeric variant deduplication after compatible branch
conversion. Mixed integer/BIT base tags and repeated NULLs collapse by value;
other columns remain part of the row key. A representative original variant
payload survives each distinct boundary. Nested UNION/UNION ALL chains preserve
their separate duplicate rules, and outer ordering/paging follows deduplication.
Tests cover CTE predicates, prepared parameters, empty descriptors, multi-column
tuples and BIGINT endpoints. Native execution combines 12,000 rows and verifies
each branch's volatile input is evaluated exactly once per row. INTERSECT, EXCEPT,
GROUP BY, DISTINCT aggregates, noninteger families and index equality remain open.

INTERSECT and EXCEPT now compare integer/BIT variant tuples by numeric keys,
including NULL equality, and return distinct representative left-side values.
Compatible nonvariant integer/BIT branches convert to variants before comparison.
Both input branches are materialized for membership checks; output deduplication
retains base payloads. Column names come from the left projection. Client tests
cover multi-column rows, CTEs, prepared parameters, paging, mixed set precedence,
empty metadata, left SMALLINT payloads and BIGINT endpoints. Native tests check
12,000 rows per operator and once-per-row volatile branch evaluation. Broader
variant families, grouping, distinct aggregates, collations and index equality
remain unfinished; exact SQL Server nullability metadata still needs review.

COUNT(DISTINCT value) and COUNT_BIG(DISTINCT value) now use numeric comparison
keys for known integer/BIT variants. Equal values across base tags count once,
NULLs are excluded, and empty/all-NULL inputs return zero. COUNT retains INT
metadata and COUNT_BIG retains BIGINT metadata. Client coverage includes ordinary
integer grouping/HAVING, CTE projections, prepared conditional replacements,
window DISTINCT rejection and exact BIGINT endpoints. A native 6,000-row check
verifies volatile argument evaluation once per row. Known variant inputs to
APPROX_COUNT_DISTINCT are rejected with 8117, as SQL Server disallows that type.
Other variant aggregate/grouping semantics and noninteger payload comparison
remain unfinished.

Known SQL_VARIANT arguments to SUM, AVG, STDEV, STDEVP, VAR and VARP are rejected
with error 8117 before execution. Validation includes DISTINCT, window aggregates,
empty/all-NULL inputs, CTE projections and parameterized calls. Invalid prepared
queries fail during preparation, and INSERT SELECT failures leave targets empty.
Explicit CAST to a supported numeric type enables the aggregate and preserves
normal numeric semantics. Broader payload families remain unfinished.

MIN/MAX now order known integer/BIT variant arguments numerically and return the
selected original payload with its base tag. A native numeric-first aggregate key
avoids repeating the argument; a native result conversion restores variant storage.
NULL inputs are ignored, and empty/all-NULL aggregates return typed NULL variants.
DISTINCT is redundant for extrema and retains those results. Window frames retain
native aggregate execution. Tests cover grouped/HAVING and CTE predicates, sliding
windows, nested conditionals, prepared inputs and exact BIGINT endpoints. Native
6,000-row checks verify each argument is evaluated once and the chosen tags survive.
No particular base tag is promised when multiple base types represent the same
extreme numeric value. Noninteger payloads and exact
catalog propagation for aggregate expressions remain unfinished.

Window partition keys now use the existing integer/BIT numeric comparison key.
Named windows are expanded before source-column annotation so inherited keys
receive the same treatment. NULLs share a partition; other partition expressions
remain part of the compound key. Client tests cover COUNT, ROW_NUMBER and MIN/MAX,
mixed tags, CTE aliases and prepared COALESCE fallback values. Noninteger payload
families and aggregate catalog propagation remain open.

The partition implementation applies SQL Server's
[variant comparison rules](https://learn.microsoft.com/en-us/sql/t-sql/data-types/sql-variant-transact-sql?view=sql-server-ver17)
to the partition expressions described by the
[OVER clause](https://learn.microsoft.com/en-us/sql/t-sql/queries/select-over-clause-transact-sql?view=sql-server-ver17).
The local audit records this interpretation; it has not been compared with a live
SQL Server instance.

GROUP BY now replaces known integer/BIT variant grouping expressions with numeric
keys. References in projections, HAVING and ORDER BY retain a representative
payload through MIN; aggregate arguments continue to consume original input rows.
GROUPING detects subtotal rows so representative values become typed NULLs there.
The parser exposes ROLLUP, CUBE and GROUPING SETS as grouping AST nodes. These
forms retain duplicate sets and actual NULL groups, following the
[GROUP BY semantics](https://learn.microsoft.com/en-us/sql/t-sql/queries/select-group-by-transact-sql?view=sql-server-ver17).
No particular base type is promised for equal values stored under different tags.
Native coverage includes 6,000 rows with mixed tags; client coverage includes
qualified references, wildcard projections, grouped windows, exact BIGINTs, CTEs,
empty subtotals and prepared conditional keys. Redundant packing is canonicalized
so repeated group references remain structurally equal. Repeated named parameters
share a numbered bound slot for the same reason; parameter values remain bound.
Correlated grouped subqueries, noninteger variant families and complete grouping
metadata/validation remain unfinished. The audit is local, without live SQL Server
comparison.

Grouping indicators now carry their declared SQL Server result types:
[GROUPING returns tinyint](https://learn.microsoft.com/en-us/sql/t-sql/functions/grouping-transact-sql?view=sql-server-ver17)
and [GROUPING_ID returns int](https://learn.microsoft.com/en-us/sql/t-sql/functions/grouping-id-transact-sql?view=sql-server-ver17).
Explicit result casts preserve these widths through the wire, empty results and
variant property inspection. Type inference and SELECT INTO catalog declarations
recognize both functions. Basic signature validation rejects empty calls,
multiple GROUPING arguments, wildcard arguments and unsupported modifiers/windows.
Client tests cover reversed argument bit order, actual NULLs versus subtotal
NULLs, prepared HAVING and variant grouping keys. Complete context validation,
exact SQL Server error wording and 32-bit mask boundaries remain unverified.

Grouping limits now count generated sets with saturating arithmetic before any
expansion. CUBE contributes combinations, ROLLUP contributes prefixes and GROUPING
SETS preserves duplicate groups; separate constructs form a Cartesian product.
Queries exceeding 4,096 sets fail with 10703. Advanced grouping expressions also
use a 32-distinct-expression limit (10706), resolving qualified and unqualified
column references to the same source identity. These limits follow the documented
[GROUP BY restrictions](https://learn.microsoft.com/en-us/sql/t-sql/queries/select-group-by-transact-sql?view=sql-server-ver17).
Nested CUBE/ROLLUP expressions inside GROUPING SETS become explicit sets only after
the total is bounded, avoiding a sqlparser/DuckDB serialization mismatch. Numeric
variant keys then use the existing group lowering. Tests cover 4,096/4,097 count
boundaries without executing a 4,096-set aggregation, 32/33 expression boundaries,
ordinary GROUP BY with 33 columns, prepared failures and failed INSERT targets.
Live SQL Server comparison and legacy grouping modifiers remain unfinished.

Legacy WITH CUBE and WITH ROLLUP now lower to the same grouping AST as the modern
forms after removing repeated expressions. Source identity canonicalization merges
qualified/unqualified references. This retains legacy duplicate-group removal
while modern grouping constructs still preserve duplicate sets. The implementation
follows the legacy forms and restrictions in the
[GROUP BY reference](https://learn.microsoft.com/en-us/sql/t-sql/queries/select-group-by-transact-sql?view=sql-server-ver17).
Mixing legacy modifiers with modern grouping constructs returns 10702. Unsupported
modifiers remain errors; GROUP BY ALL semantics remain unfinished. Tests cover
NULL groups/subtotals, DISTINCT aggregates, typed indicators, variant keys,
prepared replacements and the 12/13-expression boundary. Exact boundary error
wording and live SQL Server comparison are not yet verified.

Explicit GROUP BY ALL now retains groups excluded by WHERE. A token-aware parser
marker distinguishes this syntax from DuckDB's implicit-column shorthand and
leaves comments, strings and quoted identifiers intact. Lowering materializes the
source columns and one predicate result per input row, then makes aggregate
arguments conditional on that result. COUNT(*) uses a conditional non-NULL marker;
other aggregates ignore NULL inputs for excluded rows. Original grouping keys stay
unfiltered, including integer/BIT variants; HAVING filters the resulting groups.
A 6,000-row native test verifies one predicate evaluation per row across multiple
aggregates. Client tests cover prepared parameters, NULLs, joins, CTEs, output
aliases, wildcard projections, grouped windows, statistical aggregates and typed
empty results. A private CTE name avoids shadowing referenced user sources.
The semantics follow Microsoft's
[GROUP BY ALL description](https://learn.microsoft.com/en-us/sql/t-sql/queries/select-group-by-transact-sql?view=sql-server-ver17)
and the zero-count example in the
[AWS SQL Server migration reference](https://docs.aws.amazon.com/dms/latest/sql-server-to-aurora-mysql-migration-playbook/chap-sql-server-aurora-mysql.sql.groupby.html).
Correlated grouped subqueries and remote/FILESTREAM restrictions remain open.
Live SQL Server comparison remains outstanding: no reference environment variables
are configured; the available Docker VM is ARM64 with about 2 GB of memory, rather
than a supported x86-64 SQL Server container host. No reference container was started.

SELECT alias visibility is now checked before grouping rewrites. In resolved
source scopes, WHERE/GROUP BY/HAVING references to a SELECT alias without a real
source column of that name fail with 207. This follows the logical clause order
explained in Microsoft's
[error 207 reference](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/mssqlserver-207-database-engine-error?view=sql-server-ver17).
Source-name collisions remain source references, while ORDER BY aliases and names
exported from CTEs/derived tables remain available. Validation uses cloned syntax
to exclude datepart keywords from column lookup and does not cross nested query
boundaries. Client tests cover execution/preparation failures, unchanged INSERT
targets and these valid counterexamples. Unknown-source scopes and correlated
outer-alias binding remain unfinished; there is no live SQL Server comparison.

GROUP BY expressions now reject built-in aggregate calls and subqueries during
static validation, before grouping rewrites or execution. Error 144 uses severity
15, following the documented
[SQL Server diagnostic](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-0-to-999?view=sql-server-ver17).
The check traverses nested expressions and grouping constructs while validating
each SELECT independently. Derived-table aggregate outputs and SELECT-list
subqueries remain legal; same-query window placement keeps its existing 4108
error. Client tests cover direct and prepared calls, quoted aggregate names,
CASE/COALESCE, legacy/ALL forms and failed INSERT atomicity. User-defined aggregate
resolution, other grouping binding rules and live SQL Server comparison remain
unfinished.

Constant-only grouping keys now return error 164 (severity 15), following the
[documented diagnostic](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-0-to-999?view=sql-server-ver17).
Validation runs after grouping normalization, checks each grouping expression,
and ignores datepart units as syntax. Empty grouping tuples retain their total
semantics. Identifiers remain for binding, so a derived column with a constant
value is valid. Tests cover prepared failures, parameters, failed INSERT
atomicity, duplicate empty grouping sets and ordinary column expressions.
Outer-reference-only identifiers still need lexical binding validation; this
increment has no live SQL Server comparison.

Grouping validation now consults lexical source scopes to exclude proven outer
column references when requiring a local grouping column (error 164, severity
15). Local columns and qualifiers shadow outer names; ambiguous or unknown
bindings remain for the binder. Expression subqueries inherit enclosing scopes,
while CTE definitions and ordinary derived queries establish new boundaries.
Client coverage includes nested correlations, aliases, mixed local/outer
expressions, CTE output columns, prepared failures and unchanged INSERT targets.
The local audit records both outer-only rejection and a valid mixed expression.
Full binding across unknown sources, APPLY and set-query boundaries still needs
work; no live SQL Server comparison has been performed for this increment.

The client harness now allows 20 seconds for server startup and 30 seconds for
the first login/recovery test. On this macOS workspace, a no-op all-target build
replaced the executable inode; the following measured launch reached readiness
in 10.38 seconds, exceeding the old 10-second startup guard. The first test then
passed in 10.35 seconds with the revised guard. Five-second connection and query
deadlines are unchanged. This adjustment addresses measured launch latency, not
SQL execution failures.

Parenthesized query expressions now inherit enclosing grouping correlation,
following the query-expression structure documented in
[SELECT syntax](https://learn.microsoft.com/en-us/sql/t-sql/queries/select-transact-sql?view=sql-server-ver17).
The scope visitor distinguishes WITH definitions, derived queries and expression
subqueries from parenthesized set branches. The dialect also recognizes a scalar
query beginning with a parenthesized branch; speculative parsing restores the
ordinary expression path on failure and propagates recursion-limit errors.
Tests cover both sides of set operations, repeated parentheses, local/outer mixed
keys, multiple CTEs, arithmetic, prepared rejection and unchanged INSERT targets.
The local audit records invalid outer-only and valid mixed grouping cases. Full
APPLY/unknown-source binding and live SQL Server comparison remain unfinished.

CROSS/OUTER APPLY grouping validation now receives a scope built from the left
input before each APPLY join, matching the left-to-right dependency described in
[FROM/APPLY](https://learn.microsoft.com/en-us/sql/t-sql/queries/from-transact-sql?view=sql-server-ver17).
The visitor preserves enclosing query bindings and isolates the APPLY scope from
its own output and later joins. Known outer-only grouping keys reject with 164
(severity 15), including prepared queries. Local keys, local name shadowing and
mixed local/outer expressions remain valid. Tests also cover chained inputs,
nested correlation, failed INSERT atomicity, CROSS APPLY's empty-right omission
and OUTER APPLY's NULL extension. The local audit records rejection and the NULL
extension result. Parenthesized join trees, unresolved table-valued sources and
complete binding diagnostics remain unfinished; this is not a live SQL Server
comparison.

Unaliased parenthesized join trees now merge their constituent source scopes in
order, retaining qualifiers, ambiguous names and known types. This enables the
same integer aggregate metadata, variant numeric grouping and GROUP BY alias
validation as an unparenthesized join. Source order and explicit parentheses are
preserved. APPLY scope frames and lowering both recurse into nested join trees,
so APPLY within parentheses retains its left-input dependency and CROSS/OUTER row
semantics. Client tests cover qualified/unqualified names, prepared outer-only
rejection, variant equality and NULL extension. The local audit captures INT SUM
metadata and nested OUTER APPLY results. Aliased join groups, APPLY whose right
input is itself a joined tree, unresolved table-valued sources and live SQL Server
comparison remain unfinished.

Joined APPLY right inputs now pass their external dependency scope to each nested
constituent. An APPLY within that input additionally receives its own left-hand
sources. This preserves the left-to-right dependency documented in
[FROM/APPLY](https://learn.microsoft.com/en-us/sql/t-sql/queries/from-transact-sql?view=sql-server-ver17)
without exposing later joins. Runtime probes established that the existing
DuckDB lowering already preserves joined-right-input duplicates and OUTER NULL
extension; the missing behavior was error 164 for outer-only grouping keys inside
those constituents. Tests cover both sides of the joined input, repeated nesting,
inner APPLY chains, valid mixed keys, prepared rebinding and failed INSERT
atomicity. Local audit probes capture rejection and duplicate/NULL rows.
Join-group aliases are outside the documented grammar; exact rejection behavior,
complete table-valued-source and join-predicate binding, and live SQL Server
comparison remain unverified.

ON-expression subqueries now use a temporary scope built from the current join's
left and right inputs plus enclosing query/APPLY dependencies. The scope lasts
through nested predicate expressions and is removed before later SELECT clauses.
This corrects both false error 164 for later-table references and missed 164 when
a later table introduces a colliding column name. Valid local or mixed grouping
expressions remain accepted. Tests cover direct/prepared queries, nested joins,
joined APPLY inputs, multiple subquery levels, WHERE restoration and failed
INSERT atomicity. The audit records the collision rejection and a valid mixed
key. Complete missing-name error classification, DML join scopes, unresolved
sources and live SQL Server comparison remain unfinished.

UPDATE and DELETE now retain original join syntax through a semantic binding
pass before resolving target aliases and moving target-tree predicates. Parse-time
shape checks still run on a copy, preserving early unsupported-shape rejection.
DML scope frames use the resolved target and original traversal order. Known
qualified references available only outside the active ON scope now return 4104,
consistent with the documented
[unbound multi-part identifier diagnostic](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/mssqlserver-4104-database-engine-error?view=sql-server-ver17).
Tests cover execution/preparation failures, CTE UPDATE, unchanged target rows,
valid prepared rebinding and valid joined DELETE. Audit cases record UPDATE 164
and DELETE 4104. Complete unqualified-name binding through lowering, unresolved
sources, full missing-name diagnostic coverage and live SQL Server comparison
remain unfinished.

A remaining reproducer is captured as `UPDATE ON unqualified correlated grouping`:
an ON subquery with `GROUP BY i.k+w` should bind `w` to the current join input,
but a later table also named `w` makes the lowered WHERE predicate ambiguous.
The current execution reports 50000 and leaves the target unchanged. The audit
retains this failure explicitly; this valid-input gap is not counted as SQL Server
compatibility success.

The `UPDATE ON unqualified correlated grouping` reproducer is now fixed: names
are qualified against the original visible ON scopes before target-tree lowering.
Resolution follows the nearest source scope and leaves ambiguous or unknown
bindings for the binder. The traversal preserves datepart unit syntax while
allowing the same name to bind as a column in a later argument. Query ORDER BY
retains standalone explicit output aliases; source expressions bind normally.
Tests cover UPDATE/DELETE, correlated grouping and filters, local shadowing,
prepared rebinding, aliases and mixed sort keys. The existing audit reproducer
is retained so its transition from error 50000 to successful update is visible.
Full unresolved-source and missing-name binding and live SQL Server comparison
remain unfinished.
