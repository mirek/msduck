# CONCAT_WS and TRANSLATE core API

`msduck_sql::concat_ws` exposes the deterministic planner and evaluator merged
in PR #813. This registration changes API availability; it does not register a
DuckDB function or establish server compatibility. The earlier three-file task
report in [concat-ws-translate-core.md](concat-ws-translate-core.md) describes
the implementation before this separate export.

`plan` and `plan_with_context` accept a `Function`, original `Argument`
declarations, the default collation name and an explicit `Collation` catalog.
Conversion widths come from declarations, never current values. Literal NULL
has its own argument constructor. `plan_with_context` additionally accepts a
nonzero SELECT column position for diagnostics that require statement context.
The resulting `Plan` retains the result declaration, collation label, flags,
supplementary behavior and encoding. Planning does not evaluate expressions or
read a database, session, clock, environment or process-global state.

`evaluate` receives the plan and already SQL-formatted, converted values as
optional UTF-16 unit vectors. `None` represents SQL NULL; isolated surrogates
remain exact units. Original source declarations and declared storage bounds
still constrain these converted payloads. Adapters must preserve source
conversion errors, code pages, fixed padding and original types, and evaluate
each source expression once. They must not replace numeric or temporal sources
with fabricated character declarations to obtain convenient result metadata.

TRANSLATE equality comes from a stable caller-supplied character matcher.
`evaluate_with_keys` instead accepts established character-equivalence keys and
uses deterministic ordered lookup, retaining the first mapping without chaining.
Neither API supplies general linguistic comparison weights. Missing weights or
unsupported conversions remain explicit errors. Input/output, lookup and
comparison limits are resource barriers, not invented SQL Server diagnostics;
see the exported limit constants and the implementation report for their bounds.
ANSI UTF-8 evaluation remains unsupported even when its descriptor is known.

The raw SQL Server observations in [concat-ws-translate.md](concat-ws-translate.md),
[concat-text-conversion.md](concat-text-conversion.md),
[concat-boundary.md](concat-boundary.md) and
[concat-legacy-family.md](concat-legacy-family.md) remain the behavioral authority.
The module's existing tests preserve exact units, declarations and diagnostics
across the retained captures; local audit execution does not establish SQL Server
equivalence. Registration does not expand the evidence behind those rules.

Separate tasks still own conversion-helper exports and composition, AST binding
and logical result inference, catalog/session acquisition, native value adapters,
runtime diagnostic propagation and complete client/wire integration. Root
adapters must retain prepared and empty-result metadata independently of bound
values, and preserve error/completion token order. The numeric and temporal/GUID
formatters and collation codec work do not by themselves close those runtime gaps.
