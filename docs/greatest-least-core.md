# Deterministic GREATEST and LEAST

`msduck_sql::greatest_least` plans declarations from explicit typed operands and
selects an index among already-converted values. The module has no database,
session, wire, environment or process state. It does not lower or execute a SQL
query. Root engine integration remains separate work.

The authority is the owner-retained `reference/greatest-least.json`, merged by
PR #804: SHA-256
`5810610de46a8ced690ac5cd2fe91b895f7b5ca7e1f8342554f26d0a62f41fe9`.
Its 221 programs have four identical SQL Server 2025 captures. See
[the reference contract](greatest-least.md) for the original SQL, descriptor,
row, error and completion evidence. No SQLite function implementation was
copied; the inspected owner mssqlite checkpoint
`7f71f2081602f8e3051998f5c11f058e65fe24ec` has no GREATEST/LEAST dispatcher.

## Declaration inputs

`plan(Function, &[Argument])` returns `Plan` or an explicit SQL error/unsupported
result. Each argument supplies a logical `Type`, known or unknown nullability,
a collation coercion label for character types, and expression provenance.
Parameter values never enter this operation. `Origin::UntypedNull` distinguishes
NULL syntax from a parameter whose declaration is unknown. All-untyped-NULL
lists default to INT; an untyped NULL contributes nullability but does not force
INT into a mixed character list. `Origin::IntegerLiteral(digits)` supplies the
lexical precision contribution of an INT literal. Typed INT contributes ten
integer digits even when its current value is 1 or NULL.

Captured type precedence, decimal integer/scale merging and the precision-38
cap, money, real/float, character family and width merging, temporal scales,
binary widths and MAX demotion are retained. `DecimalFamily` preserves the
DECIMAL/NUMERIC distinction which the shared logical type does not encode.
`Declaration::ast_type()` reconstructs that spelling for compiler adapters.
Collation labels use the existing deterministic coercion rules. Conflicts retain
error 468, including argument-order-dependent names in the captured message.
Arity errors retain 189; XML/TEXT/NTEXT/IMAGE retain 8116 and the one-based
argument position; the captured DATE/INT and TIME/DATE clashes retain 206.
Only captured cross-family temporal and character conversions are admitted.
Other conversion pairs return `Unsupported::UncapturedConversion`.

Nullability and descriptor flags are declaration properties. The retained direct
nullable INT-column cases have the computed flag clear; literal, parameter,
aggregate, nonnullable and retained character-result cases have it set. Unknown
nullability, unfamiliar collation sensitivity and uncaptured nullable column
provenance leave `flags()` unknown rather than inventing a descriptor. The
three retained collations have known sensitivity. Other collation names remain
available to the caller without guessed flags.

## Value inputs

The root adapter must evaluate **every** operand exactly once, perform the
planned coercions, and pass `Result<Comparable, SqlError>` outcomes to
`Plan::select`. This API does not execute expressions or conversions. In
particular, it does not itself round decimals when scale is reduced, pad CHAR,
encode varchar, interpret date strings, widen legacy datetime ticks, or attach
an offset. The explicit `conversion_failure` helper constructs only the captured
varchar-to-INT 245 and varchar-to-decimal 8114 errors; other conversion failures
remain unsupported inputs rather than classified backend text.

Comparable values use the common declaration: bounded exact coefficients with
its scale, finite floats (REAL already rounded to binary32), character UTF-16
payloads with caller-supplied collation keys and the collation name, bounded
binary values, SQL Server mixed-endian GUIDs, or temporal instants in one common
exact tick unit. Character weights must come from an actual collation adapter;
this module does not substitute Unicode code-point order or an ASCII
approximation for linguistic ordering. Binary comparison and the existing
SQL Server GUID ordering are deterministic. Temporal conversion, range and
calendar validation belong to the caller. The captured SQL_VARIANT INT,
decimal and character families are supported; other variant families are
explicitly unsupported. Cross-collation variant character comparison remains
unknown.

Selection skips NULLs, returns `None` only for all NULLs, and preserves the first
tie. It returns the selected operand index so an adapter can preserve the
original payload, including a tied datetimeoffset's original offset. It validates
all supplied outcomes before choosing, so a later error or oversized losing
character/binary operand is not silently skipped. MAX-demoted oversized values
retain 8152 state 10. Wrong key shapes, arity, scales or bounds and nonfinite
floats return explicit unsupported results.

## Verification boundary

The focused tests consume all four raw captures without rewriting them. They
derive scalar operand declarations from source ASTs and supplied catalog or RPC
parameter declarations, compare every directly described scalar projection's
logical declaration, nullability, collation and captured flags, and check the
captured compile-error identities. The helper ASTs and input operands remain
unchanged. All applicable scalar calls, including those nested inside context
queries, receive declaration checks. Numeric and character row tests supply
independent conversions for the retained small numeric/ASCII inputs; those test
weights do not constitute a production collation implementation. Exact
38-digit values, temporal ordering and offset ties, binary/GUID/variant
selection, prepared NULL/error/recovery sequences, late errors and truncation
have focused checks.

DDL/DML, WHERE/ORDER execution, SELECT INTO catalog acquisition, aggregate
execution, RPC/prepared lifecycle, actual coercions and complete TDS token order
remain root integration requirements. Retained contextual programs are input
coverage for declaration planning, not a claim of executing all 221 SQL Server
programs. No server behavior changes until a root adapter invokes the module.
The recovered task does not complete older blocked integration dependencies.
