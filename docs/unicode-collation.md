# Unicode comparison and index-key work

Lossless Unicode storage needs keys that preserve SQL Server equality, alongside
raw UTF-16 values. DuckDB rejects STRUCT columns as index keys but accepts BLOB
expression keys. The raw representation must remain available for SELECT, OUTPUT
and exact character operations; normalized keys cannot replace stored values.

## Reference evidence

`reference/unicode-collation.json` records 140 live comparisons: 35 raw UTF-16
pairs under each of Latin1_General_100_BIN2, SQL_Latin1_General_CP1_CI_AS,
Latin1_General_100_CI_AS and Latin1_General_100_CS_AS. Inputs include trailing
spaces, control characters, NULs, canonical Unicode variants, ligatures, width
and kana variants, complete pairs and isolated surrogates. Every query completed
without an error. These are probes, not exhaustive collation conformance.

Microsoft describes BIN2 as code-point comparison in its
[collation documentation](https://learn.microsoft.com/en-us/sql/relational-databases/collations/collation-and-unicode-support?view=sql-server-ver17).
The tested non-SC Unicode BIN2 collation orders the raw UTF-16 units: a duck emoji
sorts before U+E000 because its first unit is U+D83E. Comparisons space-pad the
shorter operand. Thus `a` sorts after `a` followed by a space and NUL; trimming
trailing spaces then using ordinary lexicographic comparison gives the wrong
ordering for this case.

## Implemented boundary

`msduck_core::bin2` implements comparison and a separate equality-only key over
UTF-16 slices. Equality keys discard trailing U+0020 units and retain all other
units, including isolated surrogates and NUL. They are not sort keys. NULL policy
is supplied by the caller rather than encoded implicitly in the core.

The root supplies bounded native carrier-to-key and comparison adapters. Native
integration tests create a BLOB expression index over the key, verify duplicate
rejection and unchanged values after failed writes, and exercise 6,000 volatile
inputs with NULLs. Both the core and native comparator use the preserved live
BIN2 vectors. The adapter currently propagates NULL; SQL Server's unique-key NULL
rules still need explicit constraint planning.

## Upstream review and remaining integration

The inspected mssqlite `normalizedCollationText` trims spaces, optionally removes
combining marks, and optionally lowercases with the en-US locale. A faithful
reproduction of that equality normalization disagrees with 44 of these 140 live
comparisons. The raw mismatch list and upstream source hash are preserved in
`artifacts/compatibility/unicode-collation-upstream-equality.json`. This review
compares equality only; it does not claim to test SQLite ordering.

Linguistic collations need versioned weight/equivalence rules. SQL Server's
legacy default treats the tested surrogate variants differently from the newer
Windows collations. General Unicode normalization and case folding alone cannot
supply those rules. Do not use the BIN2 primitives as the default collation.

COLLATE binding and precedence, catalog collation propagation, expression
operators, ordering/grouping/set operations, nullable and composite unique keys,
foreign keys, index metadata and migration remain unfinished. Ordinary declared
NVARCHAR/NCHAR columns remain VARCHAR-backed pending that integration. The native
key functions remain internal groundwork. The Unicode comparison path described
below now supports resolved BIN2 operands; broad public COLLATE support remains
unfinished.

Verification for this stage: all 401 library tests, formatting and strict Clippy
pass. The native comparator matches all 35 BIN2 reference pairs. Full workspace
integration/client/audit verification remains pending; the running older Linux
snapshot has an independently reproduced UNICODE computed-flag failure. The
current pure result-property correction has passed its regression test and is
included in the 401-library-test run, but needs a new wire replay. See
`artifacts/compatibility/unicode-bin2-verification.json` for exact snapshot scopes.

## Collation precedence and lexical scopes

`reference/collation-precedence.json` adds 22 live SQL Server programs for
implicit columns, explicit COLLATE, literals, CASE, set operations and derived
queries/CTEs. An explicit label survives a derived-table or CTE boundary: an
outer comparison with a differently collated explicit literal still raises 468.
A literal-derived field retains its default label and yields to an implicitly
collated base column. Consequently a field's collation name alone is insufficient;
logical result fields must retain the coercion label as well as the name.

The probes distinguish an unresolved CASE comparison (4191) from a direct
conflict between named operands (468), and an unresolved SELECT projection (451).
An outer explicit COLLATE resolves the tested CASE and UNION ALL conflicts.
Numeric COLLATE raises 447. Diagnostic operand-name order is preserved verbatim
in the fixture. No implementation of these binding rules is claimed yet.

Microsoft's [precedence documentation](https://learn.microsoft.com/en-us/sql/t-sql/statements/collation-precedence-transact-sql?view=sql-server-ver17)
provides the general label model. The pinned SQL Server version accepts both
same-name and different-name nested explicit COLLATE in these probes, despite
the documentation's general prohibition on repeated explicit COLLATE. Binding
must follow the preserved live behavior rather than introduce that prohibition
without further version-specific evidence.

## Implemented coercion-label contract

`msduck_core::collation::Label` now represents coercible-default, implicit,
explicit and deferred no-collation states. Combining distinct explicit names
returns a typed conflict; combining distinct implicit names preserves a deferred
conflict that an enclosing explicit COLLATE can resolve. Different database
defaults remain an explicit unresolved choice for the binder's conversion rules.
This module performs no name lookup, weight generation or database access.

Logical `binding_scope::Field` records an optional label-or-conflict independently
of type widths, nullability and storage. `CatalogSnapshot` supplies the database
default explicitly. Projection inference retains labels through literals,
parameters, direct columns, character casts, CASE, concatenation, VALUES, sets,
scalar subqueries, derived tables and CTEs where those inputs are known. Unknown
inputs remain unknown; they do not erase a conflict already identified in another
operand. Stars and aliases preserve the field record. Other character functions,
JSON and generated execution-stage declarations still need propagation work.

The root acquires base-column collation names as implicit labels. Its current
single-database type catalog supplies the default; multi-database acquisition
must use the selected database's default. Explicit names in ASTs still need
catalog validation. The initial label contract did not change SQL execution;
the following sections describe subsequent diagnostics and Unicode BIN2 dispatch.

Verification: all 406 library tests, formatting and strict Clippy pass locally.
The full frozen Linux run is recorded in
`artifacts/compatibility/collation-labels-verification.json`. The preceding
450-test snapshot's 322-case local audit has only the intended UNICODE computed
flag change and an unordered APPLY row reversal among its 319 baseline cases;
three added cases and every raw difference remain preserved.

## Character-function label propagation

`reference/collation-functions.json` contains 58 live programs pairing collation
property reads with explicit outer comparisons for 29 expressions. The pure
projection binder now retains input labels for LEFT/RIGHT, case conversion,
ordinary trim, substring, reverse and replicate; numeric-to-string inputs receive
the supplied database default. ISNULL retains the first argument's label even
when the replacement has a stronger explicit label. A literal NULL first argument
instead adopts its replacement. CASE-like COALESCE/IIF/CHOOSE and CONCAT combine
labels, retaining a deferred conflict where appropriate.

The original regression covered 23 expressions and left six sensitive function
expressions unknown. The additional 20 programs in
`reference/collation-sensitive.json` establish nested/deferred conflict behavior.
The combined regression now checks 30 result labels and nine operation failures,
including exact diagnostic numbers, states, severities and messages.

REPLACE and STUFF resolve all character inputs before requiring a usable label.
NULLIF resolves its comparison but retains its first argument's result label.
Direct conflicting operands produce 468; an unresolved input label produces
4191. A typed operation failure survives an outer COLLATE. Query validation now
visits expression boundaries, including predicates, grouping, ordering, nested
wrappers and subqueries, before execution and during preparation. Equality
operators also require a resolved character collation. It uses explicit catalog
and parameter inputs, reserves CTE declarations, clears correlation across
non-lateral derived-table boundaries and respects unresolved-source barriers.
The root shares one snapshot between this validation and projection inference.
JOIN ON expressions and their subqueries see only that join's inputs; unrelated
comma sources and later aliases are excluded. APPLY definitions and projection
inference use the left input, while ordinary derived tables clear outer rows.
Nine live programs in `reference/collation-join-scopes.json` distinguish genuine
468 conflicts from out-of-scope references. The collation pass leaves those
references to name binding instead of manufacturing a collation conflict.

This is not complete statement validation: other comparison operators, nested
join source shapes, DML expressions and private execution projections still
need a unified operation plan.
Function modifiers and unrecognized expressions remain unknown. Public name
validation and comparison dispatch remain unfinished.

## Wire-descriptor reference and upstream correction needed

`reference/collation-wire.json` captures NVARCHAR and VARCHAR metadata for six
Latin1 collations. SQL_Latin1_General_CP1_CI_AS uses version 0 and sort ID 52.
The tested Latin1_General_100 collations use version 2 and sort ID 0, including
the four non-binary case/accent sensitivity combinations. The inspected mssqlite
`tds/src/collation.ts::ofName` incorrectly gives those four combinations sort ID
52. Its bit-packing approach can inform the codec, but its name-to-descriptor
mapping cannot be copied unchanged.

The TDS codec now accepts an optional descriptor on each result column. Its
name lookup recognizes exactly these six captured mappings, case-insensitively;
it does not guess locale, code page or version from a suffix. The root adapter
maps resolved result labels to descriptors only when logical and physical column
counts agree. Normal result batches and runtime-error descriptions share that
adapter. Unknown labels, unresolved conflicts and unrecognized names retain the
existing default wire fallback; they still require compiler validation. FOR JSON
and connection ENVCHANGE retain the connection default.

Codec tests compare every captured descriptor and cover all emitted character
type families, including MAX. An adapter integration test covers empty results,
NULL and Unicode rows, runtime-error metadata and unresolved/misaligned fields.
This transports collation metadata; it does not implement the corresponding
comparison rules or make public COLLATE expressions fully supported.

## Unicode BIN2 comparison dispatch

The binder now plans `=`, `<>`, `<`, `>`, `<=` and `>=` comparisons whose resolved
collation is Latin1_General_100_BIN2 and whose character coercion includes
NVARCHAR/NCHAR. It validates the original tree before applying postorder edits,
retains result declarations before lowering, and passes each operand once through
the lossless carrier adapter to the native UTF-16 comparator. Direct operand
COLLATE wrappers are removed only when that comparison is planned. The parser
also accepts T-SQL `!<` and `!>` at comparison precedence.

`reference/bin2-comparisons.json` preserves 16 operator-conflict programs and five
public comparison programs covering space padding, supplementary characters,
unpaired surrogates, NULL and empty strings. The plan regression compares exact
diagnostics and checks atomicity/idempotence. Server tests reproduce the live
values through preparation and execution. SQL Server folds the four non-NULL
constant CASE programs to non-null INT metadata; current msduck retains nullable
INT. The client differential test explicitly records all three descriptor-field
differences per affected column instead of normalizing them away.

ANSI-only BIN2 comparisons use a separate code-page byte comparator, described
below. Linguistic comparisons, collation names beyond the
validated mappings, standalone/derived COLLATE projections, IN/BETWEEN/LIKE,
sorting/grouping/set keys and DML integration remain unfinished. This path does
not substitute binary comparison for the database's linguistic default.

## ANSI BIN2 comparison dispatch

Eight programs in `reference/bin2-ansi-comparisons.json` establish Windows-1252
byte order, padding, embedded NUL, NULL, surrogate-to-ANSI conversion and mixed
Unicode precedence. Euro, OE and Y-diaeresis sort before nonbreaking space as
ANSI bytes; the mixed NVARCHAR comparison orders their Unicode values instead.

The plan now requires known character declarations (or an untyped NULL) and
selects either the UTF-16 or Windows-1252 adapter. Logical CHAR/VARCHAR conversion
happens before the ANSI adapter restores encoded bytes; it adds no best-fit or
lossy conversion. The core byte comparator shares the space-padding rule with
the UTF-16 comparator. A 6,000-row native test checks NULL propagation and single
evaluation across vector batches.

Projection inference now reuses existing logical character result declarations,
including NCHAR/CHAR/SPACE, and these constructors receive the database-default
collation label. This fixes missing Unicode precedence for bare NCHAR expressions
without adding a database or protocol dependency to the SQL crate. The constant
CASE descriptor gap remains explicit in both Unicode and ANSI client fixtures.

The frozen ANSI BIN2 revision completed Linux formatting, strict Clippy, 463
workspace Rust tests, 395 client tests and 323 diagnostic audit captures. The
raw audit comparison against the preceding local capture changed nine flags
across four character-function queries, with no row-value or transport-error
changes. These differences remain in
`artifacts/compatibility/bin2-ansi-audit-diff.json`; this audit does not establish
SQL Server equivalence for those flags. The following integer constant-CASE
metadata work is a separate revision.

Live follow-up probes in `reference/character-result-flags.json` checked those
nine changed flags: seven now match SQL Server, none regressed from a matching
value, and two ISNULL(CHAR(NULL),SPACE(...)) flags remain mismatched (33 versus
32). The exact per-column evidence is retained in
`artifacts/compatibility/character-result-flags-comparison.json`.

### Constant-expression boundary probes

Twenty additional live programs in `reference/bin2-constant-edges.json` cover
omitted CAST widths, fixed padding, LEFT/RIGHT boundaries, out-of-range CHAR and
NCHAR, SPACE, concatenation, mixed ANSI/Unicode input, runtime parameters,
linguistic collation and explicit conflicts. Decoded DONE fields are canonicalized
before persistence so undefined values are retained in the evidence.

The current raw replay is
`artifacts/compatibility/bin2-constant-edges-replay.json`; its binary hash fixes
the evaluated executable. Six cases match completely, eight differ only in
CASE descriptors, one differs only in its error completion command, and five
have execution gaps. The latter include three comparisons over concatenation,
NCHAR of an isolated surrogate, and a linguistic comparison. None is normalized
away or counted as a compatibility pass.

Concatenation binding is therefore required before broader constant folding:
`expression_collation` already combines operand labels, but the declaration
resolver does not establish the resulting character type for these expressions.
The comparison planner consequently leaves their COLLATE nodes in backend SQL.
The next change must carry concatenation family and bounded/MAX length through
binding and preserve UTF16 carriers in execution, including NULL propagation.
Constant evaluation should subsequently reuse the same comparison label and
Unicode-versus-CP1252 mode selection as execution. Omitted character CAST widths
must use 30, independently of the storage declaration default of 1.

The upstream transpiler's `collation.ts::ofExpression` selects the first available
label for binary expressions and CASE. That rule cannot replace msduck's explicit
coercion-label combination: the live conflict probes require preserving conflicts
and applying precedence before either folding or lowering comparisons.
