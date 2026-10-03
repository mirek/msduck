# Scoped CONCAT_WS and TRANSLATE compile binding

`projection::concat_functions` accepts the original AST, explicit catalog/row
scope and declaration-only parameters, plus explicit collation properties and
language. It reuses ordinary projection source/CTE/derived/APPLY resolution and
the reviewed typed conversion composition. It does not acquire catalog/session
state, inspect bound values, evaluate operands or lower backend expressions.

Plans borrow the single original AST and operand nodes. Original numeric,
binary, legacy and character declarations remain distinct; literal NULL allocates
zero and empty string literals allocate one, while typed NULL keeps its declared
width. Result declarations, collation labels and computed nullability are frozen
before execution. A saved binding rejects a changed original expression. No AST
annotation names, hidden IDs or repeated volatile evaluations are introduced.

The projected binding entry point supplies real nonzero SELECT positions,
including wildcard expansion, for captured collation diagnostics. Unknown
catalog/type/encoding/conversion properties remain explicit errors. Ordinary
projection inference and existing CONCAT behavior remain unchanged: effectful
root adoption and metadata/physical lowering are separate tasks.

Implementation checkpoint: broader fixture replay and scope/diagnostic proofs
are in progress. Compiler tests do not establish runtime or wire compatibility.
