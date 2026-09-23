# MERGE validation

The server parses ordinary and CTE-prefixed MERGE statements with token-aware
validation before
executing any statement in the batch. A missing semicolon reports 10713, repeated
actions within a match family report 10714, and an arm following an unconditional
arm in the same family reports 5324. NOT MATCHED and NOT MATCHED BY TARGET belong
to the same family. Comments and semicolons inside strings are not terminators.

The checks also run inside supported IF, WHILE and BEGIN/TRY-CATCH blocks, and
during preparation. A failing batch cannot perform an earlier INSERT before the
parse error is discovered. Native parser tests and independent tedious tests
cover valid clause pairs, comments, nested blocks, repeated actions, preparation,
error number/severity/state, batch-write prevention, and subsequent connection
reuse. The copied corpus's missing-terminator case now exercises this path.

The upstream merge validator and tests supplied the clause-family test cases.
Two upstream details were not copied: its 10714 message reverses the action and
clause labels, and it assigns severity 16 to 5324. Microsoft's published error
catalog lists severity 15 for 10713, 10714 and 5324; the implementation uses that
catalog. Live SQL Server verification of messages, diagnostic precedence, state,
and line attribution remains outstanding. Messages currently retain the parser
prefix and line numbers remain approximate.

MERGE execution is still unsupported (40515), including when nested in a WITH
query body. The translator visits embedded MERGE statements so a query wrapper
cannot bypass that boundary in execution or preparation. A regression test
confirmed that this wrapper previously allowed an unterminated MERGE to execute.

TOP/hints, complete grammar/action diagnostics, predicate binding, target
conversion, multiple-match detection, atomic actions, OUTPUT, triggers, and
row counts remain unfinished. This validation work does not establish MERGE
execution compatibility.

Sources: Microsoft [MERGE](https://learn.microsoft.com/en-us/sql/t-sql/statements/merge-transact-sql),
[errors 10000–10999](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-10000-to-10999),
and [errors 5000–5999](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-5000-to-5999).
