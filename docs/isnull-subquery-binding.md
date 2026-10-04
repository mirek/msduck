# ISNULL scalar-query binding

The retained `concat isnull aggregate subquery` SQL Server case in
`reference/openjson-isnull.json` exceeded the unchanged 5000ms client deadline
on main19ba8b82. A native detailed profile localized5.29s to binding, with
about10ms of optimizer time and7ms of execution CPU. The first operand was
repeated in type-dispatch prototypes; its lowered Unicode aggregate already
contained several copies of the scalar query.

For a first operand containing a scalar query and only recognized deterministic
scalar adapters around it, the root lowering selects `__msduck_isnull_subquery`. Its local scalar query binds the first value
once and passes a column reference to the existing ISNULL dispatch. The
replacement stays inside the original COALESCE fallback. Logical result
metadata remains inferred from the original SQL; no result values or document
contents select its declaration. Both operands are checked: outer aggregate or
window calls retain the original dispatch, as do unknown or volatile calls even
inside an existing query. Aggregates inside their original scalar-query scope
remain eligible. This prevents moving a caller's SUM or window into the wrapper
or changing a volatile expression's correlation/cache boundary. Expressions
referencing either internal binding name retain the original path to avoid
capturing their identifiers.

Public native regressions cover outer SUM/window replacements, correlated
missing/NULL rows, multirow scalar errors, typed replacement conversion and
connection reuse. Native sequences through public lowering check three
per-row replacement evaluations, a skipped replacement and one uncorrelated
first-query evaluation. These are backend evaluation controls, not SQL Server
sequence syntax claims. A carrier result nested in Unicode concatenation and
comparison must retain exact isolated UTF-16 units. Width overloads preserve
carriers through raw-unit truncation/padding instead of lossy decoding.

An isolated candidate over the actual lowered query reduced native prepare
from about2600ms to292ms and returned the same values. These measurements are
native probes, not complete SQL Server compatibility proof. Final public
client replay, correlation/cardinality/evaluation controls and exact-head
workspace/client/audit checks are required before merge.

The additional public scope/correlation/conversion/isolated-carrier queries
were captured independently on SQL Server 2025 17.0.4065.4 using the repository's
pinned reference image. The capture confirms outer SUM/window results 6,
correlated rows (1,7)/(2,92)/(3,93), lazy invalid replacements, conversion 245,
scalar cardinality 512, and exact isolated UTF-16 values through concatenation
and comparison. Raw descriptors, errors, DONE and token captures are retained
with the verification evidence; backend sequence controls remain separate.
