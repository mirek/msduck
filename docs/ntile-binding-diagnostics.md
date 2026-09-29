# NTILE binding and diagnostic boundaries

PR #584 now combines the original NTILE NULL capture/native task with the
separately claimed engine supplement. The blocked historical engine task was
explicitly stopped by the protected concat-integration-v1 authorization; that
successor is merged and Done.

Before backend lowering or parameter substitution, the root validates only
captured invalid constants: literal NULL, INT/BIGINT NULL casts, and literal
zero/negative integer counts. They reject with 4116/state 1/class 15 before
metadata even when the source is empty. Validation does not evaluate any
expression or inspect parameter values. Parameters and scalar subqueries remain
runtime inputs and retain result metadata before native count errors.

The native validator checks every input row's validity and physical integer
layout, rejecting NULL rather than returning a NULL bucket count. The engine
recognizes only its canonical message, removing DuckDB's native envelope and
constructing the captured error identity. The canonical source-column diagnostic
is converted to 4195/state 1/class 15 with the captured double-quoted column.
An already structured application THROW is preserved, including its supplied
number/state/severity.

Native tests exercise empty and populated constant inputs, source-column errors,
prepared typed parameters rebound between NULL/zero/negative/valid counts,
scalar subqueries and explicit THROW. Public clients compare retained rows,
column type/width/flags and full diagnostic tuples directly, without substituting
local known-difference expectations for these cases.

The retained fixture preserves all ordered token events and raw DONE words.
These remain evidence rather than a full token-parity claim: the property
comparison does not cover ORDER tokens, complete INFO emission or command words.
Named-window, complex constant folding, correlated expressions, other source
types and complete compile/prepare precedence still need explicit verification.
