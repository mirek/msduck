# Scoped CONCAT_WS and TRANSLATE compile binding

`projection::concat_functions` connects original AST operands and explicit
catalog/row scopes to the reviewed typed conversion composition. Its context
supplies collation properties and language; parameters supply declarations,
never bound values. It does not acquire a catalog/session, evaluate an operand,
read process state or lower a backend expression. Existing CONCAT and ordinary
projection inference remain unchanged.

`bind` produces a caller-owned plan for one function expression in its explicit
scope. `query` binds direct SELECT function outputs and validates functions
inside projected wrappers/scalar subqueries at their own scopes. `fields` is an
opt-in metadata path for direct function outputs and their CTE/derived/APPLY row
references. Without these functions it delegates to ordinary inference. Real
nonzero SELECT positions include wildcard expansion, so the captured 451
message identifies the original output position. Ordinary grouped row-reference
properties remain governed by the existing grouping rules.
FOR JSON keeps its ordinary single NVARCHAR(MAX) descriptor, canonical field
name, fragment flag and properties after underlying function validation; it is
not replaced by SELECT-list function fields.

Plans borrow the single original expression and operand nodes; metadata and
conversion rules are frozen separately. A later lowering acquires each original
operand once, then passes stored values to the conversion plan. The binder does
not generate casts, hidden plan IDs, private annotation names or cloned volatile
leaves. Rebinding identical inputs is idempotent and leaves the AST unchanged;
saved bindings reject changed expressions. Unsupported/duplicate function
modifiers are rejected rather than accepted as annotations. Binding scopes retain
qualified names, aliases, declared parameters, delimited parameter/counter-like
columns, nearest unknown/ambiguous shadows and explicit correlation boundaries.

Original numeric, binary, legacy and character source kinds remain distinct.
Literal NULL allocates zero; empty string literals allocate one. Typed NULL
retains its declared family/width, including MAX. Nonempty literals reuse the established storage
declaration rules, including ANSI >8000 and national >4000 promotion to MAX.
No function output is inferred from the current literal text instead of those
source declaration rules. CWS results are nonnullable
computed expressions; TRANSLATE results are nullable computed expressions as
captured. Derived row columns retain their separate derived origin. Computed
function results use the canonical result type identity, never an operand alias
ID. ResultType adaptation receives the validated character declaration without
values or transport types.

Scope/nesting depth is limited to 64 and projected expression traversal to 4096
nodes. Function arity remains the reviewed 3..254/3 contract. Unknown source,
collation, encoding or conversion properties remain explicit errors; known
function diagnostics retain their complete captured number/state/message.
Recursive CTEs and set members containing these functions remain unsupported in
this explicit path. Function metadata through other expression wrappers remains
ordinary inference or unknown; further adoption must preserve those distinctions
and obtain reference evidence before broadening ungrounded shapes.

The compile replay reads all four retained observations of #807, #815 and #833:
3,632 exact function result descriptors and 1,212 exact compile diagnostics.
Prepared executions compare the same declaration-only plan against each actual
binding descriptor; NULL/value rebinding never enters planning. The old #807 RPC
profile with an inferred argument length lacks emitted input TYPEINFO. Its four
observations remain explicitly UnknownOperand; the test never derives a width
from the bound string. Explicit driver declaration lengths above bounded caps
are represented as their SQL MAX declarations, independently of values.
The selected logical collation labels also match each captured finite
LCID/flags/version/sort-ID/code-page descriptor, including prepared rebindings.
Original observations that did not retain userType remain missing; retained
computed result userType values are checked without inventing old fields.

Non-ASCII bare ANSI literals require the captured CP1252 default source domain.
A UTF8 or missing/ambiguous default encoding remains UnknownContext even when a
Unicode companion or explicit COLLATE would otherwise promote the result. The
default-UTF8 native literal allocation requires new reference evidence; the
binder never substitutes UTF16 counts for its uncaptured native byte width.

The four isolated UTF-16 results in #807 use a lossless typed unit carrier only
because Rust JSON strings cannot contain isolated surrogates. Their exact units
are retained; no SQL, descriptor, error, row or fixture is repaired or dropped.
Raw fixture SHA-256 pins are:

| Fixture | SHA-256 |
| --- | --- |
| concat-ws-translate.json | 74cf73753ea383e260de98488cf7dde8cc2bb521128a9585366cb5ed9d54351f |
| concat-text-conversion.json | 4ef8ebf227b9dae4e1132196c6a04791d135df0fb69607eac886a0e6784fcc80 |
| concat-legacy-family.json | 636c51a2516b74ebb3dbb3a16e4478ac58a6d14bae33a832e18d69e31aef1711 |

The existing #842 composition replay separately verifies value conversion across
numeric, temporal/GUID, binary and legacy profiles. This binder adds scope and
metadata coverage rather than claiming new format/matching weights. TRANSLATE
length mismatch remains an evaluation diagnostic; malformed-variable matching
and uncaptured properties remain unknown. Root catalog acquisition, descriptor
adoption, native evaluation, NULL-separator behavior and wire/completion-token
support require separately scoped integration. Compiler tests are not runtime
or SQL Server compatibility completion.
