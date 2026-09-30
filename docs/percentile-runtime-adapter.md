# Runtime percentile adapter checkpoint

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
These are checkpoint results, not full compatibility evidence.

The PR remains a draft. Seeded RAND session integration, complete retained137-record
runtime replay, exact-head workspace/client/audit checks and final review/CI remain
required. ORDER payloads and completion/descriptor differences must be reported raw;
no known-difference normalization establishes a pass. NEXT VALUE FOR parsing remains
a separate frontend gap. Unproven carrier/declaration pairs remain explicit barriers.
