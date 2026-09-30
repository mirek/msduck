# Runtime percentile execution and session RAND

PR674 integrates the deterministic runtime plan with root execution. Each proven
statement-wide fraction is evaluated once, converted using its source declaration,
and bound through private parameters. The caller's variables are unchanged.
Preparation binds a harmless fraction using declarations only and does not execute
fraction effects or reject runtime NULL/character/range bindings.

Invalid fractions use a volatile native guard on the ordering input. Empty input
never invokes that guard; all-NULL ordering rows do. The guard carries a bounded
private ticket selecting the retained structured SQL diagnostic. Its typed CASE
branch cannot be reached after the guard raises, so ordering evaluation is not
repeated. Metadata comes from original logical declarations, including CONT FLOAT,
DISC source type and source collation; fraction values never supply metadata.

Two fresh pinned SQL Server probes confirmed that wire-bound FLOAT and REAL retain
subnormals: negative subnormals produce8727. Scientific SQL literals instead use
the separately captured literal-reading policy. The same probes confirm that the
tested selected division8134 is suppressed on empty percentile input. Raw probe
captures are retained under artifacts/remote/linux.local/percentile-runtime-adapter-v1.

Focused deterministic, native and client tests cover source conversion, invalid
character/NULL/range diagnostics, empty versus all-NULL input, lazy CASE, ABS,
constant scalar subqueries and repeated valid/invalid/valid prepared bindings.
These tests do not establish full SQL Server compatibility.

RAND state belongs to the session. The captured seeded recurrence uses two integer
components and the captured rounded output multiplier. A bounded registry provides
execution-scoped, unguessable call tickets to shared native functions. Each call
site draws lazily and memoizes its result across rows and native chunks; separate
call sites draw separately. Scope destruction removes its entries on success or
failure. NULL seeds, untaken CASE branches and ordinary empty input do not advance
the stream. Empty percentile input still evaluates its statement-wide fraction.
Preparation does not draw, and rollback does not restore the generator.

The source-specific seed adapters retain captured INT, BIGINT, FLOAT, VARCHAR and
NVARCHAR conversion behavior. FLOAT overflow retains232/state3, BIGINT overflow
retains8115/state2, and failed character conversion retains245/state1 and the source
family. Character conversion reuses the existing integer conversion rules, keeps
raw UTF-16 diagnostic payloads and bounds native memory access and error tickets.
Client coverage compares all777 retained seeded values by their exact FLOAT bits,
plus independent connections, preparation, rollback and conversion recovery.

Result declarations are inferred from a separate clone of the original bound
query. Supported RAND calls in this clone use the existing CONVERT(NULL,FLOAT)
declaration surrogate, which represents an unknown value rather than a foldable
NULL. The execution AST and generator remain untouched. Existing known field
metadata is preserved, and only proven missing declarations are filled when field
counts agree. Fresh SQL Server captures cover direct, nested COALESCE/ISNULL/CASE
and empty result descriptors; ISNULL's nonnullable FLOAT is preserved.

Complete retained reference plans are replayed without normalizing differences.
The original137-record percentile setup fails on unsupported CREATE SEQUENCE
INCREMENT BY. A separately labelled replay retains that failure, records a
supplemental table-only setup and executes the remaining original requests. RAND's
53-record replay retains raw descriptors, seed errors, FLOAT bits and completion
events. Raw evidence lives under the ignored artifact directory named above.
Remaining ORDER, DONE, prepared return flags, SERVERPROPERTY, sequence/frontend and
other descriptor differences are compatibility gaps, not passing comparisons.

Unproven carrier/declaration pairs remain explicit barriers. Composed or
column-derived FLOAT seed diagnostics, REAL seeds, initial unseeded entropy,
extreme FLOAT overflow formatting, additional DML/OUTPUT contexts and invalid RAND
seed suppression on empty percentile input have not been established by these
captures. They require further ground truth before broader compatibility claims.
