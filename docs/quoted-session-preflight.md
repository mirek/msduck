# Quoted names during scalar preflight

Scalar preflight classifies only unquoted expression identifiers as variable
references. `[@p]` and `"@p"` remain column references even when no scalar
`@p` exists. Column existence and declarations belong to later binding over
an explicit catalog; this pass must not fabricate scalar bindings for them.
Unquoted `@missing` retains the existing first-variable error and declaration
ordering rules. This change leaves variable declaration and procedure argument
syntax, owned DDL validation and session evaluation untouched.

The regression consumes every query from both identical 30-observation runs
in `reference/quoted-session-identifiers.json`, SHA-256
`8cf62bb9fc3962f9b6eeb5729f63210123cbd2f5c7dbfbe1e51700e548edb5e3`.
It covers quoted, qualified, parenthesized, derived, CTE and empty shapes,
bound 42/NULL and local variables. Missing quoted columns defer to column
binding while the captured unquoted missing variable still fails preflight.
Repeated checks preserve the input AST, parameter declarations and values;
local DECLARE initializers are not evaluated. A separate regression verifies
that quoted columns do not displace the first real missing-variable error.

Both tests failed against the pre-fix compiled SQL library: unbound `[@p]`
incorrectly produced the scalar error, including before a real `@missing`.
This is a deterministic compiler-stage correction. Root substitution and
descriptor propagation still need [runtime task #792](https://github.com/mirek/msduck/issues/792);
the 267 recorded wire differences are not declared fixed by this change.
Full current-head workspace/client/audit checks remain required before merge.
