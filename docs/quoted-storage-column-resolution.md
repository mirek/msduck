# Quoted columns through the actual operand resolver

The storage inference callback is backed by `aggregate_columns::resolve` in
production. Its column and source-column predicates now recognize delimited
at-sign names as columns. Join qualification and temporal annotation keep the
same distinction, while unquoted scalar parameters retain parameter binding.

Resolver-level regressions use explicit declarations derived from both retained
SQL Server captures. They cover INT column width four versus a conflicting
BIGINT parameter width eight, parameter 42/NULL, qualified and parenthesized
names, derived tables, CTEs and an empty-source predicate. Unknown, ambiguous
and shadowed local bindings stay unknown. Quoted counters retain their captured
character/SMALLINT declarations, and join operands acquire their row qualifier.

This companion addresses the verified review finding on PR #798; a substitute
column callback alone could not prove production binding. Snapshot and parameter
inputs remain unchanged. The resolver mutates only its caller-owned AST and
does not acquire catalogs or execute operands. Root runtime substitution and
the recorded wire differences remain separate work.
